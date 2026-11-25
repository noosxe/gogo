//! Asset checks (design 27 §1 and §8, S10, S11): what a PNG, an SVG and a font file must
//! look like before the skin may carry it.
//!
//! The checks read the bytes' structure and nothing more: a PNG's chunk list, an SVG's
//! tags and attributes, a font's table directory. Decoding is the renderer's. An SVG is
//! accepted only if it is static and refers to nothing outside itself.

use std::sync::Arc;

/// The kind of an asset, from its extension, checked against its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Png,
    Svg,
    /// An OTF or TTF.
    Font,
}

impl AssetKind {
    /// The kind a file name's extension stands for (case-insensitive).
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
        match ext.as_str() {
            "png" => Some(Self::Png),
            "svg" => Some(Self::Svg),
            "otf" | "ttf" => Some(Self::Font),
            _ => None,
        }
    }

    /// The size limit of this kind (design 27 §8).
    pub const fn max_bytes(self) -> usize {
        match self {
            Self::Png => crate::limits::MAX_PNG_BYTES,
            Self::Svg => crate::limits::MAX_SVG_BYTES,
            Self::Font => crate::limits::MAX_FONT_BYTES,
        }
    }
}

/// A checked asset file.
#[derive(Debug, Clone)]
pub struct Asset {
    pub kind: AssetKind,
    pub bytes: Arc<[u8]>,
    /// A PNG's size in pixels, from its header.
    pub size: Option<(u32, u32)>,
}

/// Why an asset was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssetError {
    #[error("not a PNG: {0}")]
    Png(&'static str),
    #[error("not an OTF or TTF font: {0}")]
    Font(&'static str),
    #[error("not a usable SVG: {0}")]
    SvgMalformed(&'static str),
    #[error("SVG contains <{0}>, which is not allowed (a skin's SVG is static, S10)")]
    SvgElement(String),
    #[error("SVG has the event attribute `{0}` (S10)")]
    SvgEvent(String),
    #[error("SVG refers outside itself with `{0}`: only `#fragment` references are allowed (S11)")]
    SvgReference(String),
    #[error("SVG uses {0}, which is not allowed (S10)")]
    SvgSyntax(&'static str),
}

/// Check bytes against `kind`.
pub(crate) fn check(kind: AssetKind, bytes: &[u8]) -> Result<Option<(u32, u32)>, AssetError> {
    match kind {
        AssetKind::Png => check_png(bytes).map(Some),
        AssetKind::Font => check_font(bytes).map(|()| None),
        AssetKind::Svg => check_svg(bytes).map(|()| None),
    }
}

/// The longest side of a PNG, in pixels (design 27 §8).
const MAX_PNG_SIDE: u32 = 16_384;
/// Width times height of a PNG: 32 Mpx (design 27 §8).
const MAX_PNG_PIXELS: u64 = 33_554_432;

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The magic bytes, an `IHDR` first, a chunk list that ends exactly at `IEND`.
fn check_png(b: &[u8]) -> Result<(u32, u32), AssetError> {
    if !b.starts_with(PNG_MAGIC) {
        return Err(AssetError::Png("the signature is wrong"));
    }
    let mut at = PNG_MAGIC.len();
    let mut size = None;
    loop {
        let (Some(len), Some(ty)) = (be32(b, at), b.get(at + 4..at + 8)) else {
            return Err(AssetError::Png("it ends before IEND"));
        };
        let len = len as usize;
        if size.is_none() {
            if ty != b"IHDR" || len != 13 {
                return Err(AssetError::Png("the first chunk is not IHDR"));
            }
            let (Some(w), Some(h)) = (be32(b, at + 8), be32(b, at + 12)) else {
                return Err(AssetError::Png("the header is cut short"));
            };
            if w == 0 || h == 0 {
                return Err(AssetError::Png("the image has no pixels"));
            }
            if w > MAX_PNG_SIDE || h > MAX_PNG_SIDE {
                return Err(AssetError::Png("a side is longer than 16384 pixels"));
            }
            if u64::from(w) * u64::from(h) > MAX_PNG_PIXELS {
                return Err(AssetError::Png("the image has more than 32 million pixels"));
            }
            size = Some((w, h));
        }
        let Some(next) = at
            .checked_add(12)
            .and_then(|n| n.checked_add(len))
            .filter(|n| *n <= b.len())
        else {
            return Err(AssetError::Png("a chunk runs past the end"));
        };
        at = next;
        if ty == b"IEND" {
            break;
        }
    }
    if at != b.len() {
        return Err(AssetError::Png("there is data after IEND"));
    }
    size.ok_or(AssetError::Png("it has no header"))
}

/// An sfnt header: TrueType (`0x00010000`, `true`) or OpenType CFF (`OTTO`), with a table
/// directory that fits. Font collections are not accepted.
fn check_font(b: &[u8]) -> Result<(), AssetError> {
    let Some(tag) = b.get(..4) else {
        return Err(AssetError::Font("it is too short"));
    };
    if tag != [0, 1, 0, 0] && tag != b"true" && tag != b"OTTO" {
        return Err(AssetError::Font("the header is not TrueType or OpenType"));
    }
    let tables = b
        .get(4..6)
        .map(|n| usize::from(u16::from_be_bytes([n[0], n[1]])))
        .unwrap_or(0);
    if tables == 0 || 12 + 16 * tables > b.len() {
        return Err(AssetError::Font("the table directory does not fit"));
    }
    Ok(())
}

/// Elements an SVG may not have: scripts, anything that embeds another document, and
/// animation (a skin's SVG is a still picture).
const SVG_FORBIDDEN: &[&str] = &[
    "script",
    "foreignobject",
    "iframe",
    "embed",
    "object",
    "audio",
    "video",
    "canvas",
    "animate",
    "animatetransform",
    "animatemotion",
    "animatecolor",
    "set",
    "discard",
    "handler",
    "listener",
];

fn check_svg(bytes: &[u8]) -> Result<(), AssetError> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(AssetError::SvgMalformed("it is not UTF-8"));
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut at = 0;
    let mut root_seen = false;
    let mut in_style = false;
    let malformed = AssetError::SvgMalformed;
    while let Some(lt) = text[at..].find('<').map(|i| at + i) {
        let between = &text[at..lt];
        if in_style {
            check_css(between)?;
        }
        let rest = &text[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            let end = r.find("-->").ok_or(malformed("a comment is not closed"))?;
            at = lt + 4 + end + 3;
        } else if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let end = r.find("]]>").ok_or(malformed("a CDATA is not closed"))?;
            if in_style {
                check_css(&r[..end])?;
            }
            at = lt + 9 + end + 3;
        } else if let Some(r) = rest.strip_prefix("<?") {
            let end = r
                .find("?>")
                .ok_or(malformed("a declaration is not closed"))?;
            let target = r.split(|c: char| c.is_whitespace() || c == '?').next();
            if target != Some("xml") {
                return Err(AssetError::SvgSyntax("a processing instruction"));
            }
            at = lt + 2 + end + 2;
        } else if rest.starts_with("<!") {
            return Err(AssetError::SvgSyntax("a DOCTYPE or entity declaration"));
        } else if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').ok_or(malformed("an end tag is not closed"))?;
            if local_name(r[..end].trim()).eq_ignore_ascii_case("style") {
                in_style = false;
            }
            at = lt + 2 + end + 1;
        } else {
            let (name, end, self_closing) = start_tag(&rest[1..])?;
            let local = local_name(name).to_ascii_lowercase();
            if !root_seen {
                if local != "svg" {
                    return Err(malformed("the root element is not <svg>"));
                }
                root_seen = true;
            }
            if SVG_FORBIDDEN.contains(&local.as_str()) {
                return Err(AssetError::SvgElement(local));
            }
            if local == "style" && !self_closing {
                in_style = true;
            }
            at = lt + 1 + end;
        }
    }
    if root_seen {
        Ok(())
    } else {
        Err(malformed("there is no <svg> element"))
    }
}

/// The name without a namespace prefix: `svg:script` → `script`, `xlink:href` → `href`.
fn local_name(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, l)| l)
}

/// Parse one start tag after its `<`: its name, the byte after its `>`, and whether it
/// closes itself. Checks every attribute on the way.
fn start_tag(s: &str) -> Result<(&str, usize, bool), AssetError> {
    let bad = AssetError::SvgMalformed;
    let b = s.as_bytes();
    let name_end = s
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .ok_or(bad("a tag is not closed"))?;
    let name = &s[..name_end];
    if name.is_empty() {
        return Err(bad("a tag has no name"));
    }
    let mut i = name_end;
    loop {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        match b.get(i) {
            None => return Err(bad("a tag is not closed")),
            Some(b'>') => return Ok((name, i + 1, false)),
            Some(b'/') if b.get(i + 1) == Some(&b'>') => return Ok((name, i + 2, true)),
            Some(_) => {}
        }
        let attr_end = s[i..]
            .find(|c: char| c.is_whitespace() || c == '=' || c == '>' || c == '/')
            .map(|n| i + n)
            .ok_or(bad("a tag is not closed"))?;
        let attr = &s[i..attr_end];
        i = attr_end;
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if b.get(i) != Some(&b'=') || attr.is_empty() {
            return Err(bad("an attribute has no value"));
        }
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let Some(&q) = b.get(i).filter(|q| **q == b'"' || **q == b'\'') else {
            return Err(bad("an attribute value is not quoted"));
        };
        let vstart = i + 1;
        let vend = s[vstart..]
            .find(q as char)
            .map(|n| vstart + n)
            .ok_or(bad("an attribute value is not closed"))?;
        check_attr(attr, &s[vstart..vend])?;
        i = vend + 1;
    }
}

fn check_attr(name: &str, value: &str) -> Result<(), AssetError> {
    let lname = name.to_ascii_lowercase();
    let local = local_name(&lname);
    // Namespace declarations name a namespace, not a resource.
    if lname == "xmlns" || lname.starts_with("xmlns:") {
        return Ok(());
    }
    if local.len() > 2 && local.starts_with("on") {
        return Err(AssetError::SvgEvent(name.to_owned()));
    }
    if matches!(local, "href" | "src" | "base" | "action" | "formaction")
        && !value.trim_start().starts_with('#')
    {
        return Err(AssetError::SvgReference(format!("{name}=\"{value}\"")));
    }
    if value.contains("&#") {
        return Err(AssetError::SvgSyntax(
            "a numeric character reference in a value",
        ));
    }
    let squashed: String = value
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if squashed.contains("javascript:") || squashed.contains("data:") {
        return Err(AssetError::SvgReference(format!("{name}=\"{value}\"")));
    }
    if local == "style" {
        check_css(value)?;
    } else {
        check_urls(value)?;
    }
    Ok(())
}

/// Stylesheet text: no `@import`, no escapes, `url()` only to a `#fragment`.
fn check_css(css: &str) -> Result<(), AssetError> {
    let lower = css.to_ascii_lowercase();
    if lower.contains("@import") {
        return Err(AssetError::SvgReference("@import".to_owned()));
    }
    if css.contains("&#") {
        return Err(AssetError::SvgSyntax("a character reference in a style"));
    }
    if css.contains('\\') {
        return Err(AssetError::SvgSyntax("a backslash escape in a style"));
    }
    if lower.contains("expression(")
        || lower.contains("-moz-binding")
        || lower.contains("behavior:")
    {
        return Err(AssetError::SvgSyntax("a scripting style value"));
    }
    check_urls(css)
}

/// Every `url(…)` in `s` must point at a `#fragment`.
fn check_urls(s: &str) -> Result<(), AssetError> {
    let lower = s.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("url(") {
        let after = from + i + 4;
        let target = lower[after..].trim_start_matches([' ', '\t', '"', '\'']);
        if !target.starts_with('#') {
            let shown: String = s[after..].chars().take(40).collect();
            return Err(AssetError::SvgReference(format!("url({shown}")));
        }
        from = after;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(extra_after_iend: &[u8]) -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        v.extend([0, 0, 0, 13]);
        v.extend(b"IHDR");
        v.extend([0, 0, 0, 2, 0, 0, 0, 3, 8, 6, 0, 0, 0]);
        v.extend([0; 4]);
        v.extend([0, 0, 0, 0]);
        v.extend(b"IEND");
        v.extend([0; 4]);
        v.extend(extra_after_iend);
        v
    }

    #[test]
    fn png_structure() {
        assert_eq!(check_png(&png(&[])), Ok((2, 3)));
        assert!(check_png(&png(&[1])).is_err());
        assert!(check_png(b"GIF89a").is_err());
        assert!(check_png(&png(&[])[..30]).is_err());
    }

    #[test]
    fn png_size_caps() {
        let with = |w: u32, h: u32| {
            let mut v = png(&[]);
            v[16..20].copy_from_slice(&w.to_be_bytes());
            v[20..24].copy_from_slice(&h.to_be_bytes());
            v
        };
        assert_eq!(check_png(&with(16_384, 2_048)), Ok((16_384, 2_048)));
        assert!(check_png(&with(16_385, 1)).is_err());
        assert!(check_png(&with(1, 16_385)).is_err());
        assert!(check_png(&with(16_384, 2_049)).is_err());
    }

    #[test]
    fn font_headers() {
        let mut f = b"OTTO\0\x01\0\0\0\0\0\0".to_vec();
        f.extend([0; 16]);
        assert!(check_font(&f).is_ok());
        assert!(check_font(b"wOFF\0\x01\0\0\0\0\0\0").is_err());
        assert!(check_font(b"OTTO").is_err());
    }

    fn svg(body: &str) -> Result<(), AssetError> {
        check_svg(
            format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\">{body}</svg>")
                .as_bytes(),
        )
    }

    #[test]
    fn static_svg_passes() {
        assert!(svg("<path d=\"M0 0L1 1\" fill=\"url(#g)\"/><use href=\"#a\"/>").is_ok());
        assert!(svg("<style>.a{fill:url(#g)}</style><rect class=\"a\"/>").is_ok());
        assert!(svg("<!-- c --><g><title>x</title></g>").is_ok());
    }

    #[test]
    fn unsafe_svg_fails() {
        for body in [
            "<script>alert(1)</script>",
            "<svg:script xmlns:svg=\"x\"/>",
            "<rect onload=\"x()\"/>",
            "<image href=\"https://e.example/a.png\"/>",
            "<use xlink:href=\"other.svg#a\"/>",
            "<rect fill=\"url(https://e.example/x)\"/>",
            "<rect style=\"fill:url(a.svg#b)\"/>",
            "<style>@import 'x.css';</style>",
            "<a href=\"javascript:alert(1)\"/>",
            "<animate attributeName=\"x\"/>",
            "<foreignObject/>",
            "<rect x=\"&#106;\"/>",
            "<style>.a{fill:&#117;rl(a)}</style>",
            "<style><![CDATA[.a{fill:&#117;rl(a)}]]></style>",
        ] {
            assert!(svg(body).is_err(), "{body}");
        }
        assert!(check_svg(b"<!DOCTYPE svg><svg/>").is_err());
        assert!(check_svg(b"<html/>").is_err());
        assert!(check_svg(b"<?xml-stylesheet href=\"a.css\"?><svg/>").is_err());
    }
}

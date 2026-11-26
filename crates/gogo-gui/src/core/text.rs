//! Server text made fit for a skin: plain, bounded, and without addresses (design 26
//! §3.7).

/// The longest description a skin gets, in characters.
const DESCRIPTION_MAX: usize = 4000;

/// The longest single-line text (a title, a name), in characters.
const LINE_MAX: usize = 200;

/// One line of server text: control characters become spaces, and it is cut at a
/// bound.
pub(crate) fn line(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(LINE_MAX)
        .collect();
    flat.trim().to_string()
}

/// The store's HTML description as paragraphs of plain text. Tags go, with their
/// attributes (so no link survives as an address); `script` and `style` go with their
/// content; the common entities are read; words that are web addresses are dropped.
pub(crate) fn description(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        rest = &rest[open + 1..];
        let Some(close) = rest.find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[..close];
        rest = &rest[close + 1..];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        match name.as_str() {
            "script" | "style" if !tag.starts_with('/') => {
                let end = format!("</{name}");
                let lower = rest.to_ascii_lowercase();
                rest = match lower.find(&end) {
                    Some(at) => &rest[at..],
                    None => "",
                };
            }
            "br" | "p" | "div" | "li" | "ul" | "ol" | "h1" | "h2" | "h3" | "h4" => out.push('\n'),
            _ => out.push(' '),
        }
    }
    out.push_str(rest);
    let text = unescape(&out);

    let mut paragraphs = Vec::new();
    for raw in text.split('\n') {
        let words: Vec<&str> = raw
            .split(|c: char| c.is_whitespace() || c.is_control())
            .filter(|w| !w.is_empty() && !w.contains("://") && !w.starts_with("www."))
            .collect();
        if !words.is_empty() {
            paragraphs.push(words.join(" "));
        }
    }
    paragraphs
        .join("\n")
        .chars()
        .take(DESCRIPTION_MAX)
        .collect()
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // An entity is short; a lone `&` stays.
        let end = rest.bytes().take(12).position(|b| b == b';');
        let decoded = end.and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => ' ',
                "mdash" => '—',
                "ndash" => '–',
                "hellip" => '…',
                "rsquo" => '’',
                "lsquo" => '‘',
                "rdquo" => '”',
                "ldquo" => '“',
                "trade" => '™',
                "reg" => '®',
                "copy" => '©',
                _ => {
                    let number = name.strip_prefix('#')?;
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => number.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_paragraphs_without_addresses() {
        let html = r#"<div style="x"><a href="https://example.com/a?token=t"><img src="https://example.com/i.png"></a>
            <p>One &amp; two&#33;</p><script>alert(1)</script><ul><li>Three</li><li>see https://example.com/x</li></ul></div>"#;
        assert_eq!(description(html), "One & two!\nThree\nsee");
    }

    #[test]
    fn broken_markup_and_long_text_are_bounded() {
        assert_eq!(description("a <b"), "a");
        assert_eq!(description("a <script> b"), "a");
        assert_eq!(description("& &x; &#999999999;"), "& &x; &#999999999;");
        assert_eq!(description(&"é ".repeat(10_000)).chars().count(), 4000);
        assert_eq!(line(&format!("a\nb{}", "c".repeat(500))).len(), 200);
        // A multi-byte character inside the entity window must not split.
        assert_eq!(description("&éééééééé;"), "&éééééééé;");
    }
}

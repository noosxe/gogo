//! Tokens, `tokens.kdl` (design 27 §3, design 26 §6.1).
//!
//! One `tokens "<mode>" { … }` block per mode the manifest lists. A token is a colour
//! (`"#rrggbb"` or `"#rrggbbaa"`), a number, a font (`family=`, `size=`, `weight=`, and an
//! optional `stretch=`), or an `@ref` to another token of the same mode. References are
//! resolved here, with cycle detection, so a loaded token always holds its final value.
//!
//! Every mode defines the same names. The contract's semantic tokens are required, except
//! the two status tones that have a fallback (`tone.queued` → `text-lo`,
//! `tone.paused` → `warn`).

use std::collections::{BTreeMap, BTreeSet};

use gogo_contract::{Status, tokens as contract};

use crate::diag::{Code, Collector};
use crate::fonts;
use crate::manifest::Mode;
use crate::names::is_token_name;
use crate::raw::{Lit, RawNode};
use crate::source::Span;

/// A colour, 8 bits per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// `#rrggbb` or `#rrggbbaa`.
    pub fn parse(s: &str) -> Option<Rgba> {
        let h = s.strip_prefix('#')?;
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) || !matches!(h.len(), 6 | 8) {
            return None;
        }
        let at = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Rgba {
            r: at(0)?,
            g: at(2)?,
            b: at(4)?,
            a: if h.len() == 8 { at(6)? } else { 255 },
        })
    }
}

/// A type role's value.
#[derive(Debug, Clone, PartialEq)]
pub struct FontToken {
    pub family: String,
    pub size: f64,
    pub weight: f64,
    /// The `wdth` axis, 100 is normal. Only for a variable family.
    pub stretch: Option<f64>,
}

/// What a token holds.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenValue {
    Colour(Rgba),
    Number(f64),
    Font(FontToken),
    /// The value of an `easing.*` token: a curve name the motion code reads.
    Text(String),
}

impl TokenValue {
    pub fn kind(&self) -> TokenKind {
        match self {
            TokenValue::Colour(_) => TokenKind::Colour,
            TokenValue::Number(_) => TokenKind::Number,
            TokenValue::Font(_) => TokenKind::Font,
            TokenValue::Text(_) => TokenKind::Text,
        }
    }
}

/// The type of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Colour,
    Number,
    Font,
    Text,
}

impl TokenKind {
    pub(crate) const fn noun(self) -> &'static str {
        match self {
            TokenKind::Colour => "a colour",
            TokenKind::Number => "a number",
            TokenKind::Font => "a font",
            TokenKind::Text => "text",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub value: TokenValue,
    pub span: Span,
}

/// The tokens of one mode, references resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TokenSet {
    tokens: BTreeMap<String, Token>,
    /// Names the file declares that did not make it into `tokens` (a bad value, a bad
    /// reference): reported where they are, so not reported again as missing.
    declared: BTreeSet<String>,
}

impl TokenSet {
    pub fn get(&self, name: &str) -> Option<&Token> {
        self.tokens.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Token)> {
        self.tokens.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// The type of `@name`: a token of the set, a field of a type role
    /// (`@font.body.size`), or a status tone that has a fallback.
    pub fn kind_of(&self, name: &str) -> Option<TokenKind> {
        if let Some(t) = self.tokens.get(name) {
            return Some(t.value.kind());
        }
        if let Some((role, field)) = name.rsplit_once('.')
            && matches!(
                self.tokens.get(role).map(|t| &t.value),
                Some(TokenValue::Font(_))
            )
        {
            return match field {
                "family" => Some(TokenKind::Text),
                "size" | "weight" => Some(TokenKind::Number),
                _ => None,
            };
        }
        optional_tones()
            .any(|t| t == name)
            .then_some(TokenKind::Colour)
    }
}

/// The status tones a skin may leave out.
fn optional_tones() -> impl Iterator<Item = &'static str> {
    Status::ALL
        .iter()
        .filter(|s| s.tone_fallback().is_some())
        .map(|s| s.tone())
}

/// The tokens every skin defines in every mode.
pub fn required() -> Vec<&'static str> {
    let optional: Vec<&str> = optional_tones().collect();
    contract::COLOURS
        .iter()
        .filter(|t| !optional.contains(t))
        .chain(contract::FONT_ROLES)
        .chain(contract::SHAPE)
        .chain(contract::MOTION)
        .copied()
        .collect()
}

/// The type the contract fixes for a semantic name, if it has one.
fn expected_kind(name: &str) -> Option<TokenKind> {
    if contract::COLOURS.contains(&name) {
        Some(TokenKind::Colour)
    } else if contract::FONT_ROLES.contains(&name) {
        Some(TokenKind::Font)
    } else if contract::SHAPE.contains(&name) || contract::MOTION.contains(&name) {
        Some(TokenKind::Number)
    } else if name.starts_with(contract::EASING_PREFIX) && contract::is_semantic(name) {
        Some(TokenKind::Text)
    } else {
        None
    }
}

/// A declaration before references are followed.
enum Decl {
    Value(TokenValue),
    Ref(String, Span),
}

/// Parse `tokens.kdl`. Returns the set of every mode that has a block, even a faulty one;
/// the caller looks at the findings.
pub(crate) fn parse(
    col: &mut Collector,
    nodes: &[RawNode],
    file: usize,
    modes: &[Mode],
) -> BTreeMap<Mode, TokenSet> {
    let mut out = BTreeMap::new();
    let mut spans = BTreeMap::new();
    for n in nodes {
        if n.name != "tokens" {
            col.error(
                Code::TokenStructure,
                n.name_span,
                format!(
                    "tokens.kdl holds `tokens \"<mode>\"` blocks, found `{}`",
                    n.name
                ),
            );
            continue;
        }
        let Some(mode) = n
            .str_arg(0)
            .and_then(Mode::from_name)
            .filter(|_| n.args.len() == 1)
        else {
            col.error(
                Code::TokenStructure,
                n.span,
                "`tokens` takes one mode: \"dark\" or \"light\"",
            );
            continue;
        };
        if !modes.contains(&mode) {
            col.error(
                Code::TokenMode,
                n.span,
                format!("the manifest does not list the `{mode}` mode"),
            )
            .help("add it to `modes` in skin.kdl, or remove the block");
            continue;
        }
        if out.contains_key(&mode) {
            col.error(
                Code::TokenStructure,
                n.span,
                format!("`{mode}` is defined twice"),
            );
            continue;
        }
        if !n.props.is_empty() {
            col.error(Code::TokenStructure, n.span, "`tokens` takes no properties");
        }
        out.insert(mode, block(col, n));
        spans.insert(mode, n.span);
    }
    for m in modes {
        if !out.contains_key(m) {
            col.error(
                Code::TokenStructure,
                Span::new(file, 0, 0),
                format!("the manifest lists the `{m}` mode and tokens.kdl has no `tokens \"{m}\"`"),
            );
        }
    }
    check_modes(col, &out, &spans);
    out
}

fn block(col: &mut Collector, n: &RawNode) -> TokenSet {
    let mut decls: BTreeMap<String, (Decl, Span)> = BTreeMap::new();
    let mut declared: BTreeSet<String> = BTreeSet::new();
    for t in &n.children {
        if !t.children.is_empty() {
            col.error(Code::TokenStructure, t.span, "a token has no children");
            continue;
        }
        if !is_token_name(&t.name) {
            col.error(
                Code::TokenName,
                t.name_span,
                format!("`{}` is not a token name", t.name),
            )
            .help("use lowercase letters, digits, `-` and `_`, with `.` between parts");
            continue;
        }
        if let Some((role, field)) = t.name.rsplit_once('.')
            && contract::FONT_ROLES.contains(&role)
            && contract::FONT_FIELDS.contains(&field)
        {
            col.error(
                Code::TokenName,
                t.name_span,
                format!(
                    "`{}` is a field of `{role}`, not a token of its own",
                    t.name
                ),
            )
            .help(format!("set it on `{role}` with `{field}=`"));
            continue;
        }
        if decls.contains_key(&t.name) {
            col.error(
                Code::TokenName,
                t.name_span,
                format!("token `{}` is defined twice", t.name),
            );
            continue;
        }
        declared.insert(t.name.clone());
        if let Some(d) = decl(col, t) {
            decls.insert(t.name.clone(), (d, t.span));
        }
    }
    let mut set = resolve(col, decls, &declared);
    set.declared = declared;
    set
}

fn decl(col: &mut Collector, t: &RawNode) -> Option<Decl> {
    // A type role: `font.body family="Noto Sans" size=14 weight=400`.
    if t.args.is_empty() && !t.props.is_empty() {
        return font(col, t).map(|f| Decl::Value(TokenValue::Font(f)));
    }
    let [a] = t.args.as_slice() else {
        col.error(
            Code::TokenValue,
            t.span,
            format!("token `{}` takes one value", t.name),
        )
        .help("a colour \"#rrggbb\", a number, an \"@ref\", or a font with family=, size= and weight=");
        return None;
    };
    if !t.props.is_empty() {
        col.error(
            Code::TokenValue,
            t.span,
            "a token is a value or a font, not both",
        );
        return None;
    }
    match &a.lit {
        Lit::Num(n) if n.is_finite() => Some(Decl::Value(TokenValue::Number(*n))),
        Lit::Str(s) => {
            if let Some(target) = s.strip_prefix('@') {
                if is_token_name(target) {
                    return Some(Decl::Ref(target.to_owned(), a.span));
                }
                col.error(
                    Code::TokenRef,
                    a.span,
                    format!("`{s}` is not a token reference"),
                );
                return None;
            }
            if expected_kind(&t.name) == Some(TokenKind::Text) {
                return Some(Decl::Value(TokenValue::Text(s.clone())));
            }
            match Rgba::parse(s) {
                Some(c) => Some(Decl::Value(TokenValue::Colour(c))),
                None => {
                    col.error(Code::TokenValue, a.span, format!("`{s}` is not a colour"))
                        .help("colours are \"#rrggbb\" or \"#rrggbbaa\"");
                    None
                }
            }
        }
        other => {
            col.error(
                Code::TokenValue,
                a.span,
                format!(
                    "a token is a colour, a number or an `@ref`, found {}",
                    other.kind()
                ),
            );
            None
        }
    }
}

fn font(col: &mut Collector, t: &RawNode) -> Option<FontToken> {
    let (mut family, mut size, mut weight, mut stretch) = (None, None, None, None);
    let mut ok = true;
    for p in &t.props {
        let num = |col: &mut Collector, ok: &mut bool, lo: f64, hi: f64| match p.value.lit {
            Lit::Num(n) if n.is_finite() && n >= lo && n <= hi => Some(n),
            _ => {
                *ok = false;
                col.error(
                    Code::TokenValue,
                    p.value.span,
                    format!("`{}` is a number from {lo} to {hi}", p.name),
                );
                None
            }
        };
        match p.name.as_str() {
            "family" => match &p.value.lit {
                Lit::Str(s) => {
                    if fonts::family(s).is_none() {
                        ok = false;
                        col.error(
                            Code::FontFamily,
                            p.value.span,
                            format!("font family `{s}` is not bundled with the app"),
                        )
                        .help(format!("the bundled families are {}", fonts::names()));
                    }
                    family = Some(s.clone());
                }
                _ => {
                    ok = false;
                    col.error(Code::TokenValue, p.value.span, "`family` is a string");
                }
            },
            "size" => size = num(col, &mut ok, 1.0, 1024.0),
            "weight" => weight = num(col, &mut ok, 1.0, 1000.0),
            "stretch" => stretch = num(col, &mut ok, 25.0, 200.0).map(|n| (n, p.value.span)),
            other => {
                ok = false;
                col.error(
                    Code::TokenValue,
                    p.name_span,
                    format!("a font has `family`, `size`, `weight` and `stretch`, not `{other}`"),
                );
            }
        }
    }
    for (name, missing) in [
        ("family", family.is_none()),
        ("size", size.is_none()),
        ("weight", weight.is_none()),
    ] {
        if missing && ok {
            ok = false;
            col.error(
                Code::TokenValue,
                t.span,
                format!("font `{}` has no `{name}=`", t.name),
            );
        }
    }
    let family = family?;
    if let (Some((_, sp)), Some(f)) = (stretch, fonts::family(&family))
        && !f.variable
    {
        col.error(
            Code::FontStretch,
            sp,
            format!("`stretch` needs a variable font; `{family}` is static"),
        )
        .help("`stretch` is the `wdth` axis: use it with `Noto Sans`");
        ok = false;
    }
    ok.then(|| FontToken {
        family,
        size: size.unwrap_or(0.0),
        weight: weight.unwrap_or(0.0),
        stretch: stretch.map(|s| s.0),
    })
}

/// Follow every reference to its final value; report unknown targets and cycles.
fn resolve(
    col: &mut Collector,
    decls: BTreeMap<String, (Decl, Span)>,
    declared: &BTreeSet<String>,
) -> TokenSet {
    let mut done: BTreeMap<String, Token> = BTreeMap::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    for name in decls.keys() {
        let mut chain = Vec::new();
        follow(
            col,
            &decls,
            declared,
            name,
            &mut chain,
            &mut done,
            &mut failed,
        );
    }
    // The contract fixes the type of its own tokens.
    let mut tokens = BTreeMap::new();
    for (name, tok) in done {
        if let Some(want) = expected_kind(&name)
            && tok.value.kind() != want
        {
            col.error(
                Code::TokenValue,
                tok.span,
                format!(
                    "`{name}` is {}, found {}",
                    want.noun(),
                    tok.value.kind().noun()
                ),
            );
            continue;
        }
        tokens.insert(name, tok);
    }
    TokenSet {
        tokens,
        declared: BTreeSet::new(),
    }
}

fn follow(
    col: &mut Collector,
    decls: &BTreeMap<String, (Decl, Span)>,
    declared: &BTreeSet<String>,
    name: &str,
    chain: &mut Vec<String>,
    done: &mut BTreeMap<String, Token>,
    failed: &mut BTreeSet<String>,
) -> Option<TokenValue> {
    if let Some(t) = done.get(name) {
        return Some(t.value.clone());
    }
    if failed.contains(name) {
        return None;
    }
    let (decl, span) = decls.get(name)?;
    let value = match decl {
        Decl::Value(v) => Some(v.clone()),
        Decl::Ref(target, at) => {
            if chain.iter().any(|c| c == name) {
                let mut cycle = chain.clone();
                cycle.push(name.to_owned());
                col.error(
                    Code::TokenRef,
                    *at,
                    format!("token references form a cycle: {}", cycle.join(" -> ")),
                )
                .help("a token must end in a value");
                None
            } else {
                chain.push(name.to_owned());
                let v = if decls.contains_key(target) {
                    follow(col, decls, declared, target, chain, done, failed)
                } else if let Some(v) = derived(decls, target) {
                    Some(v)
                } else if declared.contains(target)
                    || target
                        .rsplit_once('.')
                        .is_some_and(|(role, _)| declared.contains(role))
                {
                    // Declared and broken: reported where it is declared.
                    None
                } else {
                    let names = decls.keys().map(String::as_str);
                    let help = match crate::names::suggest(target, names) {
                        Some(s) => format!("did you mean `@{s}`?"),
                        None => "a reference names a token of the same mode".to_owned(),
                    };
                    col.error(
                        Code::TokenRef,
                        *at,
                        format!("`@{target}` is not defined in this mode"),
                    )
                    .help(help);
                    None
                };
                chain.pop();
                v
            }
        }
    };
    match value {
        Some(v) => {
            done.insert(
                name.to_owned(),
                Token {
                    value: v.clone(),
                    span: *span,
                },
            );
            Some(v)
        }
        None => {
            failed.insert(name.to_owned());
            None
        }
    }
}

/// `@font.body.size`: a field of a font role.
fn derived(decls: &BTreeMap<String, (Decl, Span)>, target: &str) -> Option<TokenValue> {
    let (role, field) = target.rsplit_once('.')?;
    let (Decl::Value(TokenValue::Font(f)), _) = decls.get(role)? else {
        return None;
    };
    match field {
        "family" => Some(TokenValue::Text(f.family.clone())),
        "size" => Some(TokenValue::Number(f.size)),
        "weight" => Some(TokenValue::Number(f.weight)),
        _ => None,
    }
}

/// Every mode defines the same names, with the same types, and the required ones.
fn check_modes(col: &mut Collector, sets: &BTreeMap<Mode, TokenSet>, spans: &BTreeMap<Mode, Span>) {
    let required = required();
    for (mode, set) in sets {
        let Some(span) = spans.get(mode).copied() else {
            continue;
        };
        let missing: Vec<&str> = required
            .iter()
            .copied()
            .filter(|r| !set.tokens.contains_key(*r) && !set.declared.contains(*r))
            .collect();
        if !missing.is_empty() {
            col.error(
                Code::TokenMissing,
                span,
                format!("`{mode}` lacks required tokens: {}", missing.join(", ")),
            )
            .help("design 26 §6.1 lists the semantic tokens every skin defines");
        }
    }
    let mut it = sets.iter();
    let Some((first_mode, first)) = it.next() else {
        return;
    };
    for (mode, set) in it {
        let Some(span) = spans.get(mode).copied() else {
            continue;
        };
        for (name, t) in &first.tokens {
            match set.tokens.get(name) {
                None => {
                    col.error(
                        Code::TokenMode,
                        span,
                        format!("token `{name}` is defined in `{first_mode}` but not in `{mode}`"),
                    )
                    .help("every mode defines the same tokens");
                }
                Some(o) if o.value.kind() != t.value.kind() => {
                    col.error(
                        Code::TokenMode,
                        o.span,
                        format!(
                            "token `{name}` is {} in `{first_mode}` and {} in `{mode}`",
                            t.value.kind().noun(),
                            o.value.kind().noun()
                        ),
                    );
                }
                Some(_) => {}
            }
        }
        for name in set.tokens.keys().filter(|n| !first.tokens.contains_key(*n)) {
            col.error(
                Code::TokenMode,
                span,
                format!("token `{name}` is defined in `{mode}` but not in `{first_mode}`"),
            )
            .help("every mode defines the same tokens");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(
            Rgba::parse("#7aa2ff"),
            Some(Rgba {
                r: 0x7a,
                g: 0xa2,
                b: 0xff,
                a: 255
            })
        );
        assert_eq!(Rgba::parse("#00000066").map(|c| c.a), Some(0x66));
        for bad in [
            "#fff",
            "#0008",
            "7aa2ff",
            "#7aa2f",
            "#gggggg",
            "transparent",
            "",
        ] {
            assert!(Rgba::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn required_tokens_leave_out_the_fallback_tones() {
        let r = required();
        assert!(r.contains(&"tone.ready") && r.contains(&"font.body") && r.contains(&"space.8"));
        assert!(!r.contains(&"tone.queued") && !r.contains(&"tone.paused"));
    }
}

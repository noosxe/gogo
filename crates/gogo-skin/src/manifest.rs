//! The manifest, `skin.kdl` (design 27 §2).

use std::collections::BTreeSet;
use std::fmt;

use gogo_contract::ContractVersion;

use crate::diag::{Code, Collector};
use crate::names::{is_skin_name, is_style_name, suggest};
use crate::raw::{Lit, RawNode};
use crate::source::Span;

/// A token set a skin provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    pub const ALL: &[Mode] = &[Mode::Dark, Mode::Light];

    pub const fn name(self) -> &'static str {
        match self {
            Mode::Dark => "dark",
            Mode::Light => "light",
        }
    }

    pub fn from_name(s: &str) -> Option<Mode> {
        Self::ALL.iter().copied().find(|m| m.name() == s)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A skin's own semver version: `MAJOR.MINOR.PATCH`, with optional `-pre` and `+build`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
    pub build: Option<String>,
}

impl SkinVersion {
    /// Parse a semver 2.0 version (no leading zeros in numbers, dot-separated
    /// identifiers of `[0-9A-Za-z-]`). `None` when it is not one.
    pub fn parse(s: &str) -> Option<Self> {
        let (rest, build) = match s.split_once('+') {
            Some((r, b)) => (r, Some(b)),
            None => (s, None),
        };
        let (core, pre) = match rest.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (rest, None),
        };
        let num = |p: &str| {
            let ok = !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p == "0" || !p.starts_with('0'));
            ok.then(|| p.parse::<u64>().ok()).flatten()
        };
        let ids_ok = |ids: &str, numeric_no_zero: bool| {
            !ids.is_empty()
                && ids.split('.').all(|i| {
                    !i.is_empty()
                        && i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        && !(numeric_no_zero
                            && i.bytes().all(|b| b.is_ascii_digit())
                            && i.len() > 1
                            && i.starts_with('0'))
                })
        };
        let mut parts = core.split('.');
        let (major, minor, patch) = (
            num(parts.next()?)?,
            num(parts.next()?)?,
            num(parts.next()?)?,
        );
        if parts.next().is_some() || pre.is_some_and(|p| !ids_ok(p, true)) {
            return None;
        }
        if build.is_some_and(|b| !ids_ok(b, false)) {
            return None;
        }
        Some(SkinVersion {
            major,
            minor,
            patch,
            pre: pre.map(str::to_owned),
            build: build.map(str::to_owned),
        })
    }
}

impl fmt::Display for SkinVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        if let Some(b) = &self.build {
            write!(f, "+{b}")?;
        }
        Ok(())
    }
}

/// The type of a skin-local variable (design 26 §6.4).
#[derive(Debug, Clone, PartialEq)]
pub enum UiType {
    Bool,
    Number,
    /// One of the words.
    Enum(Vec<String>),
    /// One of a route's sections.
    Section,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiDefault {
    Bool(bool),
    Number(f64),
    /// An enum word or a section name.
    Word(String),
}

/// A `var` of the manifest's `ui { }` block.
#[derive(Debug, Clone, PartialEq)]
pub struct UiVar {
    pub name: String,
    pub ty: UiType,
    pub default: UiDefault,
    pub span: Span,
}

/// A skin's manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// The skin's name and id.
    pub name: String,
    pub title: String,
    pub version: SkinVersion,
    /// The contract version the skin needs, checked against the app's (S16).
    pub contract: ContractVersion,
    /// The token sets it provides, in the order written.
    pub modes: Vec<Mode>,
    /// The routes it covers; the rest fall back (design 26 §8).
    pub routes: Vec<String>,
    /// The skin it builds on. The lookup arrives with binding (PR B4).
    pub extends: Option<String>,
    pub ui: Vec<UiVar>,
    pub span: Span,
}

/// The strings of a node's arguments, or `None` after reporting why they aren't strings.
fn strings<'a>(col: &mut Collector, n: &'a RawNode, what: &str) -> Option<Vec<(&'a str, Span)>> {
    if !n.props.is_empty() || !n.children.is_empty() {
        col.error(
            Code::ManifestValue,
            n.span,
            format!("`{what}` takes plain values only"),
        );
        return None;
    }
    let mut out = Vec::new();
    for a in &n.args {
        let Lit::Str(s) = &a.lit else {
            col.error(
                Code::ManifestValue,
                a.span,
                format!("`{what}` takes strings, found {}", a.lit.kind()),
            );
            return None;
        };
        out.push((s.as_str(), a.span));
    }
    Some(out)
}

fn one<'a>(col: &mut Collector, n: &'a RawNode, what: &str) -> Option<(&'a str, Span)> {
    let v = strings(col, n, what)?;
    match v.as_slice() {
        [one] => Some(*one),
        _ => {
            col.error(
                Code::ManifestValue,
                n.span,
                format!("`{what}` takes exactly one string"),
            );
            None
        }
    }
}

const FIELDS: &[&str] = &[
    "title", "version", "contract", "modes", "routes", "extends", "ui",
];

/// Parse and check `skin.kdl`. `folder` is the name the package is called: the folder
/// name, or the name the importer gives for a zip.
pub(crate) fn parse(
    col: &mut Collector,
    nodes: &[RawNode],
    file: usize,
    folder: &str,
) -> Option<Manifest> {
    let top = Span::new(file, 0, 0);
    let mut skins = nodes.iter().filter(|n| n.name == "skin");
    let Some(skin) = skins.next() else {
        col.error(Code::ManifestStructure, top, "skin.kdl has no `skin` node")
            .help("start with `skin \"<name>\" { … }` (design 27 §2)");
        return None;
    };
    if let Some(extra) = skins.next() {
        col.error(Code::ManifestStructure, extra.span, "a second `skin` node");
    }
    for n in nodes.iter().filter(|n| n.name != "skin") {
        col.error(
            Code::ManifestStructure,
            n.span,
            format!(
                "`{}` is not part of skin.kdl; it holds one `skin` node",
                n.name
            ),
        );
    }
    let before = col.error_count();

    // The name.
    let name = match (skin.args.as_slice(), skin.props.is_empty()) {
        ([a], true) => match &a.lit {
            Lit::Str(s) => {
                if !is_skin_name(s) {
                    col.error(
                        Code::SkinName,
                        a.span,
                        format!("skin name `{s}` is not lowercase letters, digits and `-`"),
                    )
                    .help("use `a-z`, `0-9` and `-`, at most 64 characters (design 27 §2)");
                } else if s != folder {
                    col.error(
                        Code::SkinName,
                        a.span,
                        format!("skin name `{s}` is not the folder name `{folder}`"),
                    )
                    .help("the folder is named after the skin");
                }
                s.clone()
            }
            other => {
                col.error(
                    Code::SkinName,
                    a.span,
                    format!("the skin name is a string, found {}", other.kind()),
                );
                String::new()
            }
        },
        _ => {
            col.error(
                Code::SkinName,
                skin.span,
                "`skin` takes exactly one argument: its name",
            );
            String::new()
        }
    };

    let mut seen = BTreeSet::new();
    let (mut title, mut version, mut contract, mut modes) = (None, None, None, None);
    let (mut routes, mut extends, mut ui) = (Vec::new(), None, Vec::new());
    for c in &skin.children {
        if !FIELDS.contains(&c.name.as_str()) {
            let help = match suggest(&c.name, FIELDS.iter().copied()) {
                Some(s) => format!("did you mean `{s}`?"),
                None => format!("the fields are {}", FIELDS.join(", ")),
            };
            col.error(
                Code::ManifestStructure,
                c.name_span,
                format!("unknown manifest field `{}`", c.name),
            )
            .help(help);
            continue;
        }
        if !seen.insert(c.name.as_str()) {
            col.error(
                Code::ManifestStructure,
                c.name_span,
                format!("`{}` is set twice", c.name),
            );
            continue;
        }
        match c.name.as_str() {
            "title" => {
                title = one(col, c, "title").map(|(s, sp)| {
                    if s.trim().is_empty() {
                        col.error(Code::ManifestValue, sp, "the title is empty");
                    }
                    s.to_owned()
                });
            }
            "version" => {
                version = one(col, c, "version").and_then(|(s, sp)| {
                    let v = SkinVersion::parse(s);
                    if v.is_none() {
                        col.error(
                            Code::ManifestValue,
                            sp,
                            format!("version `{s}` is not semver"),
                        )
                        .help("write MAJOR.MINOR.PATCH, for example \"1.0.0\"");
                    }
                    v
                });
            }
            "contract" => {
                contract = one(col, c, "contract").and_then(|(s, sp)| contract_version(col, s, sp));
            }
            "modes" => modes = parse_modes(col, c),
            "routes" => routes = parse_routes(col, c),
            "extends" => {
                extends = one(col, c, "extends").and_then(|(s, sp)| {
                    if !is_skin_name(s) {
                        col.error(
                            Code::ManifestValue,
                            sp,
                            format!("`extends \"{s}\"` is not a skin name"),
                        );
                        None
                    } else if s == name {
                        col.error(Code::ManifestValue, sp, "a skin can't extend itself");
                        None
                    } else {
                        Some(s.to_owned())
                    }
                });
            }
            _ => ui = parse_ui(col, c),
        }
    }
    for field in ["title", "version", "contract", "modes"] {
        if !seen.contains(field) {
            col.error(
                Code::ManifestStructure,
                skin.span,
                format!("the manifest has no `{field}`"),
            );
        }
    }
    if col.error_count() > before {
        return None;
    }
    Some(Manifest {
        name,
        title: title?,
        version: version?,
        contract: contract?,
        modes: modes?,
        routes,
        extends,
        ui,
        span: skin.span,
    })
}

fn contract_version(col: &mut Collector, s: &str, sp: Span) -> Option<ContractVersion> {
    let Ok(v) = s.parse::<ContractVersion>() else {
        col.error(
            Code::ContractVersion,
            sp,
            format!("contract `{s}` is not MAJOR.MINOR"),
        )
        .help("write it as \"1.0\"");
        return None;
    };
    if !ContractVersion::CURRENT.accepts(v) {
        col.error(
            Code::ContractVersion,
            sp,
            format!(
                "this app implements contract {}; the skin needs {v} (S16)",
                ContractVersion::CURRENT
            ),
        )
        .help("a skin loads when its major matches and its minor is not newer than the app's");
        return None;
    }
    Some(v)
}

fn parse_modes(col: &mut Collector, c: &RawNode) -> Option<Vec<Mode>> {
    let v = strings(col, c, "modes")?;
    if v.is_empty() {
        col.error(
            Code::ManifestValue,
            c.span,
            "`modes` lists at least one mode",
        )
        .help("use \"dark\", \"light\" or both");
        return None;
    }
    let mut out: Vec<Mode> = Vec::new();
    for (s, sp) in v {
        match Mode::from_name(s) {
            Some(m) if out.contains(&m) => {
                col.error(
                    Code::ManifestValue,
                    sp,
                    format!("mode `{s}` is listed twice"),
                );
            }
            Some(m) => out.push(m),
            None => {
                col.error(Code::ManifestValue, sp, format!("unknown mode `{s}`"))
                    .help("the modes are \"dark\" and \"light\"");
            }
        }
    }
    Some(out)
}

fn parse_routes(col: &mut Collector, c: &RawNode) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (s, sp) in strings(col, c, "routes").unwrap_or_default() {
        if gogo_contract::routes::route_spec(s).is_none() {
            let names = gogo_contract::routes::ROUTES.iter().map(|r| r.name);
            let help = match suggest(s, names.clone()) {
                Some(n) => format!("did you mean `{n}`?"),
                None => format!("the routes are {}", names.collect::<Vec<_>>().join(", ")),
            };
            col.error(Code::UnknownRoute, sp, format!("unknown route `{s}` (S16)"))
                .help(help);
        } else if out.iter().any(|r| r == s) {
            col.error(
                Code::ManifestValue,
                sp,
                format!("route `{s}` is listed twice"),
            );
        } else {
            out.push(s.to_owned());
        }
    }
    out
}

fn parse_ui(col: &mut Collector, c: &RawNode) -> Vec<UiVar> {
    let mut out: Vec<UiVar> = Vec::new();
    if !c.args.is_empty() || !c.props.is_empty() {
        col.error(Code::UiVar, c.span, "`ui` takes only `var` children");
    }
    for v in &c.children {
        if v.name != "var" {
            col.error(
                Code::UiVar,
                v.name_span,
                format!("`ui` holds `var`, found `{}`", v.name),
            );
            continue;
        }
        if let Some(var) = parse_var(col, v) {
            if out.iter().any(|o| o.name == var.name) {
                col.error(
                    Code::UiVar,
                    v.span,
                    format!("variable `{}` is declared twice", var.name),
                );
            } else {
                out.push(var);
            }
        }
    }
    out
}

fn parse_var(col: &mut Collector, v: &RawNode) -> Option<UiVar> {
    let name = match (v.str_arg(0), v.args.len()) {
        (Some(n), 1) if is_style_name(n) && !n.contains('.') => n,
        _ => {
            col.error(
                Code::UiVar,
                v.span,
                "`var` takes one name: lowercase letters, digits, `-` and `_`",
            );
            return None;
        }
    };
    let ty_prop = v.prop("type");
    let default = v.prop("default");
    for p in &v.props {
        if p.name != "type" && p.name != "default" {
            col.error(
                Code::UiVar,
                p.name_span,
                format!("`var` has `type` and `default`, not `{}`", p.name),
            );
        }
    }
    let (Some(ty_prop), Some(default)) = (ty_prop, default) else {
        col.error(
            Code::UiVar,
            v.span,
            format!("`var \"{name}\"` needs `type=` and `default=`"),
        );
        return None;
    };
    let ty = match &ty_prop.value.lit {
        Lit::Str(s) => parse_type(s),
        _ => None,
    };
    let Some(ty) = ty else {
        col.error(Code::UiVar, ty_prop.value.span, "unknown variable type")
            .help("the types are `bool`, `number`, `enum(a b …)` and `section`");
        return None;
    };
    let word = |col: &mut Collector, ok: &dyn Fn(&str) -> bool, what: &str| match &default.value.lit
    {
        Lit::Str(s) if ok(s) => Some(UiDefault::Word(s.clone())),
        _ => {
            col.error(
                Code::UiVar,
                default.value.span,
                format!("the default of `{name}` must be {what}"),
            );
            None
        }
    };
    let default = match &ty {
        UiType::Bool => match default.value.lit {
            Lit::Bool(b) => Some(UiDefault::Bool(b)),
            _ => {
                col.error(
                    Code::UiVar,
                    default.value.span,
                    format!("the default of `{name}` must be #true or #false"),
                );
                None
            }
        },
        UiType::Number => match default.value.lit {
            Lit::Num(n) => Some(UiDefault::Number(n)),
            _ => {
                col.error(
                    Code::UiVar,
                    default.value.span,
                    format!("the default of `{name}` must be a number"),
                );
                None
            }
        },
        UiType::Enum(words) => word(col, &|s| words.iter().any(|w| w == s), "one of its words"),
        UiType::Section => word(
            col,
            &|s| {
                gogo_contract::routes::ROUTES
                    .iter()
                    .any(|r| r.sections.contains(&s))
            },
            "a section of a route",
        ),
    }?;
    Some(UiVar {
        name: name.to_owned(),
        ty,
        default,
        span: v.span,
    })
}

fn parse_type(s: &str) -> Option<UiType> {
    match s {
        "bool" => Some(UiType::Bool),
        "number" => Some(UiType::Number),
        "section" => Some(UiType::Section),
        _ => {
            let inner = s.strip_prefix("enum(")?.strip_suffix(')')?;
            let words: Vec<String> = inner.split_whitespace().map(str::to_owned).collect();
            let unique: BTreeSet<_> = words.iter().collect();
            let ok = !words.is_empty()
                && unique.len() == words.len()
                && words.iter().all(|w| is_style_name(w) && !w.contains('.'));
            ok.then_some(UiType::Enum(words))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver() {
        for ok in [
            "1.0.0",
            "0.1.0",
            "1.2.3-rc.1",
            "1.0.0+build.5",
            "1.0.0-a-b+c",
        ] {
            assert!(SkinVersion::parse(ok).is_some(), "{ok}");
        }
        for bad in [
            "1.0", "01.0.0", "1.0.0.0", "1.0.0-", "1.0.0-01", "v1.0.0", "", "1.0.x",
        ] {
            assert!(SkinVersion::parse(bad).is_none(), "{bad}");
        }
        assert_eq!(
            SkinVersion::parse("1.2.3-rc.1")
                .map(|v| v.to_string())
                .as_deref(),
            Some("1.2.3-rc.1")
        );
    }

    #[test]
    fn ui_types() {
        assert_eq!(
            parse_type("enum(grid list)"),
            Some(UiType::Enum(vec!["grid".into(), "list".into()]))
        );
        assert_eq!(parse_type("enum()"), None);
        assert_eq!(parse_type("enum(a a)"), None);
        assert_eq!(parse_type("int"), None);
    }
}

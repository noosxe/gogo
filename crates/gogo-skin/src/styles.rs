//! Styles, `styles.kdl` (design 27 §4).
//!
//! A `style "<name>" { … }` holds property declarations and state blocks (`:hover { … }`).
//! A style named after an element applies to every element of that kind; others are
//! applied with `style="a b"`. Properties are checked against the table in
//! [`crate::props`]; token references must name tokens of the right type.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{self, Node};
use crate::diag::{Code, Collector};
use crate::names::{is_style_name, suggest};
use crate::props::{self, Ctx, Facts};
use crate::raw::RawNode;
use crate::source::Span;

/// The states a style may vary by (design 27 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    Hover,
    Pressed,
    Focus,
    /// Toggles, the current tab, a highlighted row.
    On,
    Disabled,
}

impl State {
    pub const ALL: &[State] = &[
        State::Hover,
        State::Pressed,
        State::Focus,
        State::On,
        State::Disabled,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            State::Hover => "hover",
            State::Pressed => "pressed",
            State::Focus => "focus",
            State::On => "on",
            State::Disabled => "disabled",
        }
    }

    pub fn from_name(s: &str) -> Option<State> {
        Self::ALL.iter().copied().find(|st| st.name() == s)
    }
}

/// A named style: declarations, and the declarations of each state.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub name: String,
    /// Each node is one property: its name and its parsed values.
    pub props: Vec<Node>,
    pub states: Vec<(State, Vec<Node>)>,
    pub span: Span,
}

/// Parse and check `styles.kdl`.
pub(crate) fn parse(
    col: &mut Collector,
    nodes: &[RawNode],
    ctx: &Ctx<'_>,
) -> BTreeMap<String, Style> {
    let mut out = BTreeMap::new();
    for n in nodes {
        if n.name != "style" {
            col.error(
                Code::StyleStructure,
                n.name_span,
                format!("styles.kdl holds `style` nodes, found `{}`", n.name),
            );
            continue;
        }
        let name = match (n.args.as_slice(), n.str_arg(0)) {
            ([_], Some(s)) if n.props.is_empty() => s,
            _ => {
                col.error(
                    Code::StyleStructure,
                    n.span,
                    "`style` takes one name: `style \"card\" { … }`",
                );
                continue;
            }
        };
        if !is_style_name(name) {
            col.error(
                Code::StyleName,
                n.args[0].span,
                format!("`{name}` is not a style name"),
            )
            .help("use lowercase letters, digits, `-`, `_` and `.`");
            continue;
        }
        if out.contains_key(name) {
            col.error(
                Code::StyleName,
                n.args[0].span,
                format!("style `{name}` is defined twice"),
            );
            continue;
        }
        let mut style = Style {
            name: name.to_owned(),
            props: Vec::new(),
            states: Vec::new(),
            span: n.span,
        };
        let mut facts = Facts::default();
        let mut seen = BTreeSet::new();
        for c in &n.children {
            if let Some(state) = c.name.strip_prefix(':') {
                let Some(st) = State::from_name(state) else {
                    let names = State::ALL.iter().map(|s| s.name());
                    let help = match suggest(state, names) {
                        Some(s) => format!("did you mean `:{s}`?"),
                        None => {
                            "the states are :hover, :pressed, :focus, :on and :disabled".to_owned()
                        }
                    };
                    col.error(
                        Code::UnknownState,
                        c.name_span,
                        format!("unknown state `{}`", c.name),
                    )
                    .help(help);
                    continue;
                };
                if !c.args.is_empty() || !c.props.is_empty() {
                    col.error(
                        Code::StyleStructure,
                        c.span,
                        "a state takes a block, not values",
                    );
                    continue;
                }
                if style.states.iter().any(|(s, _)| *s == st) {
                    col.error(
                        Code::StyleStructure,
                        c.name_span,
                        format!("`:{state}` is set twice"),
                    );
                    continue;
                }
                let mut sfacts = Facts::default();
                let mut sseen = BTreeSet::new();
                let mut decls = Vec::new();
                for d in &c.children {
                    if let Some(node) = decl(col, ctx, &mut sfacts, &mut sseen, d) {
                        decls.push(node);
                    }
                }
                sfacts.finish(col);
                style.states.push((st, decls));
            } else if let Some(node) = decl(col, ctx, &mut facts, &mut seen, c) {
                style.props.push(node);
            }
        }
        facts.finish(col);
        out.insert(name.to_owned(), style);
    }
    out
}

/// One property declaration of a style or state.
fn decl(
    col: &mut Collector,
    ctx: &Ctx<'_>,
    facts: &mut Facts,
    seen: &mut BTreeSet<String>,
    c: &RawNode,
) -> Option<Node> {
    if c.name.starts_with(':') {
        col.error(Code::StyleStructure, c.name_span, "states don't nest");
        return None;
    }
    if !c.children.is_empty() || !c.props.is_empty() {
        col.error(
            Code::StyleStructure,
            c.span,
            format!(
                "`{}` is a property: its values follow it on the line",
                c.name
            ),
        );
        return None;
    }
    let Some(spec) = props::style_prop(&c.name) else {
        col.error(
            Code::UnknownProperty,
            c.name_span,
            format!("unknown style property `{}`", c.name),
        )
        .help(props::unknown_help(&c.name, &[]));
        return None;
    };
    if !seen.insert(c.name.clone()) && !props::repeatable(&c.name) {
        col.warn(
            Code::DuplicateProperty,
            c.name_span,
            format!("`{}` is set twice in this block; the last one wins", c.name),
        );
    }
    let node = ast::convert(col, c);
    let args: Vec<&ast::Arg> = node.args.iter().collect();
    // A declaration with no value at all.
    if args.is_empty() && !matches!(spec.shape, props::Shape::NoneOnly) {
        col.error(
            Code::PropertyValue,
            c.span,
            format!("`{}` has no value", c.name),
        );
        return None;
    }
    props::check(col, ctx, facts, spec, &args, c.span);
    Some(node)
}

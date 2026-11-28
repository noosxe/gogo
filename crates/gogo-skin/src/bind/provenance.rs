//! Where each bound node came from, for the skin dev mode's inspector (design 25 §8).
//!
//! [`SkinPackage::bind_traced`](crate::SkinPackage::bind_traced) records it beside the
//! tree; no other bind records or allocates anything. The values are evaluated a second
//! time in the same scope, which expressions can't tell apart (they are total and free of
//! side effects, S20), so a traced bind gives the same tree as any other.
//!
//! Only what the binder itself can read is here, and it is kept as short text for
//! display: a masked value reads as its mask (S1, S2), a list as its length and a record
//! as its field names.

use std::collections::HashMap;

use gogo_contract::Value;
use gogo_ui_ir::NodeId;

use super::{ArgValue, Binder, Var};
use crate::ast::{Node as AstNode, Val};
use crate::expr::Template;
use crate::source::Span;

/// How many characters of a source or a value are kept.
pub const MAX_SHOWN: usize = 160;

/// How many nodes one bind records; the rest of a longer page has no [`Origin`].
pub const MAX_TRACED: usize = 20_000;

/// How many local names one node records.
const MAX_LOCALS: usize = 32;

/// What a traced bind knows about the tree's nodes. The ids are those of that tree.
#[derive(Debug, Clone, Default)]
pub struct Provenance {
    nodes: HashMap<NodeId, Origin>,
    left_out: HashMap<NodeId, Vec<LeftOut>>,
}

impl Provenance {
    /// The element that made `node`. A node an element made for itself (its text, its
    /// icon) has none: ask for its parent.
    pub fn get(&self, node: NodeId) -> Option<&Origin> {
        self.nodes.get(&node)
    }

    /// The children of `node` the skin has and this bind didn't draw.
    pub fn left_out(&self, node: NodeId) -> &[LeftOut] {
        self.left_out.get(&node).map_or(&[], Vec::as_slice)
    }
}

/// The element a node was bound from.
#[derive(Debug, Clone)]
pub struct Origin {
    /// `button`, `row`, `text`…
    pub element: String,
    /// `routes/game.kdl:12:5`.
    pub location: String,
    /// The text is the default skin's: a fallback route or a borrowed fragment.
    pub default_skin: bool,
    /// What the element sits inside that makes no node of its own, outermost first:
    /// `each game …`, `use "card" …`, `case "x" …`.
    pub within: Vec<String>,
    /// The words of `style=`.
    pub styles: Vec<String>,
    /// Each expression written on the element, with what it gave.
    pub bindings: Vec<BoundValue>,
    /// The skin's names in scope: `each` items, `let`, component parameters.
    pub locals: Vec<(String, String)>,
    pub action: Option<ActionOrigin>,
}

/// One expression of an element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundValue {
    /// The property, or `value` for the element's argument.
    pub name: String,
    /// The text as the skin has it, quotes and all.
    pub source: String,
    pub value: String,
}

/// The action a node is bound to, with the arguments it would be called with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOrigin {
    pub name: &'static str,
    pub args: Vec<(&'static str, String)>,
}

/// A child that made no node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOut {
    pub element: String,
    pub location: String,
    pub why: String,
}

/// What a traced bind keeps as it goes.
#[derive(Debug, Default)]
pub(crate) struct Trace {
    pub(crate) out: Provenance,
    within: Vec<String>,
}

/// `text` cut to [`MAX_SHOWN`] characters.
fn cut(text: &str) -> String {
    match text.char_indices().nth(MAX_SHOWN) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// A value as the inspector lists it. Never the content of a list or a record.
fn shown(v: &Value) -> String {
    match v {
        Value::Null => "null".to_owned(),
        Value::Str(s) => format!("\"{}\"", cut(s)),
        Value::List(l) if l.len() == 1 => "list of 1 item".to_owned(),
        Value::List(l) => format!("list of {} items", l.len()),
        Value::Map(m) => cut(&format!(
            "record {{{}}}",
            m.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
        )),
        Value::Bool(_) | Value::Num(_) | Value::Masked => v.display(),
    }
}

impl Binder<'_> {
    fn source_text(&self, at: Span) -> String {
        cut(self.code.sources().slice(at).unwrap_or_default())
    }

    fn template_value(&self, t: &Template) -> String {
        let scope = self.scope();
        match t.eval(&scope) {
            Ok(v) => shown(&v),
            Err(e) => cut(&format!("error: {e}")),
        }
    }

    /// The expressions among the element's argument and properties.
    fn expressions(&self, n: &AstNode) -> Vec<BoundValue> {
        let mut out = Vec::new();
        let args = n.args.iter().map(|a| ("value", a));
        let props = n.props.iter().map(|p| (p.name.as_str(), &p.arg));
        for (name, arg) in args.chain(props) {
            let value = match &arg.val {
                Val::Text(Template::Text(_)) => continue,
                Val::Text(t) => self.template_value(t),
                // An action's `args`: shown with the action.
                _ => continue,
            };
            out.push(BoundValue {
                name: name.to_owned(),
                source: self.source_text(arg.span),
                value,
            });
        }
        out
    }

    fn locals(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        // The last of a name is the one in scope.
        for var in self.vars.iter().rev() {
            if seen.contains(&var.name.as_str()) || out.len() == MAX_LOCALS {
                continue;
            }
            seen.push(&var.name);
            out.push((var.name.clone(), self.local(var)));
        }
        out.reverse();
        out
    }

    fn local(&self, var: &Var) -> String {
        let value = self.var_value(var).map_or_else(|| "null".to_owned(), shown);
        match &var.alias {
            Some(source) => format!("{source}: {value}"),
            None => value,
        }
    }

    /// Record the element that made `node`, with the scope it was bound in.
    pub(crate) fn trace_node(&mut self, n: &AstNode, node: NodeId) {
        let Some(trace) = &self.trace else {
            return;
        };
        if trace.out.nodes.len() >= MAX_TRACED {
            return;
        }
        let action = self
            .tree
            .get(node)
            .and_then(|node| node.action)
            .and_then(|r| self.actions.get(r))
            .map(|call| ActionOrigin {
                name: call.spec.name,
                args: call
                    .args
                    .iter()
                    .map(|a| {
                        let name = a.route_param.map_or(a.param.name, |p| p.name);
                        let value = match &a.value {
                            ArgValue::Value(v) => shown(v),
                            ArgValue::OnInput(_) => "set by the input".to_owned(),
                        };
                        (name, value)
                    })
                    .collect(),
            });
        let origin = Origin {
            element: n.name.clone(),
            location: self.code.sources().location(n.name_span),
            default_skin: self
                .default_skin()
                .is_some_and(|d| std::ptr::eq(d, self.code)),
            within: trace.within.clone(),
            styles: n.resolved.style_words.clone(),
            bindings: self.expressions(n),
            locals: self.locals(),
            action,
        };
        if let Some(trace) = &mut self.trace {
            trace.out.nodes.insert(node, origin);
        }
    }

    /// Record that `n` made no node under `parent`, because of the value at `val`.
    pub(crate) fn trace_left_out(&mut self, n: &AstNode, parent: NodeId, val: &Val, at: Span) {
        if self.trace.is_none() {
            return;
        }
        let value = match val {
            Val::Text(t) => self.template_value(t),
            _ => self
                .val(val, at)
                .map_or_else(|_| "error".to_owned(), |v| shown(&v)),
        };
        let left = LeftOut {
            element: n.name.clone(),
            location: self.code.sources().location(n.name_span),
            why: format!("{} → {value}", self.source_text(at)),
        };
        if let Some(trace) = &mut self.trace {
            let list = trace.out.left_out.entry(parent).or_default();
            // One per element of the skin, however often a repeat meets it.
            if list.len() < MAX_LOCALS && !list.iter().any(|l| l.location == left.location) {
                list.push(left);
            }
        }
    }

    /// Enter something that makes no node of its own: what `label` says of `n` is listed
    /// on every node bound until [`trace_leave`](Self::trace_leave).
    pub(crate) fn trace_within(&mut self, n: &AstNode, label: impl FnOnce() -> String) {
        if self.trace.is_none() {
            return;
        }
        let location = self.code.sources().location(n.name_span);
        if let Some(trace) = &mut self.trace {
            trace.within.push(format!("{} ({location})", label()));
        }
    }

    pub(crate) fn trace_leave(&mut self) {
        if let Some(trace) = &mut self.trace {
            trace.within.pop();
        }
    }
}

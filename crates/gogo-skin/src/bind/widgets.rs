//! App widgets in a bound tree (design 26 §5, design 27 §5.5).
//!
//! The app draws these widgets, because they carry the safety flow and secrets (design 25
//! §4). A skin places one by name and styles it: the tree gets one node for it
//! (`DrawnBy::App`, no part), in the place the skin gave it, and a [`WidgetSlot`] says
//! which variants and which part styles the skin chose. The app builds the widget's own
//! nodes from its content and these styles.

use gogo_contract::Value;
use gogo_contract::widgets::{AppWidget, Part, Placement, Variant};
use gogo_ui_ir::{
    ActionRef, AppPart, DrawnBy, LayoutStyle, NodeId, Paint, PartId, TextStyle, WidgetId,
};

use super::{BindErrorKind, Binder, Cx, R};
use crate::ast::Node as AstNode;
use crate::props;

/// One app widget placed by a skin.
#[derive(Debug, Clone)]
pub struct WidgetSlot {
    pub id: WidgetId,
    /// The node the widget occupies in the tree.
    pub node: NodeId,
    pub widget: &'static AppWidget,
    /// The action whose plan or result the widget shows (`for=`), with its arguments.
    pub action: Option<ActionRef>,
    /// Each variant of the widget and the value the skin chose, the default when it chose
    /// none (design 26 §5).
    pub variants: Vec<(&'static Variant, &'static str)>,
    /// `inline` or `modal`; `None` for a widget the app places itself.
    pub placement: Option<Placement>,
    /// The other properties the skin gave, evaluated (`game=`, `title=`, …).
    pub props: Vec<(String, Value)>,
    /// The parts the skin styled or hid.
    pub parts: Vec<PartStyle>,
}

/// What a skin says about one part of an app widget.
#[derive(Debug, Clone)]
pub struct PartStyle {
    pub id: PartId,
    pub part: &'static Part,
    /// `hidden=#true`, only ever for an optional part (S5).
    pub hidden: bool,
    pub layout: LayoutStyle,
    pub paint: Paint,
    pub text: TextStyle,
    /// The names of the style properties the skin set, so the app can tell them from its
    /// own defaults.
    pub set: Vec<String>,
}

/// Properties of a widget element that are not data for the widget.
const NOT_DATA: &[&str] = &[
    "id",
    "style",
    "when",
    "label",
    "for",
    "args",
    "placement",
    "progress",
];

impl<'a> Binder<'a> {
    pub(crate) fn widget(
        &mut self,
        n: &'a AstNode,
        w: &'static AppWidget,
        parent: NodeId,
        key: &str,
        cx: &Cx,
    ) -> R<NodeId> {
        let at = n.span;
        let c = self.compute(w.name, Some(n), &[], cx, |_| {})?;
        let wid = WidgetId::new(u32::try_from(self.widgets.len()).unwrap_or(u32::MAX));
        let mut node = Self::slot_node(&c);
        node.drawn_by = DrawnBy::App(AppPart {
            widget: wid,
            part: None,
        });
        let id = self.add(parent, key, node, at)?;

        let action = self.call(n, "for", "args")?;

        let variants = w
            .variants
            .iter()
            .filter_map(|v| {
                let chosen = n
                    .plain_prop(v.name)
                    .and_then(|t| v.values.iter().find(|x| **x == t))
                    .or_else(|| v.values.first())?;
                Some((v, *chosen))
            })
            .collect();
        let placement = match n.plain_prop("placement") {
            Some("inline") => Some(Placement::Inline),
            Some("modal") => Some(Placement::Modal),
            _ => None,
        };

        let mut data = Vec::new();
        for p in &n.props {
            let name = p.name.as_str();
            if NOT_DATA.contains(&name)
                || w.variant(name).is_some()
                || props::style_prop(name).is_some()
            {
                continue;
            }
            data.push((p.name.clone(), self.val(&p.arg.val, p.arg.span)?));
        }

        let mut parts = Vec::new();
        for p in n.children.iter().filter(|c| c.name == "part") {
            let Some(name) = p.plain_arg(0) else { continue };
            let Some((index, part)) = w.parts.iter().enumerate().find(|(_, x)| x.name == name)
            else {
                return Err(self.fail(
                    BindErrorKind::Missing(format!("`{}` has no part `{name}`", w.name)),
                    Some(p.span),
                ));
            };
            let hidden = matches!(
                p.prop("hidden").map(|h| &h.arg.val),
                Some(crate::ast::Val::Bool(true))
            );
            // S5: only an optional part can go, whatever the loader let through.
            if hidden && !part.optional {
                return Err(self.fail(
                    BindErrorKind::HiddenPart {
                        widget: w.name,
                        part: part.name,
                    },
                    Some(p.span),
                ));
            }
            let own = format!("{}.{}", w.name, part.name);
            let layers = self.layers(&own, Some(p), &[])?;
            let pc = self.compute_with(&layers, Some(p), true, &[], &c.text, |_| {})?;
            let mut set: Vec<String> = Vec::new();
            for l in &layers {
                if let Some(s) = self.code.styles.get(l) {
                    set.extend(s.props.iter().map(|d| d.name.clone()));
                }
            }
            set.extend(
                p.props
                    .iter()
                    .filter(|q| props::style_prop(&q.name).is_some())
                    .map(|q| q.name.clone()),
            );
            set.sort();
            set.dedup();
            parts.push(PartStyle {
                id: PartId::new(u32::try_from(index).unwrap_or(u32::MAX)),
                part,
                hidden,
                layout: pc.layout,
                paint: pc.paint,
                text: pc.text.run(),
                set,
            });
        }

        self.widgets.push(WidgetSlot {
            id: wid,
            node: id,
            widget: w,
            action,
            variants,
            placement,
            props: data,
            parts,
        });
        Ok(id)
    }

    fn slot_node(c: &super::style::Computed) -> gogo_ui_ir::Node {
        let mut n = gogo_ui_ir::Node::container();
        n.layout = c.layout.clone();
        n.paint = c.paint.clone();
        n
    }
}

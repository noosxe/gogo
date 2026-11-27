//! Elements to nodes (design 27 §5): control flow, containers, content and controls.

use std::borrow::Cow;
use std::sync::Arc;

use gogo_contract::Value;
use gogo_ui_ir::{
    A11y, Align, Dim, Direction, Display, Fill, ImageSource, Justify, Kind, Node, NodeId, Role,
    ScrollSpec, Shape, Track, VirtualList,
};

use super::actions::{ActionCall, BoundTerm, PendingArg, TermVal, assign, capture};
use super::list::Picked;
use super::popups::{MenuEntry, PopupContent, PopupSlot, select_options};
use super::style::{Computed, TextCtx, apply};
use super::{BindErrorKind, Binder, Cx, R, Var, term};

use crate::ast::{Node as AstNode, Term, Val};
use crate::diag::{Collector, LoadScope};
use crate::expr::{Expr, Template};
use crate::package::Component;
use crate::props;
use crate::source::Span;
use crate::styles::State;

/// The most items one `each` or `data-table` draws (design 27 §8).
pub const MAX_ROWS: usize = crate::expr::MAX_LIST;

/// How far outside 0 to 1 a `progress` may be and still count as the end it is next to
/// (design 27 §5.6).
pub const PROGRESS_EPSILON: f64 = 1e-6;

impl<'a> Binder<'a> {
    // ---- lists of elements ----

    /// Bind sibling elements under `parent`. A `let` binds for the rest of the block.
    pub(crate) fn children(
        &mut self,
        list: &'a [AstNode],
        parent: NodeId,
        path: &str,
        cx: &Cx,
    ) -> R<()> {
        let mark = self.vars.len();
        for (i, n) in list.iter().enumerate() {
            self.element(n, parent, path, i, cx)?;
        }
        self.vars.truncate(mark);
        Ok(())
    }

    fn element(&mut self, n: &'a AstNode, parent: NodeId, path: &str, i: usize, cx: &Cx) -> R<()> {
        if n.name == "let" {
            return self.let_(n);
        }
        if let Some(w) = n.prop("when")
            && !self.truthy(&w.arg.val, w.arg.span)?
        {
            return Ok(());
        }
        self.step(n.span)?;
        self.step(n.span)?;
        let key = format!("{path}/{}.{i}", n.name);
        self.enter(n.span)?;
        let r = match n.name.as_str() {
            "each" => self.each(n, parent, &key, cx),
            "switch" => self.switch(n, parent, &key, cx),
            "use" => self.use_(n, parent, &key, cx),
            _ => self
                .build(n, parent, &key, cx)
                .and_then(|id| self.finish(n, id)),
        };
        self.leave();
        r
    }

    /// `id=` and `progress=`, set on the node an element made.
    fn finish(&mut self, n: &'a AstNode, id: NodeId) -> R<()> {
        if let Some(name) = self.prop_text(n, "id")?
            && let Some(node) = self.tree.node_mut(id)
        {
            node.name = Some(Arc::from(name.as_str()));
        }
        if let Some(p) = n.prop("progress") {
            let v = self.val(&p.arg.val, p.arg.span)?;
            let f = match v {
                Value::Num(f) => f,
                Value::Null => return Ok(()),
                other => {
                    return Err(self.property(
                        "progress",
                        format!("expected a number from 0 to 1, found {}", other.type_name()),
                        p.arg.span,
                    ));
                }
            };
            // A ratio of two sums can overshoot by rounding: a hair outside is clamped.
            if !f.is_finite() || !(-PROGRESS_EPSILON..=1.0 + PROGRESS_EPSILON).contains(&f) {
                return Err(self.fail(BindErrorKind::Progress(f), Some(p.arg.span)));
            }
            let f = f.clamp(0.0, 1.0);
            if let Some(node) = self.tree.node_mut(id) {
                node.progress = Some(f as f32);
                node.layout.width = Dim::Pct(f as f32 * 100.0);
            }
        }
        Ok(())
    }

    // ---- control flow ----

    fn let_(&mut self, n: &'a AstNode) -> R<()> {
        for p in &n.props {
            // A name for the app's data is not a copy of it: a library is long.
            let var = match self.data_name(&p.arg.val) {
                Some(source) => Var::alias(p.name.clone(), source),
                None => Var::plain(p.name.clone(), self.val(&p.arg.val, p.arg.span)?),
            };
            self.vars.push(var);
        }
        Ok(())
    }

    fn each(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<()> {
        let (Some(var), Some(p)) = (n.plain_arg(0), n.prop("in")) else {
            return Ok(());
        };
        // Data can't fail a valid skin, so a long list is cut, silently (design 27 §8).
        let list = self.items(&p.arg.val, p.arg.span, "each")?;
        for (k, item) in list.into_iter().enumerate() {
            self.step(n.span)?;
            let mark = self.vars.len();
            self.vars.push(Var::plain(var, item));
            let path = format!("{key}#{k}");
            let r = self.children(&n.children, parent, &path, cx);
            self.vars.truncate(mark);
            r?;
        }
        Ok(())
    }

    /// The first [`MAX_ROWS`] items of the list `v` evaluates to (a null is no items). Only
    /// those are copied out of the data (design 27 §8).
    pub(super) fn items(&self, v: &Val, at: Span, what: &'static str) -> R<Vec<Value>> {
        let not_a_list = |got: &Value| {
            self.fail(
                BindErrorKind::NotAList {
                    what,
                    got: got.type_name(),
                },
                Some(at),
            )
        };
        if let Val::Text(t) = v {
            let sc = self.scope();
            let found = t.eval(&sc).map_err(|e| self.fail(e.into(), Some(at)))?;
            return match found {
                Cow::Borrowed(Value::List(l)) => Ok(l.iter().take(MAX_ROWS).cloned().collect()),
                Cow::Owned(Value::List(mut l)) => {
                    l.truncate(MAX_ROWS);
                    Ok(l)
                }
                Cow::Borrowed(Value::Null) | Cow::Owned(Value::Null) => Ok(Vec::new()),
                Cow::Borrowed(other) => Err(not_a_list(other)),
                Cow::Owned(other) => Err(not_a_list(&other)),
            };
        }
        match self.val(v, at)? {
            Value::List(mut l) => {
                l.truncate(MAX_ROWS);
                Ok(l)
            }
            Value::Null => Ok(Vec::new()),
            other => Err(not_a_list(&other)),
        }
    }

    fn switch(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<()> {
        let Some(a) = n.args.first() else {
            return Ok(());
        };
        let v = self.val(&a.val, a.span)?.display();
        let hit = n
            .children
            .iter()
            .find(|c| c.name == "case" && c.plain_arg(0) == Some(v.as_str()))
            .or_else(|| n.children.iter().find(|c| c.name == "else"));
        match hit {
            Some(c) => self.children(&c.children, parent, key, cx),
            None => Ok(()),
        }
    }

    /// `use "component" p=…` and `use default "fragment"`.
    fn use_(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<()> {
        let borrowed = n.args.len() == 2;
        let name = match n.args.as_slice() {
            [a] => a.val.plain().map(str::to_owned),
            [_, f] => Some(self.val(&f.val, f.span)?.display()),
            _ => None,
        }
        .unwrap_or_default();
        let missing =
            |this: &Self, what: String| this.fail(BindErrorKind::Missing(what), Some(n.span));
        let (comp, code): (&'a Component, &'a crate::package::SkinPackage) = if borrowed {
            let Some(d) = self.default_skin() else {
                return Err(missing(
                    self,
                    format!("`use default \"{name}\"`: there is no default skin to borrow from"),
                ));
            };
            let Some(c) = d.components.get(&name) else {
                return Err(missing(
                    self,
                    format!("the default skin has no fragment `{name}`"),
                ));
            };
            self.check_fragment(&name, n.span)?;
            (c, d)
        } else {
            let Some(c) = self.code.components.get(&name) else {
                return Err(missing(self, format!("the skin has no component `{name}`")));
            };
            (c, self.code)
        };
        // The parameters are evaluated where the `use` is.
        let mut params: Vec<Var> = Vec::new();
        for pname in &comp.params {
            let var = match n.prop(pname) {
                None => Var::plain(pname.clone(), Value::Null),
                Some(p) => match &p.arg.val {
                    Val::Terms(ts) => {
                        let terms = self.terms(ts, true)?;
                        let mut var = Var::plain(pname.clone(), terms_value(&terms));
                        var.terms = Some(terms);
                        var
                    }
                    v => Var {
                        value: self.val(v, p.arg.span)?,
                        spec: self.action_given(n, pname, v),
                        ..Var::plain(pname.clone(), Value::Null)
                    },
                },
            };
            params.push(var);
        }
        // The body sees its parameters and the app's data, not the caller's variables.
        let saved_vars = std::mem::replace(&mut self.vars, params);
        let saved_code = std::mem::replace(&mut self.code, code);
        // The nodes the body adds under `parent` come after the ones already there, and
        // the first of them is what `id=` names.
        let before = self.tree.children(parent).len();
        let r = self.children(&comp.body, parent, key, cx);
        self.code = saved_code;
        self.vars = saved_vars;
        r?;
        if let Some(name) = self.prop_text(n, "id")?
            && let Some(&first) = self.tree.children(parent).get(before)
            && let Some(node) = self.tree.node_mut(first)
        {
            node.name = Some(Arc::from(name.as_str()));
        }
        Ok(())
    }

    /// The action a component parameter stands for, as the loader resolved it: the one a
    /// `use` named in plain text, or the one a parameter of the caller handed on with
    /// `action="{p}"`. A variable that hides the caller's parameter has none (S16).
    fn action_given(
        &self,
        n: &AstNode,
        pname: &str,
        v: &Val,
    ) -> Option<&'static gogo_contract::ActionSpec> {
        if let Some((_, spec)) = n.resolved.use_actions.iter().find(|(p, _)| p == pname) {
            return Some(*spec);
        }
        let Val::Text(Template::Expr(Expr::Name { name, .. })) = v else {
            return None;
        };
        self.vars
            .iter()
            .rev()
            .find(|x| &x.name == name)
            .and_then(|x| x.spec)
    }

    /// A borrowed fragment holds against this skin's manifest (design 27 §5.7): its `ui.*`
    /// paths and enum words are this skin's, checked with the loader's own rules.
    ///
    /// The fragment and every component it leads to are checked, once for each skin that
    /// borrows them: the result is kept in that skin's package.
    fn check_fragment(&mut self, name: &str, at: Span) -> R<()> {
        if !self.checked().insert(name.to_owned()) {
            return Ok(());
        }
        let Some(default) = self.default_skin() else {
            return Ok(());
        };
        let manifest = self.ui_manifest();
        let found = self.route_code().fragments().get_or_check(name, || {
            let mut col = Collector::default();
            col.sources = default.sources().clone();
            col.scope = LoadScope::Route(String::new());
            crate::resolve::fragment(&mut col, manifest, &default.components, name);
            col.report
                .findings()
                .iter()
                .filter(|f| f.is_error())
                .cloned()
                .collect()
        });
        if found.is_empty() {
            return Ok(());
        }
        let errors = found
            .iter()
            .cloned()
            .map(|mut f| {
                f.level = crate::diag::Level::Route(self.route.clone());
                f
            })
            .collect();
        Err(self.fail(BindErrorKind::Borrow(errors), Some(at)))
    }

    // ---- action arguments ----

    /// Evaluate the terms of an `args`. A bare `{p}` that is a component parameter given an
    /// `args` stands for those terms (design 27 §5.7).
    pub(crate) fn terms(&self, ts: &'a [Term], allow_input: bool) -> R<Vec<BoundTerm>> {
        let mut out = Vec::new();
        for t in ts {
            if t.name.is_none()
                && let Template::Expr(Expr::Name { name, .. }) = &t.value
                && let Some(v) = self.vars.iter().rev().find(|v| &v.name == name)
                && let Some(given) = &v.terms
            {
                out.extend(given.iter().cloned());
                continue;
            }
            let reads_input = allow_input && (t.value.mentions("on") || t.value.mentions("value"));
            let val = if reads_input {
                let names = t.value.names();
                let captured = capture(names, |n| {
                    self.vars
                        .iter()
                        .rev()
                        .find(|v| v.name == n)
                        .and_then(|v| self.var_value(v))
                });
                let params = self.scope().route_params;
                TermVal::Pending(PendingArg::new(t.value.clone(), captured).on_route(params))
            } else {
                TermVal::Value(self.eval(&t.value, t.span)?)
            };
            out.push(BoundTerm {
                name: t.name.clone(),
                val,
            });
        }
        Ok(out)
    }

    /// The action a bare `{p}` stands for, when `p` is a component parameter that was given
    /// one at load.
    fn action_of(&self, v: &Val) -> Option<&'static gogo_contract::ActionSpec> {
        let Val::Text(Template::Expr(Expr::Name { name, .. })) = v else {
            return None;
        };
        self.vars
            .iter()
            .rev()
            .find(|x| &x.name == name)
            .and_then(|x| x.spec)
    }

    /// The call an element's `action` (and `args`) stand for, entered in the table.
    pub(crate) fn call(
        &mut self,
        n: &'a AstNode,
        act: &str,
        args: &str,
    ) -> R<Option<gogo_ui_ir::ActionRef>> {
        let Some(p) = n.prop(act) else {
            return Ok(None);
        };
        let spec = match n.resolved.actions.iter().find(|b| b.prop == act) {
            Some(b) => b.spec,
            None => {
                // An action named through a component parameter (`action="{action}"`) is
                // the one the loader resolved at the `use` that gave it (S16). A name that
                // wasn't resolved there is never looked up: the loader refuses the skin
                // first, so this is unreachable for a loaded one.
                if self.val(&p.arg.val, p.arg.span)?.display().is_empty() {
                    return Ok(None);
                }
                match self.action_of(&p.arg.val) {
                    Some(spec) => spec,
                    None => {
                        return Err(self.fail(
                            BindErrorKind::Action(
                                "this action was not resolved when the skin loaded (S16)"
                                    .to_owned(),
                            ),
                            Some(p.arg.span),
                        ));
                    }
                }
            }
        };
        let terms = match n.prop(args).map(|p| &p.arg.val) {
            Some(Val::Terms(ts)) => {
                let input = gogo_contract::actions::INPUT_ELEMENTS.contains(&n.name.as_str());
                self.terms(ts, input)?
            }
            _ => Vec::new(),
        };
        let args = assign(spec, terms, self.ui_manifest())
            .map_err(|e| self.action_problem(e, p.arg.span))?;
        let location = self.code.sources().location(p.arg.span);
        self.register(
            ActionCall {
                spec,
                args,
                location,
            },
            p.arg.span,
        )
        .map(Some)
    }

    // ---- styles ----

    /// The style names an element applies, in order: its kind, any extra, then `style=`;
    /// and, for an element with a `tone`, the `<name>.<tone>` variant of each of those that
    /// is defined, after them all (design 27 §4).
    pub(crate) fn layers(
        &self,
        kind: &str,
        n: Option<&AstNode>,
        extra: &[String],
    ) -> R<Vec<String>> {
        let mut v = vec![kind.to_owned()];
        v.extend(extra.iter().cloned());
        if let Some(n) = n {
            v.extend(n.resolved.style_words.iter().cloned());
        }
        if let Some(n) = n
            && let Some(tone) = self.prop_text(n, "tone")?
            && !tone.is_empty()
        {
            let variants: Vec<String> = v.iter().map(|s| format!("{s}.{tone}")).collect();
            v.extend(variants);
        }
        Ok(v)
    }

    fn decls(&self, c: &mut Computed, decls: &'a [AstNode]) -> R<()> {
        for d in decls {
            let vals = self.operands(&d.operands, d.span)?;
            if let Err(why) = apply(c, &d.name, &vals) {
                return Err(self.property(&d.name, why, d.span));
            }
        }
        Ok(())
    }

    /// Style layers, then the states in effect, then the element's own properties
    /// (design 27 §4).
    pub(crate) fn compute_with(
        &self,
        layers: &[String],
        n: Option<&'a AstNode>,
        inline: bool,
        states: &[State],
        text: &TextCtx,
        init: impl FnOnce(&mut Computed),
    ) -> R<Computed> {
        let code = self.code;
        let mut c = Computed {
            text: text.clone(),
            ..Computed::default()
        };
        init(&mut c);
        for l in layers {
            if let Some(s) = code.styles.get(l) {
                self.decls(&mut c, &s.props)?;
            }
        }
        for l in layers {
            if let Some(s) = code.styles.get(l) {
                for st in states {
                    if let Some((_, d)) = s.states.iter().find(|(x, _)| x == st) {
                        self.decls(&mut c, d)?;
                    }
                }
            }
        }
        if inline && let Some(n) = n {
            for p in &n.props {
                if props::style_prop(&p.name).is_none() || p.operands.is_empty() {
                    continue;
                }
                let vals = self.operands(&p.operands, p.arg.span)?;
                if let Err(why) = apply(&mut c, &p.name, &vals) {
                    return Err(self.property(&p.name, why, p.arg.span));
                }
            }
        }
        Ok(c)
    }

    pub(crate) fn compute(
        &self,
        kind: &str,
        n: Option<&'a AstNode>,
        states: &[State],
        cx: &Cx,
        init: impl FnOnce(&mut Computed),
    ) -> R<Computed> {
        let layers = self.layers(kind, n, &[])?;
        self.compute_with(&layers, n, true, states, &cx.text, init)
    }

    /// The paint a node has while it has keyboard focus, if a style gives it one. `layers`
    /// are the ones the node was computed with.
    fn focus_paint(
        &self,
        layers: &[String],
        n: Option<&'a AstNode>,
        states: &[State],
        cx: &Cx,
        init: impl FnOnce(&mut Computed),
    ) -> R<Option<gogo_ui_ir::Paint>> {
        if !self.has_state(layers, State::Focus) {
            return Ok(None);
        }
        let mut st = states.to_vec();
        st.push(State::Focus);
        Ok(Some(
            self.compute_with(layers, n, true, &st, &cx.text, init)?
                .paint,
        ))
    }

    // ---- node helpers ----

    pub(super) fn node_of(c: &Computed) -> Node {
        let mut n = Node::container();
        n.layout = c.layout.clone();
        n.paint = c.paint.clone();
        n
    }

    fn text_child(
        &mut self,
        parent: NodeId,
        key: &str,
        text: &str,
        ctx: &TextCtx,
        at: Span,
    ) -> R<NodeId> {
        let shown = ctx.shown(text);
        let mut node = Node::new(Kind::Text {
            text: shown,
            style: ctx.run(),
        });
        node.a11y.role = Role::Label;
        self.add(parent, key, node, at)
    }

    /// An icon or the `dot` mark, sized to the text it sits by.
    fn icon_child(
        &mut self,
        parent: NodeId,
        key: &str,
        name: &str,
        ctx: &TextCtx,
        at: Span,
    ) -> R<NodeId> {
        if !crate::icons::is_icon(name) {
            return Err(self.fail(
                BindErrorKind::Missing(format!("there is no icon `{name}`")),
                Some(at),
            ));
        }
        let mut node;
        if name == "dot" {
            node = Node::container();
            node.layout.width = Dim::Px(7.0);
            node.layout.height = Dim::Px(7.0);
            node.paint.shape = Shape::Circle;
        } else {
            let s = (ctx.size * 0.9).round().max(1.0);
            node = Node::new(Kind::Icon(Arc::from(name)));
            node.layout.width = Dim::Px(s);
            node.layout.height = Dim::Px(s);
        }
        node.layout.shrink = 0.0;
        node.paint.fill = Some(Fill::Solid(ctx.color));
        self.add(parent, key, node, at)
    }

    /// The key hint chip of a button or tab: `P`, `Enter`.
    fn key_hint(
        &mut self,
        parent: NodeId,
        key: &str,
        hint: &str,
        cx: &Cx,
        ctx: &TextCtx,
        at: Span,
    ) -> R<()> {
        let layers = self.layers("key-hint", None, &[])?;
        let c = self.compute_with(&layers, None, false, &[], ctx, |c| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
        })?;
        let _ = cx;
        let id = self.add(parent, key, Self::node_of(&c), at)?;
        self.text_child(id, &format!("{key}/text"), hint, &c.text, at)?;
        Ok(())
    }

    /// The accessible name of a control: `label=`, `title=`, its own text, or the `field`
    /// around it (S14).
    fn name_of(&self, n: &'a AstNode, own: Option<&str>, cx: &Cx) -> R<Option<String>> {
        for p in ["label", "title"] {
            if let Some(t) = self.prop_text(n, p)?
                && !t.trim().is_empty()
            {
                return Ok(Some(t));
            }
        }
        if let Some(t) = own
            && !t.trim().is_empty()
        {
            return Ok(Some(t.to_owned()));
        }
        Ok(cx.field.as_deref().map(str::to_owned))
    }

    // ---- elements ----

    fn build(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let at = n.span;
        let row = |c: &mut Computed| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
        };
        match n.name.as_str() {
            "column" | "box" | "form" => {
                let c = self.compute(&n.name, Some(n), &[], cx, |_| {})?;
                self.container(n, parent, key, cx, c, A11y::default())
            }
            "row" => {
                let c = self.compute("row", Some(n), &[], cx, row)?;
                self.container(n, parent, key, cx, c, A11y::default())
            }
            "grid" => {
                let c = self.compute("grid", Some(n), &[], cx, |c| {
                    c.layout.display = Display::Grid;
                })?;
                self.container(n, parent, key, cx, c, A11y::default())
            }
            "scroll" => self.scroll(n, parent, key, cx),
            "list" => self.list(n, parent, key, cx),
            "layer" => {
                let mut c = self.compute("layer", Some(n), &[], cx, |c| {
                    c.layout.display = Display::Grid;
                })?;
                c.layout.columns = vec![Track::Fr(1.0)];
                c.layout.rows = vec![Track::Fr(1.0)];
                let id = self.container(n, parent, key, cx, c, A11y::default())?;
                // Every child shares the one cell; `inset` ones are placed over it.
                let kids: Vec<NodeId> = self.tree.children(id).to_vec();
                for k in kids {
                    if let Some(node) = self.tree.node_mut(k) {
                        node.layout.cell = Some((1, 1));
                    }
                }
                Ok(id)
            }
            "spacer" => {
                let c = self.compute("spacer", Some(n), &[], cx, |c| c.layout.grow = 1.0)?;
                self.add(parent, key, Self::node_of(&c), at)
            }
            "divider" => {
                let c = self.compute("divider", Some(n), &[], cx, |c| {
                    c.layout.height = Dim::Px(1.0);
                    c.layout.align_self = Some(Align::Stretch);
                })?;
                self.add(parent, key, Self::node_of(&c), at)
            }
            "text" | "label" => self.text_el(n, parent, key, cx),
            "icon" => {
                let c = self.compute("icon", Some(n), &[], cx, |_| {})?;
                let name = self.arg_text(n)?;
                let id = self.icon_child(parent, key, &name, &c.text, at)?;
                if let Some(node) = self.tree.node_mut(id) {
                    node.layout = c.layout.clone();
                    let s = (c.text.size * 0.9).round().max(1.0);
                    if c.layout.width == Dim::Auto {
                        node.layout.width = Dim::Px(s);
                    }
                    if c.layout.height == Dim::Auto {
                        node.layout.height = Dim::Px(s);
                    }
                    node.paint = c.paint.clone();
                    node.paint.fill.get_or_insert(Fill::Solid(c.text.color));
                }
                Ok(id)
            }
            "art" => self.art(n, parent, key, cx),
            "image" => self.image(n, parent, key, cx),
            "dot" | "led" => self.dot(n, parent, key, cx),
            "chip" | "badge" | "display" => self.chip(n, parent, key, cx),
            "button" => self.button(n, parent, key, cx),
            "toggle" => self.toggle(n, parent, key, cx),
            "select" => self.select(n, parent, key, cx),
            "tabs" => self.tabs(n, parent, key, cx),
            "menu" => self.menu(n, parent, key, cx),
            "card" | "module" => self.card(n, parent, key, cx),
            "field" => self.field(n, parent, key, cx),
            "data-table" => self.data_table(n, parent, key, cx),
            "outlet" => self.outlet(n, parent, key, cx),
            name => {
                if let Some(w) = gogo_contract::widgets::WIDGETS
                    .iter()
                    .find(|w| w.name == name && w.depth == gogo_contract::widgets::Depth::B)
                {
                    return self.widget(n, w, parent, key, cx);
                }
                Err(self.fail(
                    BindErrorKind::Missing(format!(
                        "`{name}` is not an element the binder draws on its own"
                    )),
                    Some(at),
                ))
            }
        }
    }

    fn container(
        &mut self,
        n: &'a AstNode,
        parent: NodeId,
        key: &str,
        cx: &Cx,
        c: Computed,
        a11y: A11y,
    ) -> R<NodeId> {
        let mut node = Self::node_of(&c);
        node.a11y = a11y;
        let id = self.add(parent, key, node, n.span)?;
        let cx2 = Cx {
            text: c.text,
            field: cx.field.clone(),
        };
        self.children(&n.children, id, key, &cx2)?;
        Ok(id)
    }

    /// A scroll container: a column unless a style says otherwise, with the scrollbar
    /// the `scrollbar` and `scrollbar-thumb` styles give it.
    fn scroll(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let c = self.compute("scroll", Some(n), &[], cx, |_| {})?;
        let mut node = Self::node_of(&c);
        let mut spec = self.scroll_spec(cx)?;
        spec.per_route = self.in_outlet;
        node.scroll = Some(spec);
        node.a11y.name = self.prop_text(n, "label")?;
        let id = self.add(parent, key, node, n.span)?;
        let cx2 = Cx {
            text: c.text,
            field: cx.field.clone(),
        };
        let outlets = self.outlets;
        self.scrolls += 1;
        let r = self.children(&n.children, id, key, &cx2);
        self.scrolls -= 1;
        r?;
        // One that holds the outlet moves with the route too.
        if self.outlets != outlets
            && let Some(spec) = self.tree.node_mut(id).and_then(|n| n.scroll.as_mut())
        {
            spec.per_route = true;
        }
        Ok(id)
    }

    fn scroll_spec(&self, cx: &Cx) -> R<ScrollSpec> {
        let mut spec = ScrollSpec::default();
        let part = |this: &Self, name: &str| {
            this.compute_with(&[name.to_owned()], None, false, &[], &cx.text, |_| {})
        };
        let bar = part(self, "scrollbar")?;
        if let Dim::Px(w) = bar.layout.width {
            spec.width = w.clamp(0.0, 64.0);
            spec.radius = spec.width / 2.0;
        }
        spec.track = bar.paint.fill;
        let thumb = part(self, "scrollbar-thumb")?;
        if let Some(fill) = thumb.paint.fill {
            spec.thumb = fill;
        }
        if thumb.paint.radius[0] > 0.0 {
            spec.radius = thumb.paint.radius[0];
        }
        Ok(spec)
    }

    fn text_el(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let mut c = self.compute(&n.name, Some(n), &[], cx, |_| {})?;
        if self.prop(n, "mono")?.is_some_and(|v| v.truthy())
            && let Some(m) = c.text.mono.clone()
        {
            c.text.set_family(&m);
        }
        let text = self.arg_text(n)?;
        let shown = c.text.shown(&text);
        let mut node = Node::new(Kind::Text {
            text: shown,
            style: c.text.run(),
        });
        node.layout = c.layout;
        node.paint = c.paint;
        node.a11y.role = Role::Label;
        self.add(parent, key, node, n.span)
    }

    fn art(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let c = self.compute("art", Some(n), &[], cx, |_| {})?;
        let handle = self.arg_text(n)?;
        let fit = match self.prop_text(n, "fit")?.as_deref() {
            Some("contain") => gogo_ui_ir::Fit::Contain,
            Some("fill") => gogo_ui_ir::Fit::Fill,
            _ => gogo_ui_ir::Fit::Cover,
        };
        let mut node = Self::node_of(&c);
        node.kind = Kind::Image {
            source: ImageSource::Handle(Arc::from(handle.as_str())),
            fit,
        };
        let name = self.name_of(n, None, cx)?;
        if name.is_none() {
            return Err(self.fail(BindErrorKind::NoName("`art`"), Some(n.span)));
        }
        node.a11y.role = Role::Image;
        node.a11y.name = name;
        self.add(parent, key, node, n.span)
    }

    fn image(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let c = self.compute("image", Some(n), &[], cx, |_| {})?;
        let src = n.plain_prop("src").unwrap_or_default();
        if !self.code.assets.contains_key(src) {
            return Err(self.fail(
                BindErrorKind::Missing(format!("`{src}` is not in the skin's assets")),
                Some(n.span),
            ));
        }
        let mut node = Self::node_of(&c);
        node.kind = Kind::Image {
            source: ImageSource::Asset(Arc::from(src)),
            fit: gogo_ui_ir::Fit::Cover,
        };
        let name = self.name_of(n, None, cx)?;
        if name.is_none() {
            return Err(self.fail(BindErrorKind::NoName("`image`"), Some(n.span)));
        }
        node.a11y.role = Role::Image;
        node.a11y.name = name;
        self.add(parent, key, node, n.span)
    }

    fn dot(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let on = self.prop(n, "on")?.is_some_and(|v| v.truthy());
        let states: &[State] = if on { &[State::On] } else { &[] };
        let c = self.compute(&n.name, Some(n), states, cx, |c| {
            c.paint.shape = Shape::Circle;
            c.layout.width = Dim::Px(8.0);
            c.layout.height = Dim::Px(8.0);
            c.layout.shrink = 0.0;
        })?;
        let mut node = Self::node_of(&c);
        if let Some(label) = self.prop_text(n, "label")? {
            node.a11y.role = Role::Image;
            node.a11y.name = Some(label);
            if n.name == "led" {
                node.a11y.checked = Some(on);
            }
        }
        self.add(parent, key, node, n.span)
    }

    fn chip(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let mut c = self.compute(&n.name, Some(n), &[], cx, |c| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
        })?;
        if self.prop(n, "mono")?.is_some_and(|v| v.truthy())
            && let Some(m) = c.text.mono.clone()
        {
            c.text.set_family(&m);
        }
        let text = self.arg_text(n)?;
        let mut node = Self::node_of(&c);
        node.a11y.role = if n.name == "display" {
            Role::Status
        } else {
            Role::Label
        };
        node.a11y.name = Some(text.clone());
        let id = self.add(parent, key, node, n.span)?;
        if let Some(icon) = self.prop_text(n, "icon")? {
            self.icon_child(id, &format!("{key}/icon"), &icon, &c.text, n.span)?;
        }
        self.text_child(id, &format!("{key}/text"), &text, &c.text, n.span)?;
        Ok(id)
    }

    fn button(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let on = self.prop(n, "on")?.map(|v| v.truthy());
        let states: Vec<State> = if on == Some(true) {
            vec![State::On]
        } else {
            vec![]
        };
        let init = |c: &mut Computed| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
            c.layout.justify = Some(Justify::Center);
        };
        let layers = self.layers("button", Some(n), &[])?;
        let c = self.compute_with(&layers, Some(n), true, &states, &cx.text, init)?;
        let text = self.arg_text(n)?;
        let name = self.name_of(n, Some(&text), cx)?;
        let Some(name) = name else {
            return Err(self.fail(BindErrorKind::NoName("`button`"), Some(n.span)));
        };
        let hint = self.prop_text(n, "key")?;
        let id = self.add(parent, key, Self::node_of(&c), n.span)?;
        if let Some(icon) = self.prop_text(n, "icon")? {
            self.icon_child(id, &format!("{key}/icon"), &icon, &c.text, n.span)?;
        }
        if !text.is_empty() {
            self.text_child(id, &format!("{key}/text"), &text, &c.text, n.span)?;
        }
        let cx2 = Cx {
            text: c.text.clone(),
            field: cx.field.clone(),
        };
        self.children(&n.children, id, key, &cx2)?;
        if let (Some(h), true) = (&hint, c.key_hint) {
            self.key_hint(id, &format!("{key}/key"), h, &cx2, &c.text, n.span)?;
        }
        let action = self.call(n, "action", "args")?;
        let focus = self.focus_paint(&layers, Some(n), &states, cx, init)?;
        if let Some(node) = self.tree.node_mut(id) {
            node.a11y.role = Role::Button;
            node.a11y.name = Some(name);
            node.a11y.focusable = action.is_some();
            node.a11y.selected = on;
            node.a11y.key = hint;
            node.action = action;
            node.focus_paint = focus;
        }
        Ok(id)
    }

    fn toggle(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let on = self.prop(n, "on")?.is_some_and(|v| v.truthy());
        let states: Vec<State> = if on { vec![State::On] } else { vec![] };
        let init = |c: &mut Computed| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
            c.layout.width = Dim::Px(34.0);
            c.layout.height = Dim::Px(18.0);
            c.layout.padding = gogo_ui_ir::Sides::all(2.0);
            c.layout.shrink = 0.0;
        };
        let layers = self.layers("toggle", Some(n), &[])?;
        let mut c = self.compute_with(&layers, Some(n), true, &states, &cx.text, init)?;
        if on {
            c.layout.justify = Some(Justify::End);
        }
        let Some(name) = self.name_of(n, None, cx)? else {
            return Err(self.fail(BindErrorKind::NoName("`toggle`"), Some(n.span)));
        };
        let id = self.add(parent, key, Self::node_of(&c), n.span)?;
        let layers = self.layers("toggle-knob", None, &[])?;
        let k = self.compute_with(&layers, None, false, &states, &c.text, |k| {
            k.paint.shape = Shape::Circle;
            k.layout.width = Dim::Px(14.0);
            k.layout.height = Dim::Px(14.0);
        })?;
        self.add(id, &format!("{key}/knob"), Self::node_of(&k), n.span)?;
        let action = self.call(n, "action", "args")?;
        let focus = self.focus_paint(&layers, Some(n), &states, cx, init)?;
        if let Some(node) = self.tree.node_mut(id) {
            node.a11y.role = Role::Switch;
            node.a11y.name = Some(name);
            node.a11y.focusable = action.is_some();
            node.a11y.checked = Some(on);
            node.action = action;
            node.focus_paint = focus;
        }
        Ok(id)
    }

    fn select(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let init = |c: &mut Computed| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
        };
        let layers = self.layers("select", Some(n), &[])?;
        let mut c = self.compute_with(&layers, Some(n), true, &[], &cx.text, init)?;
        if self.prop(n, "mono")?.is_some_and(|v| v.truthy())
            && let Some(m) = c.text.mono.clone()
        {
            c.text.set_family(&m);
        }
        let Some(name) = self.name_of(n, None, cx)? else {
            return Err(self.fail(BindErrorKind::NoName("`select`"), Some(n.span)));
        };
        let raw = self.prop(n, "value")?.unwrap_or(Value::Null);
        let options = match self.prop(n, "options")? {
            Some(Value::List(l)) => l,
            Some(Value::Str(s)) => s.split_whitespace().map(Value::str).collect(),
            _ => Vec::new(),
        };
        let shown = select_shown(&raw, &options);
        let choices = select_options(&raw, &options[..options.len().min(MAX_ROWS)]);
        let id = self.add(parent, key, Self::node_of(&c), n.span)?;
        let t = self.text_child(id, &format!("{key}/value"), &shown, &c.text, n.span)?;
        if let Some(node) = self.tree.node_mut(t) {
            node.layout.grow = 1.0;
        }
        self.icon_child(
            id,
            &format!("{key}/chevron"),
            "chevron-down",
            &c.text,
            n.span,
        )?;
        let action = self.call(n, "action", "args")?;
        let focus = self.focus_paint(&layers, Some(n), &[], cx, init)?;
        if let Some(node) = self.tree.node_mut(id) {
            node.a11y.role = Role::ComboBox;
            node.a11y.name = Some(name);
            node.a11y.value = Some(shown);
            node.a11y.focusable = action.is_some();
            node.action = action;
            node.focus_paint = focus;
        }
        self.popups.push(PopupSlot {
            node: id,
            content: PopupContent::Select(choices),
        });
        Ok(id)
    }

    fn tabs(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let at = n.span;
        let var = n
            .plain_prop("bind")
            .and_then(|b| b.strip_prefix("ui."))
            .unwrap_or_default()
            .to_owned();
        let sections: Vec<&str> = n
            .plain_prop("sections")
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        let current = match self.current_ui() {
            Value::Map(m) => m.get(&var).map(Value::display).unwrap_or_default(),
            _ => String::new(),
        };
        // A tab looks the same whichever it is but for being the current one, so the two
        // looks are computed at most once each.
        let tab_layers = self.layers("tab", None, &[])?;
        let mut focus_of: [Option<Option<gogo_ui_ir::Paint>>; 2] = [None, None];
        let c = self.compute("tabs", Some(n), &[], cx, |c| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::End);
        })?;
        let mut node = Self::node_of(&c);
        node.a11y.role = Role::TabList;
        node.a11y.name = Some("Sections".to_owned());
        let id = self.add(parent, key, node, at)?;
        let Some(set) = gogo_contract::actions::skin_action("ui.set") else {
            return Err(self.fail(
                BindErrorKind::Missing("the contract has no `ui.set`".to_owned()),
                Some(at),
            ));
        };
        for (i, s) in sections.into_iter().enumerate() {
            let on = s == current;
            let states: Vec<State> = if on { vec![State::On] } else { vec![] };
            let init = |c: &mut Computed| {
                c.layout.direction = Direction::Row;
                c.layout.align_items = Some(Align::Center);
            };
            let tc = self.compute_with(&tab_layers, None, false, &states, &c.text, init)?;
            let tkey = format!("{key}/tab.{s}");
            let tid = self.add(id, &tkey, Self::node_of(&tc), at)?;
            let title = section_title(s);
            self.text_child(tid, &format!("{tkey}/text"), &title, &tc.text, at)?;
            let hint = (i + 1).to_string();
            if tc.key_hint {
                self.key_hint(tid, &format!("{tkey}/key"), &hint, cx, &tc.text, at)?;
            }
            let args = assign(
                set,
                vec![
                    term(None, Value::str(var.as_str())),
                    term(None, Value::str(s)),
                ],
                self.ui_manifest(),
            )
            .map_err(|e| self.action_problem(e, at))?;
            let location = self.code.sources().location(at);
            let action = self.register(
                ActionCall {
                    spec: set,
                    args,
                    location,
                },
                at,
            )?;
            let focus = match &focus_of[usize::from(on)] {
                Some(done) => done.clone(),
                None => {
                    let made = if self.has_state(&tab_layers, State::Focus) {
                        let mut st = states.clone();
                        st.push(State::Focus);
                        Some(
                            self.compute_with(&tab_layers, None, false, &st, &c.text, init)?
                                .paint,
                        )
                    } else {
                        None
                    };
                    focus_of[usize::from(on)] = Some(made.clone());
                    made
                }
            };
            if let Some(t) = self.tree.node_mut(tid) {
                t.a11y.role = Role::Tab;
                t.a11y.name = Some(title);
                t.a11y.focusable = true;
                t.a11y.selected = Some(on);
                t.a11y.key = Some(hint);
                t.action = Some(action);
                t.focus_paint = focus;
            }
        }
        Ok(id)
    }

    fn menu(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let c = self.compute("menu", Some(n), &[], cx, |c| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
        })?;
        let text = self.arg_text(n)?;
        let mut node = Self::node_of(&c);
        node.a11y.role = Role::MenuButton;
        node.a11y.name = Some(text.clone());
        node.a11y.focusable = true;
        let id = self.add(parent, key, node, n.span)?;
        self.text_child(id, &format!("{key}/text"), &text, &c.text, n.span)?;
        self.icon_child(
            id,
            &format!("{key}/chevron"),
            "chevron-down",
            &c.text,
            n.span,
        )?;
        // The items open in a popup, which the tree has no place for.
        let mut entries = Vec::new();
        for item in n.children.iter().filter(|i| i.name == "item") {
            self.step(item.span)?;
            if let Some(w) = item.prop("when")
                && !self.truthy(&w.arg.val, w.arg.span)?
            {
                continue;
            }
            entries.push(self.menu_entry(item, cx)?);
        }
        self.popups.push(PopupSlot {
            node: id,
            content: PopupContent::Menu(entries),
        });
        Ok(id)
    }

    fn menu_entry(&mut self, item: &'a AstNode, cx: &Cx) -> R<MenuEntry> {
        let label = self.arg_text(item)?;
        let action = self.call(item, "action", "args")?;
        let name = self.name_of(item, Some(&label), cx)?;
        if action.is_some() && name.is_none() {
            return Err(self.fail(BindErrorKind::NoName("`item`"), Some(item.span)));
        }
        let icon = self.prop_text(item, "icon")?;
        if let Some(icon) = &icon
            && !crate::icons::is_icon(icon)
        {
            return Err(self.fail(
                BindErrorKind::Missing(format!("there is no icon `{icon}`")),
                Some(item.span),
            ));
        }
        Ok(MenuEntry {
            name: name.unwrap_or_else(|| label.clone()),
            label,
            icon,
            key: self.prop_text(item, "key")?,
            action,
        })
    }

    fn card(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let kind = n.name.as_str();
        let c = self.compute(kind, Some(n), &[], cx, |_| {})?;
        let title = self.prop_text(n, "title")?;
        let mut node = Self::node_of(&c);
        node.a11y.role = Role::Group;
        node.a11y.name = title.clone();
        let id = self.add(parent, key, node, n.span)?;
        let cx2 = Cx {
            text: c.text.clone(),
            field: cx.field.clone(),
        };
        let link = self.prop_text(n, "link")?;
        if title.is_some() || link.is_some() {
            let hl = self.layers(&format!("{kind}-header"), None, &[])?;
            let hc = self.compute_with(&hl, None, false, &[], &c.text, |c| {
                c.layout.direction = Direction::Row;
                c.layout.align_items = Some(Align::Center);
            })?;
            let hkey = format!("{key}/header");
            let hid = self.add(id, &hkey, Self::node_of(&hc), n.span)?;
            if let Some(t) = &title {
                let tl = self.layers(&format!("{kind}-title"), None, &[])?;
                let tc = self.compute_with(&tl, None, false, &[], &hc.text, |_| {})?;
                let mut tn = Node::new(Kind::Text {
                    text: tc.text.shown(t),
                    style: tc.text.run(),
                });
                tn.layout = tc.layout;
                tn.paint = tc.paint;
                tn.a11y.role = Role::Heading;
                self.add(hid, &format!("{key}/title"), tn, n.span)?;
            }
            let rl = format!("{kind}-rule");
            if self.code.styles.contains_key(&rl) {
                let rc = self.compute_with(&[rl], None, false, &[], &hc.text, |c| {
                    c.layout.height = Dim::Px(1.0);
                    c.layout.grow = 1.0;
                })?;
                self.add(hid, &format!("{key}/rule"), Self::node_of(&rc), n.span)?;
            } else {
                let mut sp = Node::container();
                sp.layout.grow = 1.0;
                self.add(hid, &format!("{key}/spacer"), sp, n.span)?;
            }
            if let Some(link) = link {
                let ll = self.layers("link", None, &[])?;
                let lc = self.compute_with(&ll, None, false, &[], &hc.text, |c| {
                    c.layout.direction = Direction::Row;
                    c.layout.align_items = Some(Align::Center);
                })?;
                let lkey = format!("{key}/link");
                let lid = self.add(hid, &lkey, Self::node_of(&lc), n.span)?;
                self.text_child(lid, &format!("{lkey}/text"), &link, &lc.text, n.span)?;
                let action = self.call(n, "link-action", "link-args")?;
                if action.is_some() && link.trim().is_empty() {
                    return Err(self.fail(BindErrorKind::NoName("the card's link"), Some(n.span)));
                }
                if let Some(node) = self.tree.node_mut(lid) {
                    node.a11y.role = Role::Button;
                    node.a11y.name = Some(link);
                    node.a11y.focusable = action.is_some();
                    node.action = action;
                }
            }
        }
        self.children(&n.children, id, key, &cx2)?;
        Ok(id)
    }

    fn field(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let c = self.compute("field", Some(n), &[], cx, |c| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
        })?;
        let label = self.arg_text(n)?;
        let id = self.add(parent, key, Self::node_of(&c), n.span)?;
        let layers = self.layers("field-label", None, &[])?;
        let lc = self.compute_with(&layers, None, false, &[], &c.text, |_| {})?;
        let mut l = Node::new(Kind::Text {
            text: lc.text.shown(&label),
            style: lc.text.run(),
        });
        l.layout = lc.layout;
        l.paint = lc.paint;
        l.a11y.role = Role::Label;
        self.add(id, &format!("{key}/label"), l, n.span)?;
        let cx2 = Cx {
            text: c.text,
            field: Some(Arc::from(label.as_str())),
        };
        self.children(&n.children, id, key, &cx2)?;
        Ok(id)
    }

    fn outlet(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let at = n.span;
        let c = self.compute("outlet", Some(n), &[], cx, |c| c.layout.grow = 1.0)?;
        let current = {
            let sc = self.scope();
            match crate::expr::Scope::var(&sc, "route") {
                Some(Value::Map(m)) => m.get("current").map(Value::display),
                _ => None,
            }
        };
        let Some(current) = current.filter(|c| c != "shell" && !c.is_empty()) else {
            return Err(self.fail(
                BindErrorKind::Missing(
                    "the `outlet` draws `route.current`, which is not a route here".to_owned(),
                ),
                Some(at),
            ));
        };
        let mut node = Self::node_of(&c);
        node.a11y.role = Role::Region;
        node.a11y.name = Some("Main".to_owned());
        let id = self.add(parent, key, node, at)?;
        let Some(code) = self.source_of(&current)? else {
            return Err(self.fail(
                BindErrorKind::Missing(format!("the skin has no `{current}` route")),
                Some(at),
            ));
        };
        let Some(r) = code.routes.get(&current) else {
            return Err(self.fail(
                BindErrorKind::Missing(format!("the skin has no `{current}` route")),
                Some(at),
            ));
        };
        // The route is another route's files, state and variables.
        let saved = self.save_route();
        self.enter_route(code, &current);
        self.vars = Vec::new();
        let cx2 = Cx {
            text: c.text,
            field: None,
        };
        let inside = std::mem::replace(&mut self.in_outlet, true);
        let r = self.children(&r.children, id, key, &cx2);
        self.in_outlet = inside;
        self.outlets += 1;
        self.restore_route(saved);
        r?;
        Ok(id)
    }

    // ---- data-table ----

    fn data_table(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let at = n.span;
        let layers = self.layers("table", Some(n), &[])?;
        let c = self.compute_with(&layers, Some(n), false, &[], &cx.text, |_| {})?;
        // Rows of one fixed height inside a scroll container are virtualised.
        let row_h = self.row_height(&c.text)?;
        let virtual_h = row_h.filter(|_| self.virtualises());
        let rows = match n.prop("rows") {
            Some(p) => match virtual_h {
                Some(h) => {
                    let body = format!("{key}/row#");
                    // Known by the box that holds the rows.
                    let pick = self.window(&format!("{key}/body"), &body, 1, h, 0.0);
                    self.picked(&p.arg.val, p.arg.span, "rows", pick)?
                }
                None => Picked::all(self.items(&p.arg.val, p.arg.span, "rows")?),
            },
            None => Picked::all(Vec::new()),
        };
        let cols: Vec<&'a AstNode> = n.children.iter().filter(|c| c.name == "col").collect();
        let mut node = Self::node_of(&c);
        node.a11y.role = Role::Table;
        node.a11y.name = self.prop_text(n, "label")?;
        let id = self.add(parent, key, node, at)?;
        if virtual_h.is_some()
            && let Some(node) = self.tree.node_mut(id)
        {
            node.layout.row_gap = 0.0;
        }

        let head_layers = self.layers("table-head", None, &[])?;
        let hc = self.compute_with(&head_layers, None, false, &[], &c.text, |c| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
        })?;
        let mut head = Self::node_of(&hc);
        head.a11y.role = Role::Row;
        let hid = self.add(id, &format!("{key}/head"), head, at)?;
        for (j, col) in cols.iter().enumerate() {
            let title = self.arg_text(col)?;
            let cl = self.layers("table-head-cell", None, &[])?;
            let mut cc = self.compute_with(&cl, None, false, &[], &hc.text, |_| {})?;
            self.col_width(col, &mut cc)?;
            let ckey = format!("{key}/head/{j}");
            let mut cell = Self::node_of(&cc);
            cell.a11y.role = Role::Cell;
            let cid = self.add(hid, &ckey, cell, col.span)?;
            self.text_child(cid, &format!("{ckey}/text"), &title, &cc.text, col.span)?;
        }

        let body = match (virtual_h, rows.window.clone()) {
            (Some(item_h), Some(window)) => {
                let mut body = Node::container();
                body.layout.direction = Direction::Column;
                body.layout.shrink = 0.0;
                body.list = Some(VirtualList {
                    count: rows.count,
                    columns: 1,
                    item_h,
                    gap: 0.0,
                    window,
                });
                Some((self.add(id, &format!("{key}/body"), body, at)?, item_h))
            }
            _ => None,
        };
        let mut next = 0;
        for (i, row) in rows.items {
            self.step(at)?;
            if let Some((body, h)) = body
                && i > next
            {
                let skipped = format!("{key}/body/s#{next}");
                self.spacer(body, &skipped, (i - next) as f32 * h, at)?;
            }
            next = i + 1;
            let mark = self.vars.len();
            self.vars.push(Var::plain("row", row));
            let r = match body {
                Some((body, h)) => {
                    self.in_item += 1;
                    let r = self.table_row(n, &cols, body, key, i, &c.text, Some(h));
                    self.in_item -= 1;
                    r
                }
                None => self.table_row(n, &cols, id, key, i, &c.text, None),
            };
            self.vars.truncate(mark);
            r?;
        }
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    fn table_row(
        &mut self,
        n: &'a AstNode,
        cols: &[&'a AstNode],
        table: NodeId,
        key: &str,
        i: u32,
        text: &TextCtx,
        virtual_h: Option<f32>,
    ) -> R<()> {
        let at = n.span;
        let hl = self.prop(n, "highlight")?.is_some_and(|v| v.truthy());
        let states: &[State] = if hl { &[State::On] } else { &[] };
        let rl = self.layers("table-row", None, &[])?;
        let rc = self.compute_with(&rl, None, false, states, text, |c| {
            c.layout.direction = Direction::Row;
            c.layout.align_items = Some(Align::Center);
        })?;
        let rkey = format!("{key}/row#{i}");
        let mut rn = Self::node_of(&rc);
        rn.a11y.role = Role::Row;
        rn.a11y.selected = hl.then_some(true);
        if let Some(h) = virtual_h {
            rn.item = Some(i);
            rn.layout.height = Dim::Px(h);
            rn.layout.shrink = 0.0;
        }
        let rid = self.add(table, &rkey, rn, at)?;
        for (j, col) in cols.iter().enumerate() {
            let mono = self.prop(col, "mono")?.is_some_and(|v| v.truthy());
            let cl = self.layers("table-cell", None, &[])?;
            let mut cc = self.compute_with(&cl, None, false, &[], &rc.text, |c| {
                c.layout.direction = Direction::Row;
                c.layout.align_items = Some(Align::Center);
                c.layout.row_gap = 6.0;
                c.layout.column_gap = 6.0;
            })?;
            if mono && let Some(m) = cc.text.mono.clone() {
                cc.text.set_family(&m);
            }
            self.col_width(col, &mut cc)?;
            let ckey = format!("{rkey}/{j}");
            let mut cell = Self::node_of(&cc);
            cell.a11y.role = Role::Cell;
            let cid = self.add(rid, &ckey, cell, col.span)?;
            if let Some(a) = col.args.get(1) {
                let t = self.val(&a.val, a.span)?.display();
                self.text_child(cid, &format!("{ckey}/text"), &t, &cc.text, col.span)?;
            }
            let cx = Cx {
                text: cc.text,
                field: None,
            };
            self.children(&col.children, cid, &ckey, &cx)?;
        }
        Ok(())
    }

    /// A column's `w`: fixed, or a share of what is left.
    fn col_width(&self, col: &'a AstNode, c: &mut Computed) -> R<()> {
        match col.prop("w") {
            Some(p) => {
                let v = self.val(&p.arg.val, p.arg.span)?;
                let mut scratch = Computed::default();
                if let Err(why) = apply(&mut scratch, "w", &[v]) {
                    return Err(self.property("w", why, p.arg.span));
                }
                c.layout.width = scratch.layout.width;
                c.layout.shrink = 0.0;
            }
            None => {
                c.layout.grow = 1.0;
                c.layout.width = Dim::Px(0.0);
            }
        }
        Ok(())
    }
}

/// A `use`d parameter that was given `args`: one term is its value, several are a list.
fn terms_value(terms: &[BoundTerm]) -> Value {
    let vals: Vec<Value> = terms
        .iter()
        .map(|t| match &t.val {
            TermVal::Value(v) => v.clone(),
            TermVal::Pending(_) => Value::Null,
        })
        .collect();
    match (terms, vals.as_slice()) {
        ([t], [v]) if t.name.is_none() => v.clone(),
        _ => Value::List(vals),
    }
}

/// What a `select` shows: the name of the chosen option, or the value itself.
fn select_shown(raw: &Value, options: &[Value]) -> String {
    let name_of = |m: &std::collections::BTreeMap<String, Value>| {
        m.get("name").or(m.get("id")).map(Value::display)
    };
    let id = match raw {
        Value::Map(m) => m.get("id").map(Value::display),
        v => Some(v.display()),
    };
    let from_options = options.iter().find_map(|o| match o {
        Value::Map(m) if m.get("id").map(Value::display) == id => name_of(m),
        _ => None,
    });
    from_options.unwrap_or_else(|| match raw {
        Value::Map(m) => name_of(m).unwrap_or_default(),
        v => v.display(),
    })
}

/// The title of a game route's section tab.
fn section_title(s: &str) -> String {
    match s {
        "dlc" => "DLC".to_owned(),
        "saves" => "Cloud saves".to_owned(),
        "setup" => "Linux setup".to_owned(),
        other => {
            let mut cs = other.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().chain(cs).collect(),
                None => String::new(),
            }
        }
    }
}

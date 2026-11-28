//! Binding a route to the IR (design 24 §2, design 27 §9 step 4, design 26 §8).
//!
//! [`SkinPackage::bind`] evaluates a loaded skin's route against the app's data and the
//! skin's own `ui` state and gives a [`Bound`]: the resolved [`Tree`](gogo_ui_ir::Tree)
//! the engine lays out and draws, and the tables behind the opaque ids in it. Every
//! template and expression was parsed at load, so binding only evaluates (design 27 §7).
//!
//! # What a tree holds
//!
//! - **Keys** are stable path ids: `root/row.0/button.2`, with `each.1#3` for the fourth
//!   item of a repeat and `use.4` for a component. The same skin with the same data gives
//!   the same keys, so focus and scroll state can survive a re-bind.
//! - **Styles** are resolved: the element's kind style, then its `style=` styles left to
//!   right (their states in effect after their base properties), then its own inline
//!   properties (design 27 §4). Text properties pass from parent to child here; nothing
//!   inherits in the IR.
//! - **Actions** become an [`ActionRef`](gogo_ui_ir::ActionRef) on the node and a
//!   [`ActionCall`] in the [`ActionTable`]: the action's spec and its arguments, evaluated.
//! - **App widgets** become one node each, drawn by the app (`DrawnBy::App`), and a
//!   [`WidgetSlot`] with the skin's variants and per-part styles. The app builds the
//!   widget's content (design 25 §4).
//!
//! - **Menus and selects** are one node each, and a [`PopupSlot`] with the items or
//!   options the app's popup lists.
//!
//! # Fallback
//!
//! A route a skin doesn't cover, one that failed to load, or one whose binding fails, is
//! served by the default skin's route, drawn with the active skin's tokens
//! ([`SkinPackage::bind_with_fallback`], design 26 §8). The reason is in
//! [`Bound::fallbacks`], and a failed binding carries the file and line.
//!
//! # Safety invariants enforced here
//!
//! Binding re-checks what the data can still break: S1/S2 (a masked value never reaches
//! an action or becomes text), S5 (a part that can't be hidden is not hidden), S14 (every
//! control has a name once its text is known), S16 (an action named through a component
//! parameter is one a skin may bind), S21 (a `settings.set` key is on the skin-settable
//! list), and S12's runtime caps. S4 holds by construction: the binder prepares calls and
//! never runs one.

mod actions;
mod elements;
mod error;
mod list;
mod popups;
mod provenance;
mod state;
mod style;
mod tokens;
mod widgets;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gogo_contract::routes::route_spec;
use gogo_contract::{SourceParam, Value};
use gogo_ui_ir::{A11y, ActionRef, Color, Dim, Node, NodeId, Role, Tree};

use crate::ast::{Node as AstNode, Operand, Val};
use crate::expr::{Scope, Template};
use crate::manifest::{Manifest, Mode};
use crate::package::SkinPackage;
use crate::source::Span;
use crate::styles::State;
use crate::tokens::TokenSet;

pub use actions::{ActionCall, ActionTable, ArgValue, CallArg, Input, PendingArg, PendingError};
pub use error::{BindError, BindErrorKind};
pub use popups::{MenuEntry, PopupContent, PopupSlot, SelectOption};
pub use provenance::{
    ActionOrigin, BoundValue, LeftOut, MAX_SHOWN, MAX_TRACED, Origin, Provenance,
};
pub use state::{UiState, UiStateError};
pub use widgets::{PartStyle, WidgetSlot};

use actions::{BoundTerm, Problem, TermVal};
use style::TextCtx;

/// How many nodes one bound route may have (design 27 §8). A longer result is a failed
/// binding, not a truncated one: only a list is cut (at 1 000).
pub const MAX_BOUND_NODES: usize = 100_000;

/// How many steps one bind may take (design 27 §8): each element met and each item of an
/// `each` or `data-table` is one, so nested lists with empty bodies can't spin for ever.
/// More than this is a failed binding, like too many nodes.
pub const MAX_BIND_STEPS: usize = 500_000;

/// How many components, repeats and outlets may nest when bound.
const MAX_DEPTH: usize = 96;

/// Everything a bind needs besides the route's name.
#[derive(Clone, Copy)]
pub struct Binding<'a> {
    /// The app's data: sources, route parameters (`$id`) and `now`. Variables of the skin
    /// (`let`, parameters) and its tokens are the binder's; this scope is asked for the
    /// rest, and its `token` is never called.
    pub scope: &'a dyn Scope,
    /// The skin's own `ui` variables.
    pub ui: &'a UiState,
    /// Which token set to draw with.
    pub mode: Mode,
    /// The default skin: where a route falls back to and a fragment is borrowed from.
    pub default: Option<DefaultSkin<'a>>,
}

/// The default skin and its own `ui` state, which a fallback route reads.
#[derive(Clone, Copy)]
pub struct DefaultSkin<'a> {
    pub skin: &'a SkinPackage,
    pub ui: &'a UiState,
}

pub use list::{Lists, MAX_ITEMS, OVERSCAN};

/// A route, bound.
#[derive(Debug, Clone)]
pub struct Bound {
    pub tree: Tree,
    /// What each [`ActionRef`] in the tree stands for.
    pub actions: ActionTable,
    /// The app widgets in the tree, by [`WidgetId`](gogo_ui_ir::WidgetId) index.
    pub widgets: Vec<WidgetSlot>,
    /// What each `menu` and `select` in the tree opens.
    pub popups: Vec<PopupSlot>,
    /// The routes of this page the default skin drew instead (design 26 §8).
    pub fallbacks: Vec<Fallback>,
    /// Where the nodes came from, from [`SkinPackage::bind_traced`] only.
    pub provenance: Option<Provenance>,
}

/// A route the default skin drew, and why.
#[derive(Debug, Clone)]
pub struct Fallback {
    pub route: String,
    pub why: FallbackWhy,
}

#[derive(Debug, Clone)]
pub enum FallbackWhy {
    /// The skin doesn't cover the route.
    NotCovered,
    /// The route is in the manifest and failed to load: the loader's report says why.
    FailedToLoad,
    /// The route loaded and its binding failed, with the file and line.
    Bind(BindError),
}

/// The token values of each mode, kept with a loaded skin.
#[derive(Debug, Clone, Default)]
pub(crate) struct Tables {
    tokens: BTreeMap<Mode, tokens::Values>,
}

impl Tables {
    pub(crate) fn of(sets: &BTreeMap<Mode, TokenSet>) -> Self {
        Tables {
            tokens: sets.iter().map(|(m, s)| (*m, tokens::values(s))).collect(),
        }
    }
}

impl SkinPackage {
    /// Bind `route` of this skin against the data (design 27 §9 step 4).
    ///
    /// This skin's route is used as it is: a route it lacks, or one that fails, is an
    /// error. A `use default` still borrows from [`Binding::default`]. The `shell` route
    /// draws the route named by the `route.current` source in its `outlet`.
    ///
    /// # Errors
    /// A [`BindError`] with the file and line of the skin text that failed.
    pub fn bind(&self, route: &str, b: &Binding<'_>) -> Result<Bound, BindError> {
        run(self, route, b, &BTreeSet::new(), false, None, false)
    }

    /// Like [`bind`](Self::bind), with per-route fallback to the default skin (design 26
    /// §8): a route this skin doesn't cover, that failed to load or whose binding fails is
    /// drawn from the default skin, with this skin's tokens. The page keeps the rest of
    /// the skin's routes (the shell, say, around a fallback `game` route). Each fallback
    /// is in [`Bound::fallbacks`].
    ///
    /// # Errors
    /// A [`BindError`] when the default skin's own route fails too, or there is no
    /// default skin to fall back to.
    pub fn bind_with_fallback(&self, route: &str, b: &Binding<'_>) -> Result<Bound, BindError> {
        self.fall_back(route, b, None, false)
    }

    /// As [`bind_with_fallback`](Self::bind_with_fallback), drawing of each virtualised
    /// list only the rows `lists` asks for (design 27 §5.4).
    ///
    /// # Errors
    /// As [`bind_with_fallback`](Self::bind_with_fallback).
    pub fn bind_windowed(
        &self,
        route: &str,
        b: &Binding<'_>,
        lists: &Lists,
    ) -> Result<Bound, BindError> {
        self.fall_back(route, b, Some(lists), false)
    }

    /// As [`bind_windowed`](Self::bind_windowed), also recording in
    /// [`Bound::provenance`] where each node came from and what its expressions gave, for
    /// the skin dev mode's inspector (design 25 §8). The tree is the same.
    ///
    /// # Errors
    /// As [`bind_with_fallback`](Self::bind_with_fallback).
    pub fn bind_traced(
        &self,
        route: &str,
        b: &Binding<'_>,
        lists: &Lists,
    ) -> Result<Bound, BindError> {
        self.fall_back(route, b, Some(lists), true)
    }

    fn fall_back(
        &self,
        route: &str,
        b: &Binding<'_>,
        lists: Option<&Lists>,
        traced: bool,
    ) -> Result<Bound, BindError> {
        let mut overrides: BTreeSet<String> = BTreeSet::new();
        let mut notes: Vec<Fallback> = Vec::new();
        loop {
            match run(self, route, b, &overrides, true, lists, traced) {
                Ok(mut bound) => {
                    notes.append(&mut bound.fallbacks);
                    bound.fallbacks = notes;
                    return Ok(bound);
                }
                Err(e) => {
                    let failed = e.route().to_owned();
                    if b.default.is_none() || overrides.contains(&failed) {
                        return Err(e);
                    }
                    overrides.insert(failed.clone());
                    notes.push(Fallback {
                        route: failed,
                        why: FallbackWhy::Bind(e),
                    });
                }
            }
        }
    }
}

fn run<'a>(
    active: &'a SkinPackage,
    route: &str,
    b: &'a Binding<'a>,
    overrides: &'a BTreeSet<String>,
    fallback: bool,
    lists: Option<&'a Lists>,
    traced: bool,
) -> Result<Bound, BindError> {
    let mut bn = Binder::new(active, b, overrides, fallback, route)?;
    bn.lists = lists;
    bn.trace = traced.then(Box::default);
    let Some(code) = bn.source_of(route)? else {
        return Err(bn.fail(
            BindErrorKind::Missing(format!("the skin has no `{route}` route")),
            None,
        ));
    };
    bn.enter_route(code, route);
    let Some(r) = code.routes.get(route) else {
        return Err(bn.fail(
            BindErrorKind::Missing(format!("the skin has no `{route}` route")),
            None,
        ));
    };
    let root = bn.tree.root();
    let mut cx = bn.initial_cx();
    // The page: the root fills the window, and a style named `page` is its look.
    let page = bn.layers("page", None, &[])?;
    let c = bn.compute_with(&page, None, false, &[], &cx.text, |c| {
        c.layout.width = Dim::Pct(100.0);
        c.layout.height = Dim::Pct(100.0);
    })?;
    if let Some(node) = bn.tree.node_mut(root) {
        node.layout = c.layout;
        node.paint = c.paint;
    }
    cx.text = c.text;
    bn.children(&r.children, root, "root", &cx)?;
    Ok(Bound {
        tree: bn.tree,
        actions: bn.actions,
        widgets: bn.widgets,
        popups: bn.popups,
        fallbacks: bn.notes,
        provenance: bn.trace.map(|t| t.out),
    })
}

/// What the text properties and a `field` label hand down to children.
#[derive(Clone)]
pub(crate) struct Cx {
    pub text: TextCtx,
    /// The label of the enclosing `field`: the accessible name of its control.
    pub field: Option<Arc<str>>,
}

/// The state of the route being bound, kept while an `outlet` binds another.
pub(crate) struct SavedRoute<'a> {
    code: &'a SkinPackage,
    route_code: &'a SkinPackage,
    ui_manifest: &'a Manifest,
    ui: &'a Value,
    route: String,
    vars: Vec<Var>,
}

/// One variable in scope.
pub(crate) struct Var {
    pub name: String,
    pub value: Value,
    /// The variable is another name for this one of the app's data, and `value` is
    /// unused.
    pub alias: Option<String>,
    /// For a component parameter given an `args`: the terms, which a bare `{name}` in an
    /// action's `args` stands for (design 27 §5.7).
    pub terms: Option<Vec<BoundTerm>>,
    /// For a component parameter given an action's name: the action the loader resolved
    /// it to, which an `action="{name}"` in the body binds (design 26 S16). A variable
    /// that hides the parameter has none.
    pub spec: Option<&'static gogo_contract::ActionSpec>,
}

impl Var {
    /// A variable that is a plain value.
    pub(crate) fn plain(name: impl Into<String>, value: Value) -> Self {
        Var {
            name: name.into(),
            value,
            alias: None,
            terms: None,
            spec: None,
        }
    }
}

impl Var {
    /// Another name for `source`, which the app's data has.
    pub(crate) fn alias(name: impl Into<String>, source: String) -> Self {
        Var {
            alias: Some(source),
            ..Var::plain(name, Value::Null)
        }
    }
}

pub(crate) type R<T> = Result<T, BindError>;

pub(crate) struct Binder<'a> {
    b: &'a Binding<'a>,
    active: &'a SkinPackage,
    /// Token values to look in, the active skin's first.
    tokens: Vec<&'a tokens::Values>,
    overrides: &'a BTreeSet<String>,
    fallback: bool,
    /// The skin whose files the node being bound was read from: its styles, components
    /// and spans.
    pub(crate) code: &'a SkinPackage,
    /// The skin the route being bound belongs to: its manifest is `ui_manifest`.
    route_code: &'a SkinPackage,
    /// The skin whose `ui` variables the route reads: the route's own.
    ui_manifest: &'a Manifest,
    ui: &'a Value,
    /// The route being bound, for errors.
    pub(crate) route: String,
    pub(crate) vars: Vec<Var>,
    pub(crate) tree: Tree,
    pub(crate) actions: ActionTable,
    pub(crate) widgets: Vec<WidgetSlot>,
    pub(crate) popups: Vec<PopupSlot>,
    notes: Vec<Fallback>,
    nodes: usize,
    steps: usize,
    depth: usize,
    /// Which rows of virtualised lists to bind; `None` binds lists whole.
    lists: Option<&'a Lists>,
    /// How many `scroll` elements are around what is being bound.
    pub(crate) scrolls: usize,
    /// How many items of a virtualised list are around what is being bound.
    pub(crate) in_item: usize,
    /// Whether what is being bound is inside the `outlet`.
    pub(crate) in_outlet: bool,
    /// How many outlets have been bound.
    pub(crate) outlets: usize,
    /// Borrowed fragments already checked against this skin's manifest in this bind.
    checked: BTreeSet<String>,
    /// Set for a traced bind.
    pub(crate) trace: Option<Box<provenance::Trace>>,
}

/// The scope expressions are evaluated in: the binder's variables, the `ui` source and the
/// tokens, then the app's data.
pub(crate) struct EnvScope<'x> {
    data: &'x dyn Scope,
    vars: &'x [Var],
    ui: &'x Value,
    tokens: &'x [&'x tokens::Values],
    /// The parameters of the route being bound.
    pub(crate) route_params: &'static [SourceParam],
}

/// What an absent route parameter reads as: null when the contract declares it optional
/// for the route, and nothing otherwise.
pub(crate) fn absent_param(params: &[SourceParam], name: &str) -> Option<&'static Value> {
    static NULL: Value = Value::Null;
    params
        .iter()
        .any(|p| p.name == name && p.optional)
        .then_some(&NULL)
}

impl Scope for EnvScope<'_> {
    fn var(&self, name: &str) -> Option<&Value> {
        if let Some(v) = self.vars.iter().rev().find(|v| v.name == name) {
            return match &v.alias {
                Some(source) => self.data.var(source),
                None => Some(&v.value),
            };
        }
        if name == "ui" {
            return Some(self.ui);
        }
        self.data.var(name)
    }
    fn param(&self, name: &str) -> Option<&Value> {
        self.data
            .param(name)
            .or_else(|| absent_param(self.route_params, name))
    }
    fn token(&self, name: &str) -> Option<&Value> {
        self.tokens.iter().find_map(|t| t.get(name))
    }
    fn call(&self, name: &str, args: &[&Value]) -> Option<Result<Value, crate::expr::CallError>> {
        self.data.call(name, args)
    }
    fn now(&self) -> f64 {
        self.data.now()
    }
}

impl<'a> Binder<'a> {
    fn new(
        active: &'a SkinPackage,
        b: &'a Binding<'a>,
        overrides: &'a BTreeSet<String>,
        fallback: bool,
        route: &str,
    ) -> R<Self> {
        let mut tokens: Vec<&'a tokens::Values> = Vec::new();
        match active.tables().tokens.get(&b.mode) {
            Some(t) => tokens.push(t),
            None => {
                return Err(BindError::new(BindErrorKind::NoMode(b.mode), route, None));
            }
        }
        if let Some(d) = b.default
            && let Some(t) = d.skin.tables().tokens.get(&b.mode)
        {
            tokens.push(t);
        }
        let mut root = Node::container();
        root.a11y = A11y {
            role: Role::Group,
            name: Some(active.manifest.title.clone()),
            ..A11y::default()
        };
        Ok(Binder {
            b,
            active,
            tokens,
            overrides,
            fallback,
            code: active,
            route_code: active,
            ui_manifest: &active.manifest,
            ui: b.ui.as_value(),
            route: route.to_owned(),
            vars: Vec::new(),
            tree: Tree::new("root", root),
            actions: ActionTable::default(),
            widgets: Vec::new(),
            popups: Vec::new(),
            notes: Vec::new(),
            nodes: 1,
            steps: 0,
            depth: 0,
            lists: None,
            scrolls: 0,
            in_item: 0,
            in_outlet: false,
            outlets: 0,
            checked: BTreeSet::new(),
            trace: None,
        })
    }

    /// The skin a route is read from: the default skin's when this page already fell back
    /// on it, or when the active skin doesn't have it (and falling back is on).
    pub(crate) fn source_of(&mut self, route: &str) -> R<Option<&'a SkinPackage>> {
        let default = self.b.default.map(|d| d.skin);
        if self.overrides.contains(route) {
            return Ok(default);
        }
        if self.active.routes.contains_key(route) {
            return Ok(Some(self.active));
        }
        if !self.fallback {
            return Ok(None);
        }
        let Some(d) = default else { return Ok(None) };
        let why = if self.active.report().failed_routes().contains(&route) {
            FallbackWhy::FailedToLoad
        } else {
            FallbackWhy::NotCovered
        };
        if !self.notes.iter().any(|n| n.route == route) {
            self.notes.push(Fallback {
                route: route.to_owned(),
                why,
            });
        }
        Ok(Some(d))
    }

    /// Switch to the files and `ui` state of the skin that owns `route`.
    pub(crate) fn enter_route(&mut self, code: &'a SkinPackage, route: &str) {
        self.code = code;
        self.route_code = code;
        self.ui_manifest = &code.manifest;
        self.route = route.to_owned();
        self.ui = match self.b.default {
            Some(d) if !std::ptr::eq(code, self.active) && std::ptr::eq(code, d.skin) => {
                d.ui.as_value()
            }
            _ => self.b.ui.as_value(),
        };
    }

    /// What `enter_route` replaces, to put back after an embedded route.
    pub(crate) fn save_route(&mut self) -> SavedRoute<'a> {
        SavedRoute {
            code: self.code,
            route_code: self.route_code,
            ui_manifest: self.ui_manifest,
            ui: self.ui,
            route: self.route.clone(),
            vars: std::mem::take(&mut self.vars),
        }
    }

    pub(crate) fn restore_route(&mut self, s: SavedRoute<'a>) {
        self.code = s.code;
        self.route_code = s.route_code;
        self.ui_manifest = s.ui_manifest;
        self.ui = s.ui;
        self.route = s.route;
        self.vars = s.vars;
    }

    pub(crate) fn initial_cx(&self) -> Cx {
        let mut text = TextCtx::default();
        let scope = self.scope();
        if let Some(Value::Map(m)) = scope.token("font.body") {
            if let Some(fam) = m.get("family") {
                text.set_family(&fam.display());
            }
            if let Some(Value::Num(n)) = m.get("size") {
                text.size = *n as f32;
            }
            if let Some(Value::Num(n)) = m.get("weight") {
                text.weight = *n as f32;
            }
        }
        if let Some(Value::Str(c)) = scope.token("text-hi")
            && let Some(c) = crate::tokens::Rgba::parse(c)
        {
            text.color = Color::rgba(c.r, c.g, c.b, c.a);
        }
        if let Some(Value::Map(m)) = scope.token("font.mono")
            && let Some(fam) = m.get("family")
        {
            text.mono = Some(Arc::from(fam.display().as_str()));
        }
        Cx { text, field: None }
    }

    pub(crate) fn scope(&self) -> EnvScope<'_> {
        EnvScope {
            data: self.b.scope,
            vars: &self.vars,
            ui: self.ui,
            tokens: &self.tokens,
            route_params: route_spec(&self.route).map_or(&[], |r| r.params),
        }
    }

    // ---- errors ----

    pub(crate) fn fail(&self, kind: BindErrorKind, at: Option<Span>) -> BindError {
        BindError::new(kind, &self.route, at.map(|s| (s, self.code.sources())))
    }

    pub(crate) fn property(&self, name: &str, why: impl Into<String>, at: Span) -> BindError {
        self.fail(
            BindErrorKind::Property {
                name: name.to_owned(),
                why: why.into(),
            },
            Some(at),
        )
    }

    pub(crate) fn action_problem(&self, p: Problem, at: Span) -> BindError {
        match p {
            Problem::Masked(m) => self.fail(BindErrorKind::Masked(m), Some(at)),
            Problem::Other(m) => self.fail(BindErrorKind::Action(m), Some(at)),
        }
    }

    // ---- evaluation ----

    pub(crate) fn eval(&self, t: &Template, at: Span) -> R<Value> {
        let sc = self.scope();
        t.eval(&sc)
            .map(std::borrow::Cow::into_owned)
            .map_err(|e| self.fail(e.into(), Some(at)))
    }

    /// The value of a node's argument or property.
    pub(crate) fn val(&self, v: &Val, at: Span) -> R<Value> {
        match v {
            Val::Text(t) => self.eval(t, at),
            Val::Num(n) => Ok(Value::Num(*n)),
            Val::Bool(b) => Ok(Value::Bool(*b)),
            Val::Null => Ok(Value::Null),
            Val::Token(name) => self.token(name, at),
            Val::Terms(_) | Val::Invalid => Err(self.fail(
                BindErrorKind::Missing("this value has no meaning here".to_owned()),
                Some(at),
            )),
        }
    }

    /// The value of property `name` of `n`, if it is there.
    pub(crate) fn prop(&self, n: &AstNode, name: &str) -> R<Option<Value>> {
        n.prop(name)
            .map(|p| self.val(&p.arg.val, p.arg.span))
            .transpose()
    }

    /// The text of property `name`, or `None` when it is missing or null.
    pub(crate) fn prop_text(&self, n: &AstNode, name: &str) -> R<Option<String>> {
        Ok(self
            .prop(n, name)?
            .filter(|v| !matches!(v, Value::Null))
            .map(|v| v.display()))
    }

    /// The text of the node's first argument.
    pub(crate) fn arg_text(&self, n: &AstNode) -> R<String> {
        match n.args.first() {
            Some(a) => Ok(self.val(&a.val, a.span)?.display()),
            None => Ok(String::new()),
        }
    }

    pub(crate) fn token(&self, name: &str, at: Span) -> R<Value> {
        self.tokens
            .iter()
            .find_map(|t| t.get(name))
            .cloned()
            .ok_or_else(|| {
                self.fail(
                    BindErrorKind::Missing(format!("unknown token `@{name}`")),
                    Some(at),
                )
            })
    }

    /// A style property's operands as values.
    pub(crate) fn operands(&self, ops: &[Operand], at: Span) -> R<Vec<Value>> {
        ops.iter()
            .map(|o| match o {
                Operand::Num(n) => Ok(Value::Num(*n)),
                Operand::Word(w) => Ok(Value::Str(w.clone())),
                Operand::Token(t) => self.token(t, at),
                Operand::Bool(b) => Ok(Value::Bool(*b)),
                Operand::Null => Ok(Value::Null),
                Operand::Dyn(t) => self.eval(t, at),
            })
            .collect()
    }

    // ---- the tree ----

    pub(crate) fn add(&mut self, parent: NodeId, key: &str, node: Node, at: Span) -> R<NodeId> {
        self.nodes += 1;
        if self.nodes > MAX_BOUND_NODES {
            return Err(self.fail(BindErrorKind::TooManyNodes(MAX_BOUND_NODES), Some(at)));
        }
        self.tree
            .add_child(parent, key, node)
            .map_err(|e| self.fail(BindErrorKind::Tree(e.to_string()), Some(at)))
    }

    pub(crate) fn enter(&mut self, at: Span) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.fail(BindErrorKind::TooDeep(MAX_DEPTH), Some(at)));
        }
        Ok(())
    }

    pub(crate) fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// An action's id, after the call is made.
    pub(crate) fn register(&mut self, call: ActionCall, at: Span) -> R<ActionRef> {
        self.actions
            .push(call)
            .ok_or_else(|| self.fail(BindErrorKind::TooManyNodes(MAX_BOUND_NODES), Some(at)))
    }

    /// Whether a style named `layers` defines `state`.
    pub(crate) fn has_state(&self, layers: &[String], state: State) -> bool {
        layers.iter().any(|l| {
            self.code
                .styles
                .get(l)
                .is_some_and(|s| s.states.iter().any(|(st, _)| *st == state))
        })
    }

    /// The manifest whose `ui` variables the route being bound reads.
    pub(crate) fn ui_manifest(&self) -> &'a Manifest {
        self.ui_manifest
    }

    pub(crate) fn default_skin(&self) -> Option<&'a SkinPackage> {
        self.b.default.map(|d| d.skin)
    }

    pub(crate) fn checked(&mut self) -> &mut BTreeSet<String> {
        &mut self.checked
    }

    pub(crate) fn current_ui(&self) -> &'a Value {
        self.ui
    }

    /// The skin whose manifest the route being bound is checked against.
    pub(crate) fn route_code(&self) -> &'a SkinPackage {
        self.route_code
    }

    /// One step of the bind budget.
    pub(crate) fn step(&mut self, at: Span) -> R<()> {
        self.steps += 1;
        if self.steps > MAX_BIND_STEPS {
            return Err(self.fail(BindErrorKind::TooManySteps(MAX_BIND_STEPS), Some(at)));
        }
        Ok(())
    }
}

/// Make a `TermVal::Value` term.
pub(crate) fn term(name: Option<String>, v: Value) -> BoundTerm {
    BoundTerm {
        name,
        val: TermVal::Value(v),
    }
}

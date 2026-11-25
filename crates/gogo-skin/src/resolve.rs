//! The contract check (design 27 §7 and §9 step 2; design 26 S5, S14, S16, S21): resolves
//! every name a skin writes to its `&'static` contract spec, and checks the fields and the
//! types an element needs.
//!
//! The format-level pass (`elements`) checks structure and keeps names as strings. This
//! pass walks the same tree once more with the contract at hand:
//!
//! - **actions**: `action`, `link-action` and a widget's `for` resolve to an
//!   [`ActionSpec`](gogo_contract::ActionSpec) kept in the node's [`Resolved`]; the
//!   `args` terms are matched to the action's parameters (unknown, repeated, extra and
//!   missing ones are errors) and typed against them;
//! - **routes** in `nav.go` and the sections of a `tabs` element;
//! - **widgets**: part names, variant values and placements, and the parts that can't be
//!   hidden (S5);
//! - **S21**: `settings.set` takes a literal key from the skin-settable list;
//! - **expressions** (see [`crate::typing`]): every name, every field path and the types
//!   an element needs.
//!
//! A component's body is checked twice. Once on its own, where its parameters have no
//! type, and once at each `use` in a route, where each parameter has the type of the
//! argument given there (design 27 §7): that second walk reports only what depends on a
//! parameter's type, and points at the argument.

use std::collections::{BTreeMap, BTreeSet};

use gogo_contract::actions::{self, Param, ParamType};
use gogo_contract::widgets::{Depth, WIDGETS};
use gogo_contract::{ActionSpec, AppWidget, FieldType, RouteSpec, routes, settings};

use crate::ast::{Arg, ArgRef, BoundAction, BoundArg, Node, Val};
use crate::diag::{Code, Collector};
use crate::expr::Template;
use crate::manifest::{Manifest, UiType};
use crate::names::suggest;
use crate::package::{Component, Route};
use crate::source::Span;
use crate::typing::Ty;

/// How many components deep a `use` is followed at one route, and how many nodes one
/// route's expansion visits. Both are far above what the limits of design 27 §8 allow a
/// valid skin, so they only stop a skin that is already failing.
const MAX_USE_DEPTH: usize = 48;
const MAX_VISITS: usize = 200_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pass {
    /// A route's own tree.
    Route,
    /// A component's body on its own: its parameters have no type.
    Generic,
    /// A component's body at a `use`: only what depends on a parameter's type.
    Instance,
}

/// What an element may be inside: the widget a `part` belongs to.
#[derive(Clone, Copy, Default)]
pub(crate) struct Parent {
    widget: Option<&'static AppWidget>,
}

/// What a `use` wrote for one component parameter, as far as the checks of the component's
/// body can know it (design 27 §5.3, §5.7).
#[derive(Clone)]
enum Given {
    /// Plain text: an action name, or a literal for an argument.
    Text(String),
    /// The terms of an `args`, each with the type it has where the `use` wrote it. A bare
    /// `{p}` term in the body stands for all of them.
    Terms(Vec<(crate::ast::Term, Ty)>),
}

pub(crate) struct Checker<'a> {
    pub col: &'a mut Collector,
    pub manifest: &'a Manifest,
    pub pass: Pass,
    pub route: Option<&'static RouteSpec>,
    /// Variables in scope, innermost last.
    pub env: Vec<(String, Ty)>,
    /// The item types of the `where` predicates being checked, innermost last.
    pub items: Vec<Ty>,
    pub words: &'a BTreeSet<String>,
    params: &'a BTreeMap<String, Vec<String>>,
    comps: Option<&'a BTreeMap<String, Component>>,
    comp: Option<String>,
    /// What each component parameter was given at the `use` being followed.
    given: BTreeMap<String, Given>,
    /// How many leading entries of `env` are the component's own parameters. A name
    /// found further in is a `let`, `each` or input variable that shadows a parameter,
    /// and then `given` no longer applies to it.
    nparams: usize,
    /// The parameter a dynamically named action came from: what its argument errors
    /// depend on.
    dep: Option<Ty>,
    depth: usize,
    visits: usize,
}

/// Check every component on its own.
pub(crate) fn components(
    col: &mut Collector,
    manifest: &Manifest,
    comps: &mut BTreeMap<String, Component>,
) {
    let words = enum_words(manifest);
    let params: BTreeMap<String, Vec<String>> = comps
        .iter()
        .map(|(n, c)| (n.clone(), c.params.clone()))
        .collect();
    for c in comps.values_mut() {
        let mut ck = Checker::new(col, manifest, &words, &params, None, Pass::Generic, None);
        ck.comp = Some(c.name.clone());
        ck.env = c
            .params
            .iter()
            .map(|p| (p.clone(), Ty::default()))
            .collect();
        ck.nparams = ck.env.len();
        ck.nodes(&mut c.body, Parent::default());
    }
}

/// Check every route's tree, following each `use` into its component with the types of
/// the arguments given. `follow` is false when the components loop or are too large to
/// expand.
pub(crate) fn routes(
    col: &mut Collector,
    manifest: &Manifest,
    comps: &BTreeMap<String, Component>,
    routes: &mut BTreeMap<String, Route>,
    follow: bool,
    mut enter: impl FnMut(&mut Collector, &str),
) {
    let words = enum_words(manifest);
    let params: BTreeMap<String, Vec<String>> = comps
        .iter()
        .map(|(n, c)| (n.clone(), c.params.clone()))
        .collect();
    for (name, route) in routes.iter_mut() {
        enter(col, name);
        let mut ck = Checker::new(
            col,
            manifest,
            &words,
            &params,
            follow.then_some(comps),
            Pass::Route,
            Some(route.spec),
        );
        ck.nodes(&mut route.children, Parent::default());
    }
}

/// Every word an expression may use bare, besides variables: a status, an OS, a word of
/// a fixed set in a source's shape (design 26 §2, §3), and, from the skin's own manifest,
/// the words of its `enum` variables and the contract's section names.
fn enum_words(manifest: &Manifest) -> BTreeSet<String> {
    use gogo_contract::{Os, Status, shapes};
    fn walk(t: &FieldType, seen: &mut BTreeSet<&'static str>, out: &mut BTreeSet<String>) {
        match t {
            FieldType::OneOf(words) => out.extend(words.iter().map(|w| (*w).to_owned())),
            FieldType::Masked(t) | FieldType::List(t) | FieldType::Option(t) => walk(t, seen, out),
            FieldType::Record(s) if seen.insert(s.name) => {
                for f in s.all_fields() {
                    walk(&f.ty, seen, out);
                }
            }
            _ => {}
        }
    }
    let mut out: BTreeSet<String> = BTreeSet::new();
    out.extend(Status::ALL.iter().map(|s| s.name().to_owned()));
    out.extend(Os::ALL.iter().map(|o| o.name().to_owned()));
    let mut seen = BTreeSet::new();
    for s in shapes::SHAPES {
        walk(&FieldType::Record(s), &mut seen, &mut out);
    }
    for r in routes::ROUTES {
        out.extend(r.sections.iter().map(|s| (*s).to_owned()));
    }
    for v in &manifest.ui {
        if let UiType::Enum(words) = &v.ty {
            out.extend(words.iter().cloned());
        }
    }
    out
}

impl<'a> Checker<'a> {
    fn new(
        col: &'a mut Collector,
        manifest: &'a Manifest,
        words: &'a BTreeSet<String>,
        params: &'a BTreeMap<String, Vec<String>>,
        comps: Option<&'a BTreeMap<String, Component>>,
        pass: Pass,
        route: Option<&'static RouteSpec>,
    ) -> Self {
        Checker {
            col,
            manifest,
            pass,
            route,
            env: Vec::new(),
            items: Vec::new(),
            words,
            params,
            comps,
            comp: None,
            given: BTreeMap::new(),
            nparams: 0,
            dep: None,
            depth: 0,
            visits: 0,
        }
    }

    /// Report an error. In an instance walk only what depends on a parameter's type is
    /// reported, at the argument that gave it.
    pub fn emit(
        &mut self,
        ty: Option<&Ty>,
        code: Code,
        span: Span,
        msg: String,
        help: Option<String>,
    ) {
        let (mut span, mut msg) = (span, msg);
        if self.pass == Pass::Instance {
            let Some(t) = ty.filter(|t| t.param).or(self.dep.as_ref()).copied() else {
                return;
            };
            let real = self.col.sources.location(span);
            if let Some(o) = t.origin {
                span = o;
            }
            let comp = self.comp.as_deref().unwrap_or_default();
            msg = format!("{msg} (in component `{comp}`, at {real})");
        }
        let f = self.col.error(code, span, msg);
        if let Some(h) = help {
            let _ = f.help(h);
        }
    }

    // ---- trees ----

    pub fn nodes(&mut self, list: &mut [Node], parent: Parent) {
        let mark = self.env.len();
        for n in list {
            if self.pass == Pass::Instance {
                self.visits += 1;
                if self.visits > MAX_VISITS {
                    break;
                }
            }
            self.node(n, parent);
        }
        self.env.truncate(mark);
    }

    fn node(&mut self, n: &mut Node, parent: Parent) {
        match n.name.as_str() {
            "let" => self.let_(n),
            "each" => self.each(n),
            "switch" => self.switch(n),
            "case" | "else" => self.nodes(&mut n.children, Parent::default()),
            "use" => self.use_(n),
            "part" => self.part(n, parent),
            "data-table" => self.data_table(n),
            name => {
                if let Some(w) = WIDGETS
                    .iter()
                    .find(|w| w.name == name && w.depth == Depth::B)
                {
                    self.widget(n, w);
                } else {
                    self.element(n);
                }
            }
        }
    }

    /// Check the templates among a node's arguments.
    fn arg_templates(&mut self, n: &mut Node) {
        for i in 0..n.args.len() {
            let loc = self.loc_arg(&n.args[i]);
            if let Val::Text(t) = &mut n.args[i].val {
                self.template(t, &loc);
            }
        }
    }

    // ---- ordinary elements ----

    fn element(&mut self, n: &mut Node) {
        self.arg_templates(n);
        self.props(n);
        self.actions(n);
        if n.name == "tabs" {
            self.tabs(n);
        }
        self.nodes(&mut n.children, Parent::default());
    }

    /// The templates among a node's properties, with the type each element needs.
    fn props(&mut self, n: &mut Node) {
        let widget = WIDGETS
            .iter()
            .find(|w| w.name == n.name && w.depth == Depth::B);
        for i in 0..n.props.len() {
            let name = n.props[i].name.clone();
            if matches!(
                name.as_str(),
                "action" | "link-action" | "for" | "args" | "link-args"
            ) || (n.name == "tabs" && matches!(name.as_str(), "bind" | "sections"))
                || widget.is_some_and(|w| w.variant(&name).is_some() || name == "placement")
                || !crate::elements::takes_prop(&n.name, &name)
            {
                continue;
            }
            let span = n.props[i].arg.span;
            let loc = self.loc_arg(&n.props[i].arg);
            let Val::Text(t) = &mut n.props[i].arg.val else {
                continue;
            };
            if matches!(t, Template::Text(_)) {
                continue;
            }
            let ty = self.template(t, &loc);
            let single = matches!(t, Template::Expr(_));
            if !single {
                continue;
            }
            match name.as_str() {
                "when" | "on" | "highlight" => self.need_testable(&ty, &name, span),
                "rows" => self.need_list(&ty, &name, span),
                "progress" => self.need_fraction(&ty, &name, span),
                _ => {}
            }
        }
    }

    // ---- control flow ----

    fn let_(&mut self, n: &mut Node) {
        for i in 0..n.props.len() {
            let loc = self.loc_arg(&n.props[i].arg);
            let name = n.props[i].name.clone();
            let ty = match &mut n.props[i].arg.val {
                Val::Text(t) => {
                    let ty = self.template(t, &loc);
                    if matches!(t, Template::Expr(_)) {
                        ty
                    } else {
                        Ty::text()
                    }
                }
                Val::Num(_) => Ty::of(FieldType::Number),
                Val::Bool(_) => Ty::of(FieldType::Bool),
                _ => Ty::default(),
            };
            // Bound for the rest of the block: `nodes` drops it with the block.
            self.env.push((name, ty));
        }
    }

    fn each(&mut self, n: &mut Node) {
        let mut item = Ty::default();
        if let Some(i) = n.props.iter().rposition(|p| p.name == "in") {
            let span = n.props[i].arg.span;
            let loc = self.loc_arg(&n.props[i].arg);
            if let Val::Text(t) = &mut n.props[i].arg.val {
                let ty = self.template(t, &loc);
                if matches!(t, Template::Expr(_)) {
                    self.need_list(&ty, "each", span);
                }
                item = ty.with(ty.ft.and_then(|f| f.item()).copied());
            }
        }
        let var = n.plain_arg(0).map(str::to_owned);
        let mark = self.env.len();
        if let Some(v) = var {
            self.env.push((v, item));
        }
        self.nodes(&mut n.children, Parent::default());
        self.env.truncate(mark);
    }

    fn switch(&mut self, n: &mut Node) {
        self.arg_templates(n);
        self.nodes(&mut n.children, Parent::default());
    }

    fn data_table(&mut self, n: &mut Node) {
        self.arg_templates(n);
        let mut row = Ty::default();
        // `rows` is evaluated outside the table, so before `row` is bound.
        if let Some(i) = n.props.iter().rposition(|p| p.name == "rows") {
            let span = n.props[i].arg.span;
            let loc = self.loc_arg(&n.props[i].arg);
            if let Val::Text(t) = &mut n.props[i].arg.val {
                let ty = self.template(t, &loc);
                if matches!(t, Template::Expr(_)) {
                    self.need_list(&ty, "rows", span);
                }
                row = ty.with(ty.ft.and_then(|f| f.item()).copied());
            }
        }
        let mark = self.env.len();
        self.env.push(("row".to_owned(), row));
        self.props_except(n, "rows");
        self.nodes(&mut n.children, Parent::default());
        self.env.truncate(mark);
    }

    /// `props`, skipping one property that was checked already.
    fn props_except(&mut self, n: &mut Node, skip: &str) {
        let held = n.props.iter().position(|p| p.name == skip);
        let Some(i) = held else {
            self.props(n);
            return;
        };
        // Hide it from `props` by moving it out for the duration.
        let p = n.props.remove(i);
        self.props(n);
        n.props.insert(i, p);
    }

    // ---- components ----

    fn use_(&mut self, n: &mut Node) {
        // `use default "fragment"` and `use "component"`.
        let comp = match n.args.as_slice() {
            [a] => a.val.plain().map(str::to_owned),
            _ => None,
        };
        self.arg_templates(n);
        let mut binds: Vec<(String, Ty)> = Vec::new();
        let mut givens: Vec<(String, Given)> = Vec::new();
        for i in 0..n.props.len() {
            let name = n.props[i].name.clone();
            let span = n.props[i].arg.span;
            let loc = self.loc_arg(&n.props[i].arg);
            let ty = match &mut n.props[i].arg.val {
                Val::Text(t) => {
                    let ty = self.template(t, &loc);
                    match t {
                        Template::Expr(e) => {
                            // `action="{action}"` passed on: the name it stands for.
                            if let crate::expr::Expr::Name { name: q, .. } = e
                                && let Some(Given::Text(s)) = self.param_given(q)
                            {
                                givens.push((name.clone(), Given::Text(s.clone())));
                            }
                            ty
                        }
                        Template::Text(s) => {
                            givens.push((name.clone(), Given::Text(s.clone())));
                            Ty::text()
                        }
                        Template::Interp(_) => Ty::text().not_plain(),
                    }
                }
                // A parameter named `args` is read as the terms of an action's `args`.
                Val::Terms(terms) => {
                    let mut tys = Vec::new();
                    self.with_inputs("use", |ck| {
                        for t in terms.iter_mut() {
                            let loc = ck.loc_term(t);
                            tys.push(ck.template(&mut t.value, &loc));
                        }
                    });
                    // A bare `{p}` here, in a component that forwards its own parameter
                    // to another component, stands for what that parameter was given.
                    let list = self.splice(terms, &tys);
                    let ty = match list.as_slice() {
                        [(t, ty, _)] if matches!(t.value, Template::Expr(_)) => *ty,
                        [_] => Ty::text(),
                        _ => Ty::text().not_plain(),
                    };
                    let kept = list
                        .into_iter()
                        .map(|(t, ty, _)| {
                            let origin = ty.origin.or(Some(span));
                            (
                                t,
                                Ty {
                                    param: true,
                                    origin,
                                    ..ty
                                },
                            )
                        })
                        .collect();
                    givens.push((name.clone(), Given::Terms(kept)));
                    ty
                }
                Val::Num(_) => Ty::of(FieldType::Number),
                Val::Bool(_) => Ty::of(FieldType::Bool),
                _ => Ty::default(),
            };
            if name == "when" {
                self.need_testable(&ty, "when", span);
            } else if name != "id" {
                binds.push((
                    name,
                    Ty {
                        param: true,
                        // A `use` may leave a parameter out, which makes it null.
                        nullable: true,
                        origin: ty.origin.or(Some(span)),
                        ..ty
                    },
                ));
            }
        }
        let Some(comp) = comp else { return };
        if self.depth >= MAX_USE_DEPTH {
            return;
        }
        let Some(comps) = self.comps else { return };
        let Some(body) = comps.get(&comp).map(|c| c.body.clone()) else {
            return;
        };
        let declared = self.params.get(&comp).cloned().unwrap_or_default();
        // The body starts from its own parameters; a parameter not given is a null.
        let env: Vec<(String, Ty)> = declared
            .iter()
            .map(|p| {
                let ty = binds
                    .iter()
                    .find(|(n, _)| n == p)
                    .map(|(_, t)| *t)
                    .unwrap_or_default();
                (p.clone(), ty)
            })
            .collect();
        let given: BTreeMap<String, Given> = givens
            .into_iter()
            .filter(|(name, _)| declared.contains(name))
            .collect();
        let saved = (
            std::mem::replace(&mut self.env, env),
            std::mem::take(&mut self.items),
            self.pass,
            self.comp.replace(comp),
            std::mem::replace(&mut self.given, given),
            std::mem::replace(&mut self.nparams, declared.len()),
        );
        self.pass = Pass::Instance;
        self.depth += 1;
        let mut body = body;
        self.nodes(&mut body, Parent::default());
        self.depth -= 1;
        self.env = saved.0;
        self.items = saved.1;
        self.pass = saved.2;
        self.comp = saved.3;
        self.given = saved.4;
        self.nparams = saved.5;
    }

    // ---- app widgets ----

    fn widget(&mut self, n: &mut Node, w: &'static AppWidget) {
        n.resolved.widget = Some(w);
        self.props(n);
        for i in 0..n.props.len() {
            let name = n.props[i].name.clone();
            let arg = &n.props[i].arg;
            if let Some(v) = w.variant(&name) {
                let (span, text) = (self.inner_span(arg, 0, None), arg.val.plain());
                match text.and_then(|t| v.values.iter().find(|x| **x == t)) {
                    Some(value) => n.resolved.variants.push((v, value)),
                    None => {
                        let shown = text.unwrap_or("an expression");
                        let msg =
                            format!("`{shown}` is not a value of `{name}` on `{}` (S16)", w.name);
                        let help = format!("`{name}` takes {}", list(v.values));
                        self.emit(None, Code::UnknownVariant, span, msg, Some(help));
                    }
                }
            } else if name == "placement" {
                self.placement(n, i, w);
            }
        }
        self.actions(n);
        self.nodes(&mut n.children, Parent { widget: Some(w) });
    }

    fn placement(&mut self, n: &Node, i: usize, w: &'static AppWidget) {
        let arg = &n.props[i].arg;
        let span = self.inner_span(arg, 0, None);
        let text = arg.val.plain();
        let allowed: Vec<&str> = w
            .placements
            .iter()
            .map(|p| match p {
                gogo_contract::Placement::Inline => "inline",
                gogo_contract::Placement::Modal => "modal",
            })
            .collect();
        if text.is_some_and(|t| allowed.contains(&t)) {
            return;
        }
        let msg = format!(
            "`{}` is not a placement of `{}` (S16)",
            text.unwrap_or("an expression"),
            w.name
        );
        let help = if allowed.is_empty() {
            format!("the app places `{}`; it takes no `placement`", w.name)
        } else {
            format!("`placement` takes {}", list(&allowed))
        };
        self.emit(None, Code::UnknownVariant, span, msg, Some(help));
    }

    fn part(&mut self, n: &mut Node, parent: Parent) {
        let Some(w) = parent.widget else { return };
        let Some(name) = n.plain_arg(0).map(str::to_owned) else {
            return;
        };
        let span = self.inner_span(&n.args[0], 0, None);
        let Some(part) = w.part(&name) else {
            let names: Vec<&str> = w.parts.iter().map(|p| p.name).collect();
            let help = if names.is_empty() {
                format!("`{}` has no parts", w.name)
            } else {
                match suggest(&name, names.iter().copied()) {
                    Some(s) => format!("did you mean `{s}`?"),
                    None => format!("the parts of `{}` are {}", w.name, names.join(", ")),
                }
            };
            self.emit(
                None,
                Code::UnknownPart,
                span,
                format!("`{}` has no part `{name}` (S16)", w.name),
                Some(help),
            );
            return;
        };
        n.resolved.part = Some((w, part));
        if let Some(h) = n.prop("hidden")
            && matches!(h.arg.val, Val::Bool(true))
            && !part.optional
        {
            let at = h.arg.span;
            self.emit(
                None,
                Code::HiddenPart,
                at,
                format!(
                    "the `{name}` part of `{}` can't be hidden (S5)",
                    w.name
                ),
                Some("only an optional part may be hidden: the plan's footnote and its confirm and cancel buttons are always drawn".to_owned()),
            );
        }
    }

    // ---- tabs ----

    fn tabs(&mut self, n: &mut Node) {
        // `bind="ui.tab"`: a `section` variable of the manifest.
        if let Some(p) = n.prop("bind")
            && let Some(bind) = p.arg.val.plain()
        {
            let span = self.inner_span(&p.arg, 0, None);
            let var = bind.strip_prefix("ui.").and_then(|v| self.ui_var(v));
            match var {
                Some(v) if v.ty == UiType::Section => {}
                Some(v) => {
                    let msg = format!("`ui.{}` is not a `section` variable", v.name);
                    self.emit(
                        None,
                        Code::TypeMismatch,
                        span,
                        msg,
                        Some("`tabs` binds a variable declared `type=\"section\"`".to_owned()),
                    );
                }
                None => {
                    let vars: Vec<&str> =
                        self.manifest.ui.iter().map(|v| v.name.as_str()).collect();
                    let help = if vars.is_empty() {
                        "the manifest declares no `ui` variables".to_owned()
                    } else {
                        format!("the variables are {}", vars.join(", "))
                    };
                    self.emit(
                        None,
                        Code::UnknownField,
                        span,
                        format!("`{bind}` is not a `ui` variable of this skin"),
                        Some(help),
                    );
                }
            }
        }
        // `sections="a b"`: sections of the route the element is in.
        if self.pass != Pass::Route {
            return;
        }
        let Some(route) = self.route else { return };
        let Some(i) = n.props.iter().rposition(|p| p.name == "sections") else {
            return;
        };
        let arg = &n.props[i].arg;
        let Some(text) = arg.val.plain() else { return };
        let words: Vec<(usize, &str)> = word_offsets(text);
        let mut found = Vec::new();
        let mut errors = Vec::new();
        for (at, w) in words {
            match route.sections.iter().find(|s| **s == w) {
                Some(s) => found.push(*s),
                None => errors.push((self.inner_span(arg, at, Some(w.len())), w.to_owned())),
            }
        }
        for (span, w) in errors {
            let help = if route.sections.is_empty() {
                format!("the `{}` route has no sections", route.name)
            } else {
                match suggest(&w, route.sections.iter().copied()) {
                    Some(s) => format!("did you mean `{s}`?"),
                    None => format!(
                        "the sections of `{}` are {}",
                        route.name,
                        route.sections.join(", ")
                    ),
                }
            };
            self.emit(
                None,
                Code::UnknownSection,
                span,
                format!("`{}` has no section `{w}` (S16)", route.name),
                Some(help),
            );
        }
        n.resolved.sections = found;
    }

    pub fn ui_var(&self, name: &str) -> Option<&'a crate::manifest::UiVar> {
        self.manifest.ui.iter().find(|v| v.name == name)
    }

    // ---- actions ----

    /// Resolve `action`, `link-action` and a widget's `for`, with their arguments.
    fn actions(&mut self, n: &mut Node) {
        let pairs: [(&str, &str); 3] = [
            ("action", "args"),
            ("link-action", "link-args"),
            ("for", "args"),
        ];
        let mut args_used = [false; 2];
        for (act, args) in pairs {
            let Some(p) = n.prop(act) else { continue };
            args_used[usize::from(args == "link-args")] = true;
            let mut dynamic: Option<Ty> = None;
            let name = match p.arg.val.plain() {
                Some(name) => name.to_owned(),
                // `action="{param}"` of a component: the name each `use` gave it.
                None => match self.passed_action(&p.arg.val) {
                    Some((name, ty)) => {
                        dynamic = Some(ty);
                        name
                    }
                    // Anything else is the format-level pass's error, except a name that
                    // a `let` or `each` has taken over from the parameter: the format
                    // pass can't tell, and the action would not be a fixed one.
                    None => {
                        if let Val::Text(Template::Expr(crate::expr::Expr::Name { name, .. })) =
                            &p.arg.val
                            && self
                                .env
                                .iter()
                                .rposition(|(x, _)| x == name)
                                .is_some_and(|i| i >= self.nparams)
                            && self.comp.is_some()
                        {
                            let at = self.inner_span(&p.arg, 0, None);
                            self.emit(
                                None,
                                Code::ActionDynamic,
                                at,
                                format!("`{act}` is `{{{name}}}`, a variable that hides the component parameter of that name (S16)"),
                                Some("a skin binds actions by fixed name".to_owned()),
                            );
                        }
                        self.check_terms_only(n, args);
                        continue;
                    }
                },
            };
            let at = self.inner_span(&p.arg, 0, None);
            let Some(spec) = actions::skin_action(&name) else {
                // A plain name in the element was reported by the format-level pass.
                if let Some(ty) = dynamic {
                    let names = actions::ACTIONS.iter().map(|x| x.name);
                    let help = match suggest(&name, names) {
                        Some(s) => format!("did you mean `{s}`?"),
                        None => "design 26 §4 lists the actions a skin may bind".to_owned(),
                    };
                    self.emit(
                        Some(&ty),
                        Code::UnknownAction,
                        at,
                        format!("unknown action `{name}` (S16)"),
                        Some(help),
                    );
                }
                self.check_terms_only(n, args);
                continue;
            };
            self.dep = dynamic;
            let bound = self.bind_action(n, act, args, spec, at);
            self.dep = None;
            n.resolved.actions.push(bound);
        }
        for (i, args) in ["args", "link-args"].into_iter().enumerate() {
            if !args_used[i]
                && let Some(p) = n.prop(args)
            {
                let at = p.name_span;
                let act = if args == "args" {
                    "an `action`"
                } else {
                    "a `link-action`"
                };
                self.check_terms_only(n, args);
                self.emit(
                    None,
                    Code::ActionArgs,
                    at,
                    format!("`{args}` without {act} to take them"),
                    None,
                );
            }
        }
    }

    /// The action a component's `action="{param}"` stands for at the `use` being
    /// followed, with the parameter's type. Only an instance walk knows it.
    fn passed_action(&self, v: &Val) -> Option<(String, Ty)> {
        let Val::Text(Template::Expr(crate::expr::Expr::Name { name, .. })) = v else {
            return None;
        };
        let Given::Text(lit) = self.param_given(name)? else {
            return None;
        };
        let ix = self.env.iter().rposition(|(n, _)| n == name)?;
        Some((lit.clone(), self.env[ix].1))
    }

    /// What a component parameter was given at the `use` being followed, when `name`
    /// still means the parameter: a `let`, `each` or input variable of the same name in
    /// between hides it (design 27 §5.7).
    fn param_given(&self, name: &str) -> Option<&Given> {
        let ix = self.env.iter().rposition(|(n, _)| n == name)?;
        if ix >= self.nparams {
            return None;
        }
        self.given.get(name)
    }

    /// The terms of an `args` with every bare `{p}` that stands for a parameter of the
    /// component being followed replaced by the terms its `use` wrote for `p` (design 27
    /// §5.7). Each result carries the type it has where it was written, and the index of
    /// the term it came from. A `name={p}` term takes the one term the `use` gave, if
    /// it gave just one. Anything else stays as it is.
    fn splice(&self, terms: &[crate::ast::Term], tys: &[Ty]) -> Vec<(crate::ast::Term, Ty, usize)> {
        let mut out = Vec::new();
        for (i, (t, ty)) in terms.iter().zip(tys).enumerate() {
            let given = match &t.value {
                Template::Expr(crate::expr::Expr::Name { name, .. }) => self.param_given(name),
                _ => None,
            };
            match (given, &t.name) {
                (Some(Given::Terms(list)), None) => {
                    out.extend(list.iter().map(|(t, ty)| (t.clone(), *ty, i)));
                }
                (Some(Given::Terms(list)), Some(_))
                    if list.len() == 1 && list[0].0.name.is_none() =>
                {
                    let (one, one_ty) = &list[0];
                    let term = crate::ast::Term {
                        value: one.value.clone(),
                        ..t.clone()
                    };
                    out.push((term, *one_ty, i));
                }
                (Some(Given::Text(lit)), _) => {
                    let term = crate::ast::Term {
                        value: Template::Text(lit.clone()),
                        ..t.clone()
                    };
                    out.push((term, *ty, i));
                }
                _ => out.push((t.clone(), *ty, i)),
            }
        }
        out
    }

    /// A `name={p}` term takes the one unnamed term the `use` wrote for `p`; for several
    /// terms, or a named one, there is no single value to give `name`, so it is refused
    /// where the `use` wrote them (design 27 §5.7).
    fn named_forward(&mut self, t: &crate::ast::Term, ty: &Ty) {
        let (Some(name), Template::Expr(crate::expr::Expr::Name { name: p, .. })) =
            (&t.name, &t.value)
        else {
            return;
        };
        let Some(Given::Terms(list)) = self.param_given(p) else {
            return;
        };
        if list.len() == 1 && list[0].0.name.is_none() {
            return;
        }
        let msg = format!("`{name}={{{p}}}` takes one unnamed term, and `{p}` is not one (S16)");
        let help =
            format!("pass a single value, or write `{{{p}}}` on its own to forward every term");
        self.emit(
            Some(ty),
            Code::ActionArgs,
            term_value_span(t),
            msg,
            Some(help),
        );
    }

    /// Check the names in the terms of `args` when there is no action to match them to.
    fn check_terms_only(&mut self, n: &mut Node, args: &str) {
        let Some(i) = n.props.iter().rposition(|p| p.name == args) else {
            return;
        };
        let Val::Terms(terms) = &mut n.props[i].arg.val else {
            return;
        };
        let mut terms = std::mem::take(terms);
        self.with_inputs(&n.name, |ck| {
            for t in &mut terms {
                let loc = ck.loc_term(t);
                ck.template(&mut t.value, &loc);
            }
        });
        if let Val::Terms(slot) = &mut n.props[i].arg.val {
            *slot = terms;
        }
    }

    /// The input variables of an action's `args` are bound while its terms are checked
    /// (design 26 §4); which elements may use them is the format-level pass's rule.
    fn with_inputs(&mut self, element: &str, f: impl FnOnce(&mut Self)) {
        let mark = self.env.len();
        let on = if element == "toggle" {
            Ty::of(FieldType::Bool)
        } else {
            Ty::default()
        };
        self.env.push(("on".to_owned(), on));
        self.env.push(("value".to_owned(), Ty::default()));
        f(self);
        self.env.truncate(mark);
    }

    fn bind_action(
        &mut self,
        n: &mut Node,
        prop: &str,
        args: &str,
        spec: &'static ActionSpec,
        at: Span,
    ) -> BoundAction {
        let mut out = BoundAction {
            prop: prop.to_owned(),
            spec,
            terms: Vec::new(),
            args: Vec::new(),
        };
        // A string that didn't parse was reported already.
        if n.prop(args)
            .is_some_and(|p| matches!(p.arg.val, Val::Invalid))
        {
            return out;
        }
        let mut terms = match n.props.iter_mut().rfind(|p| p.name == args) {
            Some(p) => match &mut p.arg.val {
                Val::Terms(t) => std::mem::take(t),
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        let args_span = n.prop(args).map(|p| p.arg.span);
        // Names and types of every term first.
        let mut tys = Vec::new();
        self.with_inputs(&n.name, |ck| {
            for t in &mut terms {
                let loc = ck.loc_term(t);
                let ty = ck.template(&mut t.value, &loc);
                tys.push(ty);
            }
        });
        let args_span = args_span.unwrap_or(at);
        // A bare `{p}` that is a component parameter stands for the terms the `use` wrote
        // for it: they are checked here as if written in this `args` (design 27 §5.7).
        for (t, ty) in terms.iter().zip(&tys) {
            self.named_forward(t, ty);
        }
        let spliced = self.splice(&terms, &tys);
        // On an element with no input, `value` and `on` in a term were reported by the
        // format-level pass: they stay out of the matching.
        let no_input = !gogo_contract::actions::INPUT_ELEMENTS.contains(&n.name.as_str());
        let spliced: Vec<(crate::ast::Term, Ty, usize)> = spliced
            .into_iter()
            .filter(|(t, _, _)| {
                !(no_input
                    && gogo_contract::actions::INPUT_VARS
                        .iter()
                        .any(|v| t.value.mentions(v)))
            })
            .collect();
        // What is missing is reported at the `use` that gave the forwarded terms, not at
        // the component's own `args`, which the author of the `use` can't change.
        let use_at = spliced
            .iter()
            .find(|(_, ty, _)| ty.param)
            .and_then(|(_, ty, _)| ty.origin);
        let m = Matcher {
            spec,
            args_span,
            use_at,
        };
        let seen: Vec<crate::ast::Term> = spliced.iter().map(|(t, _, _)| t.clone()).collect();
        let tys: Vec<Ty> = spliced.iter().map(|(_, ty, _)| *ty).collect();
        out.args = self.match_terms(&m, &seen, &tys);
        out.terms = seen;
        if let Some(p) = n.props.iter_mut().rfind(|p| p.name == args)
            && let Val::Terms(slot) = &mut p.arg.val
        {
            *slot = terms;
        }
        out
    }

    /// Match the terms of an action's `args` to its parameters: `name=value` by name, the
    /// others in order. Reports what doesn't fit, and returns what does.
    fn match_terms(
        &mut self,
        m: &Matcher,
        terms: &[crate::ast::Term],
        tys: &[Ty],
    ) -> Vec<BoundArg> {
        let spec = m.spec;
        let params = spec.params;
        let names = || params.iter().map(|p| p.name).collect::<Vec<_>>().join(", ");
        let route_ix = params.iter().position(|p| p.ty == ParamType::RouteParams);
        let mut assigned: Vec<Option<usize>> = vec![None; params.len()];
        let mut route_terms: Vec<usize> = Vec::new();
        // Named terms.
        for (ti, t) in terms.iter().enumerate() {
            let Some(nm) = &t.name else { continue };
            match params.iter().position(|p| p.name == nm) {
                Some(pi) if assigned[pi].is_none() => assigned[pi] = Some(ti),
                Some(_) => self.emit(
                    None,
                    Code::ActionArgs,
                    t.span,
                    format!("`{nm}` is given twice to `{}`", spec.name),
                    None,
                ),
                None if route_ix.is_some() => route_terms.push(ti),
                None => {
                    let help = match suggest(nm, params.iter().map(|p| p.name)) {
                        Some(s) => format!("did you mean `{s}`?"),
                        None if params.is_empty() => format!("`{}` takes no arguments", spec.name),
                        None => format!("its parameters are {}", names()),
                    };
                    self.emit(
                        None,
                        Code::ActionArgs,
                        t.span,
                        format!("`{}` has no parameter `{nm}` (S16)", spec.name),
                        Some(help),
                    );
                }
            }
        }
        // The others, in order. A `RouteParams` parameter takes all that are left.
        let mut next = 0usize;
        for (ti, t) in terms.iter().enumerate() {
            if t.name.is_some() {
                continue;
            }
            while next < params.len() && assigned[next].is_some() {
                next += 1;
            }
            match params.get(next) {
                Some(p) if p.ty == ParamType::RouteParams => route_terms.push(ti),
                Some(_) => {
                    assigned[next] = Some(ti);
                    next += 1;
                }
                None => {
                    let msg = format!(
                        "`{}` takes {} argument{}, found more (S16)",
                        spec.name,
                        params.len(),
                        if params.len() == 1 { "" } else { "s" }
                    );
                    self.emit(
                        None,
                        Code::ActionArgs,
                        t.span,
                        msg,
                        Some(format!("its parameters are {}", names())),
                    );
                }
            }
        }
        // What is missing.
        for (pi, p) in params.iter().enumerate() {
            if assigned[pi].is_none() && !p.optional && p.ty != ParamType::RouteParams {
                self.emit(
                    m.missing_ty().as_ref(),
                    Code::ActionArgs,
                    m.args_span,
                    format!("`{}` needs `{}` (S16)", spec.name, p.name),
                    Some(format!("its parameters are {}", names())),
                );
            }
        }
        // Each term against its parameter.
        let mut out = Vec::new();
        let mut route: Option<&'static RouteSpec> = None;
        for (pi, ti) in assigned.iter().enumerate() {
            let Some(ti) = *ti else { continue };
            let var = (params[pi].ty == ParamType::UiValue)
                .then(|| {
                    let vi = params.iter().position(|p| p.ty == ParamType::UiVar)?;
                    plain_text(&terms[assigned[vi]?].value)
                })
                .flatten();
            let value = self.check_arg(spec, &params[pi], &terms[ti], &tys[ti], var);
            if let ArgRef::Route(r) = &value {
                route = Some(r);
            }
            out.push(BoundArg {
                term: ti,
                param: &params[pi],
                value,
            });
        }
        if let (Some(pi), Some(route)) = (route_ix, route) {
            out.extend(self.route_params(m, route, &route_terms, terms, &params[pi]));
        }
        out
    }

    /// The terms after a `nav.go` route: its parameters, in order or by name.
    fn route_params(
        &mut self,
        m: &Matcher,
        route: &'static RouteSpec,
        route_terms: &[usize],
        terms: &[crate::ast::Term],
        param: &'static Param,
    ) -> Vec<BoundArg> {
        let mut out = Vec::new();
        let mut taken: Vec<&'static gogo_contract::SourceParam> = Vec::new();
        let mut positional = route.params.iter();
        for &ti in route_terms {
            let t = &terms[ti];
            let sp = match &t.name {
                Some(nm) => route.params.iter().find(|p| p.name == nm),
                None => positional.by_ref().find(|p| !taken.contains(p)),
            };
            let Some(sp) = sp else {
                let msg = if let Some(nm) = &t.name {
                    format!("the `{}` route has no parameter `{nm}` (S16)", route.name)
                } else {
                    format!(
                        "`{}` takes {} parameter(s), found more (S16)",
                        route.name,
                        route.params.len()
                    )
                };
                let names: Vec<&str> = route.params.iter().map(|p| p.name).collect();
                let help = if names.is_empty() {
                    format!("the `{}` route takes no parameters", route.name)
                } else {
                    format!("its parameters are {}", names.join(", "))
                };
                self.emit(None, Code::ActionArgs, t.span, msg, Some(help));
                continue;
            };
            if taken.contains(&sp) {
                self.emit(
                    None,
                    Code::ActionArgs,
                    t.span,
                    format!("`{}` is given twice to the `{}` route", sp.name, route.name),
                    None,
                );
                continue;
            }
            taken.push(sp);
            // A section parameter is one of the route's sections.
            if sp.name == "section"
                && let Some(text) = plain_text(&t.value)
            {
                let at = term_value_span(t);
                if !route.sections.contains(&text) {
                    let help = format!(
                        "the sections of `{}` are {}",
                        route.name,
                        route.sections.join(", ")
                    );
                    self.emit(
                        None,
                        Code::UnknownSection,
                        at,
                        format!("`{}` has no section `{text}` (S16)", route.name),
                        Some(help),
                    );
                }
            }
            out.push(BoundArg {
                term: ti,
                param,
                value: ArgRef::RouteParam(sp),
            });
        }
        for sp in route.params.iter().filter(|p| !p.optional) {
            if !taken.contains(&sp) {
                self.emit(
                    m.missing_ty().as_ref(),
                    Code::ActionArgs,
                    m.args_span,
                    format!("the `{}` route needs `{}` (S16)", route.name, sp.name),
                    Some(format!("write `{} <{}>`", route.name, sp.name)),
                );
            }
        }
        out
    }

    /// One term against its parameter's type.
    fn check_arg(
        &mut self,
        spec: &'static ActionSpec,
        param: &'static Param,
        term: &crate::ast::Term,
        ty: &Ty,
        var: Option<&str>,
    ) -> ArgRef {
        let at = term_value_span(term);
        let text = plain_text(&term.value);
        let is_expr = matches!(term.value, Template::Expr(_));
        let literal = text.is_some();
        // A parameter the skin names by a literal: a route, a variable, a setting key.
        if param.ty.literal_only() {
            let code = if param.ty == ParamType::SettingKey {
                Code::SettingKey
            } else {
                Code::TypeMismatch
            };
            let Some(text) = text else {
                let why = match param.ty {
                    ParamType::SettingKey => {
                        "a setting key is written out in the skin, never read from a field (S21)"
                            .to_owned()
                    }
                    ParamType::Route => {
                        "a route is written out in the skin, never read from a field (S16)"
                            .to_owned()
                    }
                    ParamType::UiVar => {
                        "a `ui` variable is written out in the skin (S16)".to_owned()
                    }
                    _ => format!("`{}` is written out in the skin (S16)", param.name),
                };
                self.emit(
                    Some(ty),
                    code,
                    at,
                    format!("`{}` must be a literal: {why}", param.name),
                    Some("write the value itself, not an expression".to_owned()),
                );
                return ArgRef::Value;
            };
            return match param.ty {
                ParamType::Route => match routes::route_spec(text) {
                    Some(r) => ArgRef::Route(r),
                    None => {
                        let names = routes::ROUTES.iter().map(|r| r.name);
                        let help = match suggest(text, names.clone()) {
                            Some(s) => format!("did you mean `{s}`?"),
                            None => {
                                format!("the routes are {}", names.collect::<Vec<_>>().join(", "))
                            }
                        };
                        self.emit(
                            Some(ty),
                            Code::UnknownRoute,
                            at,
                            format!("`{text}` is not a route of the contract (S16)"),
                            Some(help),
                        );
                        ArgRef::Value
                    }
                },
                ParamType::UiVar => match self.ui_var(text) {
                    Some(v) => ArgRef::UiVar(v.name.clone()),
                    None => {
                        let vars: Vec<&str> =
                            self.manifest.ui.iter().map(|v| v.name.as_str()).collect();
                        let help = if vars.is_empty() {
                            "the manifest declares no `ui` variables".to_owned()
                        } else {
                            format!("the variables are {}", vars.join(", "))
                        };
                        self.emit(
                            Some(ty),
                            Code::ActionArgs,
                            at,
                            format!("`{text}` is not a `ui` variable of this skin"),
                            Some(help),
                        );
                        ArgRef::Value
                    }
                },
                ParamType::SettingKey => match settings::SKIN_SETTABLE.iter().find(|k| **k == text)
                {
                    Some(k) => ArgRef::SettingKey(k),
                    None => {
                        let (msg, help) = if settings::is_app_only(text) {
                            (
                                format!(
                                    "`{text}` is an app-only setting: a skin can't bind it (S21)"
                                ),
                                "only the app's own settings route changes an app-only setting"
                                    .to_owned(),
                            )
                        } else {
                            (
                                format!("`{text}` is not a setting a skin may set (S21)"),
                                format!("the keys are {}", settings::SKIN_SETTABLE.join(", ")),
                            )
                        };
                        self.emit(Some(ty), Code::SettingKey, at, msg, Some(help));
                        ArgRef::Value
                    }
                },
                _ => ArgRef::Value,
            };
        }
        // A value for a `ui` variable fits the variable's type.
        if param.ty == ParamType::UiValue
            && let Some(var) = var.and_then(|v| self.ui_var(v))
            && let Some(text) = text
            && let Some(why) = ui_value_error(&var.ty, text)
        {
            self.emit(
                Some(ty),
                Code::TypeMismatch,
                at,
                format!("`{text}` is not a value of `ui.{}`: {why}", var.name),
                None,
            );
            return ArgRef::Value;
        }
        if let (ParamType::OneOf(words), Some(text)) = (param.ty, text)
            && !words.contains(&text)
        {
            self.emit(
                Some(ty),
                Code::TypeMismatch,
                at,
                format!("`{text}` is not one of the values of `{}`", param.name),
                Some(format!("`{}` takes {}", param.name, list(words))),
            );
        } else if param.ty == ParamType::BoundField && literal {
            self.emit(
                Some(ty),
                Code::TypeMismatch,
                at,
                format!("`{}` takes a field, not text", param.name),
                Some("write the field path, for example `{g.slug}`".to_owned()),
            );
        } else if is_expr
            && ty.plain
            && let Some(ft) = ty.ft
            && !param.ty.accepts(&ft)
        {
            self.emit(
                Some(ty),
                Code::TypeMismatch,
                at,
                format!(
                    "`{}` of `{}` takes {:?}, and this is {}",
                    param.name,
                    spec.name,
                    param.ty,
                    crate::typing::type_name(&ft)
                ),
                None,
            );
        }
        ArgRef::Value
    }

    // ---- locations and type needs ----

    fn need_testable(&mut self, ty: &Ty, what: &str, span: Span) {
        if let Some(ft) = ty.ft
            && !ty.nullable
            && !ft.is_testable()
        {
            let msg = format!(
                "`{what}` tests a Bool, something that may be null, or a list; this is {}",
                crate::typing::type_name(&ft)
            );
            self.emit(Some(ty), Code::TypeMismatch, span, msg, None);
        }
    }

    fn need_list(&mut self, ty: &Ty, what: &str, span: Span) {
        if let Some(ft) = ty.ft
            && ft.item().is_none()
        {
            let msg = format!(
                "`{what}` takes a list; this is {}",
                crate::typing::type_name(&ft)
            );
            self.emit(Some(ty), Code::TypeMismatch, span, msg, None);
        }
    }

    fn need_fraction(&mut self, ty: &Ty, what: &str, span: Span) {
        if let Some(ft) = ty.ft
            && !matches!(ft.inner(), FieldType::Fraction)
        {
            let msg = format!(
                "`{what}` takes a Fraction (0 to 1); this is {}",
                crate::typing::type_name(&ft)
            );
            self.emit(Some(ty), Code::TypeMismatch, span, msg, None);
        }
    }

    /// The span of `len` bytes at `at` inside a plain string's content, or the whole
    /// value when the text isn't as written. With no `len`, the whole content.
    fn inner_span(&self, a: &Arg, at: usize, len: Option<usize>) -> Span {
        match (a.inner, a.val.plain()) {
            (Some(i), Some(text)) => a
                .span
                .inner(i + at, len.unwrap_or(text.len().saturating_sub(at))),
            _ => a.span,
        }
    }
}

struct Matcher {
    spec: &'static ActionSpec,
    /// The `args` value, or the action's name when there is none.
    args_span: Span,
    /// The `use` argument that gave the terms spliced into `args`, if any were.
    use_at: Option<Span>,
}

impl Matcher {
    /// The type that places a missing-argument error at the `use`, in an instance walk.
    fn missing_ty(&self) -> Option<Ty> {
        self.use_at.map(|origin| Ty {
            param: true,
            origin: Some(origin),
            ..Ty::default()
        })
    }
}

fn plain_text(t: &Template) -> Option<&str> {
    match t {
        Template::Text(s) => Some(s),
        _ => None,
    }
}

/// Where a term's value is: the whole term, less a `name=` prefix when it is exact.
fn term_value_span(t: &crate::ast::Term) -> Span {
    match (&t.name, t.exact) {
        (Some(n), true) => t
            .span
            .inner(n.len() + 1, t.span.len.saturating_sub(n.len() + 1)),
        _ => t.span,
    }
}

/// Why `text` is not a value of a `ui` variable of type `ty`.
fn ui_value_error(ty: &UiType, text: &str) -> Option<String> {
    match ty {
        UiType::Bool if text != "true" && text != "false" => Some("it is a bool".to_owned()),
        UiType::Number if text.parse::<f64>().is_err() => Some("it is a number".to_owned()),
        UiType::Enum(words) if !words.iter().any(|w| w == text) => {
            Some(format!("its values are {}", words.join(", ")))
        }
        UiType::Section if !routes::ROUTES.iter().any(|r| r.sections.contains(&text)) => {
            Some("it is one of the contract's sections".to_owned())
        }
        _ => None,
    }
}

/// `a, b or c`, each in backticks.
fn list(words: &[&str]) -> String {
    let q: Vec<String> = words.iter().map(|w| format!("`{w}`")).collect();
    match q.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => "nothing".to_owned(),
    }
}

/// The words of a whitespace-separated string with the offset of each.
fn word_offsets(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match (start, c.is_whitespace()) {
            (None, false) => start = Some(i),
            (Some(st), true) => {
                out.push((st, &s[st..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push((st, &s[st..]));
    }
    out
}

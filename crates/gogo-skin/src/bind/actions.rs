//! Actions in a bound tree: the table behind each [`ActionRef`] (design 24 §2, design 25
//! §4, design 26 §4).
//!
//! The engine hands back the [`ActionRef`] of the node a user activated. The table maps it
//! to the action's `&'static` spec and its arguments, already evaluated: the binder never
//! runs an action (S4), it only prepares the call.
//!
//! An argument that uses an input variable (`value` of a `select`, `on` of a `toggle`;
//! design 26 §4) can't be known until the input happens, so it is kept as a
//! [`PendingArg`]: the parsed expression with the variables it reads as they were when the
//! route was bound. The app evaluates it, with the input, when the user acts.

use std::collections::BTreeSet;

use gogo_contract::SourceParam;
use gogo_contract::actions::{Param, ParamType};
use gogo_contract::{ActionSpec, Value, routes, settings};
use gogo_ui_ir::ActionRef;

use super::absent_param;
use crate::expr::{EvalError, Scope, Template};
use crate::manifest::{Manifest, UiType};

/// Every action call of one bound tree. An [`ActionRef`] indexes it, and is valid for that
/// tree only (design 24 §2).
#[derive(Debug, Clone, Default)]
pub struct ActionTable {
    calls: Vec<ActionCall>,
}

impl ActionTable {
    /// The call behind `r`.
    pub fn get(&self, r: ActionRef) -> Option<&ActionCall> {
        self.calls.get(r.index())
    }

    /// How many calls the tree has.
    pub fn len(&self) -> usize {
        self.calls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Every call with its id, in the order the binder met them.
    pub fn iter(&self) -> impl Iterator<Item = (ActionRef, &ActionCall)> {
        self.calls.iter().enumerate().filter_map(|(i, c)| {
            let i = u32::try_from(i).ok()?;
            Some((ActionRef::new(i), c))
        })
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.calls.truncate(len);
    }

    pub(crate) fn push(&mut self, call: ActionCall) -> Option<ActionRef> {
        let i = u32::try_from(self.calls.len()).ok()?;
        self.calls.push(call);
        Some(ActionRef::new(i))
    }
}

/// An action ready to be invoked: which one, and the value of each argument.
#[derive(Debug, Clone)]
pub struct ActionCall {
    pub spec: &'static ActionSpec,
    /// The arguments given, in the order of the action's parameters. A parameter that is
    /// optional and left out, or given null, is not here.
    pub args: Vec<CallArg>,
    /// The skin text the call was bound from, `routes/game.kdl:12:5`.
    pub location: String,
}

/// One argument of a call.
#[derive(Debug, Clone)]
pub struct CallArg {
    pub param: &'static Param,
    /// For a `nav.go` route parameter (`game {$id} files`): which parameter of the route
    /// it is. `param` is then the action's `params` parameter.
    pub route_param: Option<&'static SourceParam>,
    pub value: ArgValue,
}

/// What an argument is.
#[derive(Debug, Clone)]
pub enum ArgValue {
    /// Known now.
    Value(Value),
    /// Known when the user acts.
    OnInput(PendingArg),
}

/// What the user's input binds while a [`PendingArg`] is evaluated (design 26 §4).
#[derive(Debug, Clone, Default)]
pub struct Input {
    /// A toggle's state before the input.
    pub on: Option<bool>,
    /// The option a select was set to.
    pub value: Option<Value>,
}

/// An argument that reads an input variable.
///
/// The value is only known when the user acts, so the checks a known value gets at bind
/// (S1, S16, S21: a masked value, a fixed set of words, the type of a `ui` value, a
/// route's section) are kept here and run on the value [`eval`](Self::eval) computes. An
/// argument that must be written out in the skin (a route, a `ui` variable, a settings key)
/// can't be pending at all: binding refuses it.
#[derive(Debug, Clone)]
pub struct PendingArg {
    template: Template,
    captured: Vec<(String, Value)>,
    /// What the value must satisfy; set when the term is matched to its parameter.
    check: Option<Check>,
    /// The parameters of the route that was bound: an absent optional one reads as null.
    route_params: &'static [SourceParam],
}

/// What a pending value must satisfy once it is known.
#[derive(Debug, Clone)]
enum Check {
    /// A parameter of the action; a `ui` value also knows the type of its variable.
    Param {
        spec: &'static ActionSpec,
        param: &'static Param,
        ui: Option<UiType>,
    },
    /// A parameter of a `nav.go` route.
    Route {
        route: &'static routes::RouteSpec,
        param: &'static SourceParam,
    },
}

/// Why a [`PendingArg`] gives no value.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PendingError {
    /// The expression failed on the data, like any evaluation.
    #[error("{0}")]
    Eval(#[from] EvalError),
    /// The input makes a value the action may not take: the call fails and nothing runs.
    #[error("{0}")]
    Rejected(String),
    /// The input makes a masked value (S1, S2).
    #[error("{0}")]
    Masked(String),
}

impl PendingArg {
    pub(crate) fn new(template: Template, captured: Vec<(String, Value)>) -> Self {
        PendingArg {
            template,
            captured,
            check: None,
            route_params: &[],
        }
    }

    pub(crate) fn on_route(mut self, params: &'static [SourceParam]) -> Self {
        self.route_params = params;
        self
    }

    fn checked(mut self, check: Check) -> Self {
        self.check = Some(check);
        self
    }

    /// Evaluate against the input. `data` answers what the skin's expressions read from the
    /// app (sources, route parameters, `now`); the variables the route had in scope when
    /// it was bound are the ones kept here. The value then gets the checks its parameter
    /// has (S1, S16, S21), as a value known at bind does.
    ///
    /// # Errors
    /// [`PendingError::Eval`] when the expression fails on the data, like any evaluation;
    /// [`PendingError::Rejected`] or [`PendingError::Masked`] when the input makes a value
    /// the action may not take. The call must not run then.
    pub fn eval(&self, data: &dyn Scope, input: &Input) -> Result<Value, PendingError> {
        let mut vars = self.captured.clone();
        if let Some(on) = input.on {
            vars.push(("on".to_owned(), Value::Bool(on)));
        }
        if let Some(v) = &input.value {
            vars.push(("value".to_owned(), v.clone()));
        }
        let scope = Captured {
            data,
            vars,
            route_params: self.route_params,
        };
        let v = self.template.eval(&scope)?.into_owned();
        let checked = match &self.check {
            Some(Check::Param { spec, param, ui }) => {
                check_value(spec, param, &v, None, ui.as_ref()).and_then(|()| {
                    not_null(&v, param.optional, || {
                        format!(
                            "`{}` of `{}` is null here, and the action needs it",
                            param.name, spec.name
                        )
                    })
                })
            }
            Some(Check::Route { route, param }) => check_route_param(route, param, &v),
            None if matches!(v, Value::Masked) => Err(Problem::Masked(
                "a masked value can't be an argument (S1, S2)".to_owned(),
            )),
            None => Ok(()),
        };
        match checked {
            Ok(()) => Ok(v),
            Err(Problem::Masked(m)) => Err(PendingError::Masked(m)),
            Err(Problem::Other(m)) => Err(PendingError::Rejected(m)),
        }
    }
}

/// A null is only fine for an optional parameter.
fn not_null(v: &Value, optional: bool, msg: impl FnOnce() -> String) -> Result<(), Problem> {
    if matches!(v, Value::Null) && !optional {
        return other(msg());
    }
    Ok(())
}

/// A value for one parameter of a `nav.go` route (S1, S16).
fn check_route_param(
    route: &'static routes::RouteSpec,
    sp: &'static SourceParam,
    v: &Value,
) -> Result<(), Problem> {
    match v {
        Value::Null if sp.optional => Ok(()),
        Value::Null => other(format!(
            "`{}` of the `{}` route is null here",
            sp.name, route.name
        )),
        Value::Masked => Err(Problem::Masked(format!(
            "a masked value can't be an argument (`{}`, S1)",
            sp.name
        ))),
        v if sp.name == "section" && !v.as_str().is_some_and(|s| route.sections.contains(&s)) => {
            other(format!(
                "`{}` is not a section of `{}`",
                v.display(),
                route.name
            ))
        }
        _ => Ok(()),
    }
}

struct Captured<'a> {
    data: &'a dyn Scope,
    vars: Vec<(String, Value)>,
    route_params: &'static [SourceParam],
}

impl Scope for Captured<'_> {
    fn var(&self, name: &str) -> Option<&Value> {
        match self.vars.iter().rev().find(|(n, _)| n == name) {
            Some((_, v)) => Some(v),
            None => self.data.var(name),
        }
    }
    fn param(&self, name: &str) -> Option<&Value> {
        self.data
            .param(name)
            .or_else(|| absent_param(self.route_params, name))
    }
    fn token(&self, name: &str) -> Option<&Value> {
        self.data.token(name)
    }
    fn call(&self, name: &str, args: &[&Value]) -> Option<Result<Value, crate::expr::CallError>> {
        self.data.call(name, args)
    }
    fn now(&self) -> f64 {
        self.data.now()
    }
}

/// One term of an `args`, evaluated.
#[derive(Debug, Clone)]
pub(crate) struct BoundTerm {
    pub name: Option<String>,
    pub val: TermVal,
}

#[derive(Debug, Clone)]
pub(crate) enum TermVal {
    Value(Value),
    Pending(PendingArg),
}

/// Why a call can't be made.
#[derive(Debug, Clone)]
pub(crate) enum Problem {
    /// A masked value would reach an action (S1, S2).
    Masked(String),
    Other(String),
}

fn other<T>(msg: impl Into<String>) -> Result<T, Problem> {
    Err(Problem::Other(msg.into()))
}

/// Match the terms to the action's parameters the way the loader does (design 27 §5.3):
/// `name=value` by name, the others in order, and for `nav.go` the terms after the route
/// are the route's parameters. Then check what only the data can show.
pub(crate) fn assign(
    spec: &'static ActionSpec,
    terms: Vec<BoundTerm>,
    manifest: &Manifest,
) -> Result<Vec<CallArg>, Problem> {
    let params = spec.params;
    let route_ix = params.iter().position(|p| p.ty == ParamType::RouteParams);
    let mut slots: Vec<Option<BoundTerm>> = vec![None; params.len()];
    let mut route_terms: Vec<BoundTerm> = Vec::new();
    let mut unnamed: Vec<BoundTerm> = Vec::new();
    for t in terms {
        let Some(nm) = &t.name else {
            unnamed.push(t);
            continue;
        };
        match params.iter().position(|p| p.name == nm) {
            Some(pi) if slots[pi].is_none() => slots[pi] = Some(t),
            Some(_) => return other(format!("`{nm}` is given twice to `{}`", spec.name)),
            None if route_ix.is_some() => route_terms.push(t),
            None => return other(format!("`{}` has no parameter `{nm}`", spec.name)),
        }
    }
    let mut next = 0usize;
    for t in unnamed {
        while next < params.len() && slots[next].is_some() {
            next += 1;
        }
        match params.get(next) {
            Some(p) if p.ty == ParamType::RouteParams => route_terms.push(t),
            Some(_) => {
                slots[next] = Some(t);
                next += 1;
            }
            None => return other(format!("`{}` was given too many arguments", spec.name)),
        }
    }

    // The type of the `ui` variable a `ui.set` names, for the value it is given.
    let ui_ty: Option<UiType> = params
        .iter()
        .position(|p| p.ty == ParamType::UiVar)
        .and_then(|vi| match slots.get(vi) {
            Some(Some(BoundTerm {
                val: TermVal::Value(v),
                ..
            })) => v
                .as_str()
                .and_then(|name| manifest.ui.iter().find(|u| u.name == name))
                .map(|u| u.ty.clone()),
            _ => None,
        });
    let mut out = Vec::new();
    let mut route: Option<&'static routes::RouteSpec> = None;
    for (p, slot) in params.iter().zip(slots) {
        let Some(t) = slot else {
            if !p.optional && p.ty != ParamType::RouteParams {
                return other(format!("`{}` needs `{}`", spec.name, p.name));
            }
            continue;
        };
        let arg = match t.val {
            // A name the skin writes out is checked at bind, so it can't wait for input.
            TermVal::Pending(_) if p.ty.literal_only() => {
                return other(format!(
                    "`{}` of `{}` is written out in the skin, never read from the input (S16, S21)",
                    p.name, spec.name
                ));
            }
            TermVal::Pending(pa) => ArgValue::OnInput(pa.checked(Check::Param {
                spec,
                param: p,
                ui: ui_ty.clone(),
            })),
            TermVal::Value(Value::Null) if p.optional => continue,
            TermVal::Value(Value::Null) => {
                return other(format!(
                    "`{}` of `{}` is null here, and the action needs it",
                    p.name, spec.name
                ));
            }
            TermVal::Value(v) => {
                check_value(spec, p, &v, Some(manifest), ui_ty.as_ref())?;
                if p.ty == ParamType::Route {
                    route = v.as_str().and_then(routes::route_spec);
                }
                ArgValue::Value(v)
            }
        };
        out.push(CallArg {
            param: p,
            route_param: None,
            value: arg,
        });
    }

    if let (Some(ri), Some(route)) = (route_ix, route) {
        let mut taken: Vec<&'static SourceParam> = Vec::new();
        let mut positional = route.params.iter();
        for t in route_terms {
            let sp = match &t.name {
                Some(nm) => route.params.iter().find(|p| p.name == nm),
                None => positional.by_ref().find(|p| !taken.contains(p)),
            };
            let Some(sp) = sp else {
                return other(format!(
                    "the `{}` route has no such parameter (it has {})",
                    route.name,
                    route
                        .params
                        .iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            };
            if taken.contains(&sp) {
                return other(format!(
                    "`{}` is given twice to the `{}` route",
                    sp.name, route.name
                ));
            }
            taken.push(sp);
            let value = match t.val {
                TermVal::Pending(pa) => {
                    ArgValue::OnInput(pa.checked(Check::Route { route, param: sp }))
                }
                TermVal::Value(Value::Null) if sp.optional => continue,
                TermVal::Value(Value::Null) => {
                    return other(format!(
                        "`{}` of the `{}` route is null here",
                        sp.name, route.name
                    ));
                }
                TermVal::Value(Value::Masked) => {
                    return Err(Problem::Masked(format!(
                        "a masked value can't be an argument (`{}`, S1)",
                        sp.name
                    )));
                }
                TermVal::Value(v) => {
                    if sp.name == "section"
                        && !v.as_str().is_some_and(|s| route.sections.contains(&s))
                    {
                        return other(format!(
                            "`{}` is not a section of `{}`",
                            v.display(),
                            route.name
                        ));
                    }
                    ArgValue::Value(v)
                }
            };
            out.push(CallArg {
                param: &params[ri],
                route_param: Some(sp),
                value,
            });
        }
        for sp in route.params.iter().filter(|p| !p.optional) {
            if !taken.contains(&sp) {
                return other(format!("the `{}` route needs `{}`", route.name, sp.name));
            }
        }
    } else if route_ix.is_some() && !route_terms.is_empty() {
        return other("route parameters need a route first");
    }
    Ok(out)
}

/// What only the data can show about one value (S1, S16, S21). `manifest` is there for a
/// value known at bind; a pending one can't be a `ui` variable, so it has none. `ui` is the
/// type of the variable a `ui.set` value is for.
fn check_value(
    spec: &'static ActionSpec,
    p: &'static Param,
    v: &Value,
    manifest: Option<&Manifest>,
    ui: Option<&UiType>,
) -> Result<(), Problem> {
    if matches!(v, Value::Masked) {
        return Err(Problem::Masked(format!(
            "a masked value can't be the `{}` of `{}` (S1, S2)",
            p.name, spec.name
        )));
    }
    let text = v.as_str();
    match p.ty {
        ParamType::Route if !text.is_some_and(|t| routes::route_spec(t).is_some()) => {
            other(format!("`{}` is not a route", v.display()))
        }
        ParamType::UiVar
            if !text
                .is_some_and(|t| manifest.is_some_and(|m| m.ui.iter().any(|u| u.name == t))) =>
        {
            other(format!(
                "`{}` is not a `ui` variable of this skin",
                v.display()
            ))
        }
        ParamType::UiValue => {
            match ui.and_then(|ty| crate::resolve::ui_value_error(ty, &v.display())) {
                Some(why) => other(format!(
                    "`{}` is not a value of that variable: {why}",
                    v.display()
                )),
                None => Ok(()),
            }
        }
        // S21: only the keys on the skin-settable list, whatever the arguments said.
        ParamType::SettingKey if !text.is_some_and(|t| settings::SKIN_SETTABLE.contains(&t)) => {
            other(format!(
                "`{}` is not a setting a skin may set (S21)",
                v.display()
            ))
        }
        ParamType::OneOf(words) if !text.is_some_and(|t| words.contains(&t)) => other(format!(
            "`{}` is not one of the values of `{}`",
            v.display(),
            p.name
        )),
        _ => Ok(()),
    }
}

/// The variables a pending argument needs, by name, taken from the scope.
pub(crate) fn capture<'a>(
    names: impl IntoIterator<Item = &'a str>,
    vars: impl Fn(&str) -> Option<&'a Value>,
) -> Vec<(String, Value)> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::new();
    for n in names {
        if !seen.insert(n) {
            continue;
        }
        // A source name like `library.stats` is not a variable: the data scope has it.
        if let Some(v) = vars(n) {
            out.push((n.to_owned(), v.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Mode, SkinVersion, UiDefault, UiType, UiVar};
    use crate::source::Span;
    use gogo_contract::ContractVersion;
    use gogo_contract::actions::skin_action;

    /// A manifest with one `ui` variable.
    fn manifest() -> Manifest {
        Manifest {
            name: "x".to_owned(),
            title: "X".to_owned(),
            version: SkinVersion {
                major: 1,
                minor: 0,
                patch: 0,
                pre: None,
                build: None,
            },
            contract: ContractVersion::new(1, 0),
            modes: vec![Mode::Dark],
            routes: vec![],
            extends: None,
            ui: vec![UiVar {
                name: "tab".to_owned(),
                ty: UiType::Section,
                default: UiDefault::Word("overview".to_owned()),
                span: Span::default(),
            }],
            span: Span::default(),
        }
    }

    fn v(name: Option<&str>, s: &str) -> BoundTerm {
        BoundTerm {
            name: name.map(str::to_owned),
            val: TermVal::Value(Value::str(s)),
        }
    }

    fn assign_named(action: &str, terms: Vec<BoundTerm>) -> Result<Vec<CallArg>, Problem> {
        let spec = skin_action(action).expect("an action");
        assign(spec, terms, &manifest())
    }

    #[test]
    fn terms_match_by_name_then_in_order() {
        let out = assign_named("dlc.add", vec![v(Some("dlc"), "all"), v(None, "7")]).unwrap();
        let by: Vec<(&str, String)> = out
            .iter()
            .map(|a| match &a.value {
                ArgValue::Value(x) => (a.param.name, x.display()),
                ArgValue::OnInput(_) => panic!("known"),
            })
            .collect();
        assert_eq!(by, [("id", "7".to_owned()), ("dlc", "all".to_owned())]);
        assert!(
            assign_named("dlc.add", vec![v(None, "7")]).is_err(),
            "needs dlc"
        );
        assert!(assign_named("dlc.add", vec![v(None, "7"), v(None, "all"), v(None, "x")]).is_err());
        assert!(assign_named("dlc.add", vec![v(Some("id"), "7"), v(Some("id"), "8")]).is_err());
        assert!(assign_named("dlc.add", vec![v(Some("nope"), "7")]).is_err());
    }

    #[test]
    fn a_setting_key_must_be_on_the_skin_settable_list() {
        // S21, again at bind: whatever the loader let through.
        assert!(assign_named("settings.set", vec![v(None, "gui.mode"), v(None, "dark")]).is_ok());
        for key in ["cloud_saves.writes", "gui.nonsense", ""] {
            let r = assign_named("settings.set", vec![v(None, key), v(None, "on")]);
            assert!(matches!(r, Err(Problem::Other(_))), "{key}");
        }
    }

    #[test]
    fn a_route_takes_its_parameters_after_it() {
        let out = assign_named(
            "nav.go",
            vec![v(None, "game"), v(None, "5"), v(None, "builds")],
        )
        .unwrap();
        let names: Vec<Option<&str>> = out.iter().map(|a| a.route_param.map(|p| p.name)).collect();
        assert_eq!(names, [None, Some("id"), Some("section")]);
        // By name, in any order, and a missing optional one is fine.
        assert!(assign_named("nav.go", vec![v(None, "game"), v(Some("id"), "5")]).is_ok());
        // The required parameter, a section that doesn't exist, and a route that doesn't.
        assert!(assign_named("nav.go", vec![v(None, "game")]).is_err());
        assert!(assign_named("nav.go", vec![v(None, "game"), v(None, "5"), v(None, "x")]).is_err());
        assert!(assign_named("nav.go", vec![v(None, "nowhere")]).is_err());
        assert!(assign_named("nav.go", vec![v(None, "queue"), v(None, "5")]).is_err());
    }

    #[test]
    fn null_and_masked_values_are_refused_where_they_matter() {
        let null = BoundTerm {
            name: None,
            val: TermVal::Value(Value::Null),
        };
        // A required parameter given null is an error at bind (design 27 §7, "Failures").
        assert!(matches!(
            assign_named("game.play", vec![null.clone()]),
            Err(Problem::Other(_))
        ));
        // An optional one is left out.
        let out = assign_named("nav.go", vec![v(None, "game"), v(None, "5"), null]).unwrap();
        assert_eq!(out.len(), 2);
        // S1, S2: a secret is never an argument.
        let masked = BoundTerm {
            name: None,
            val: TermVal::Value(Value::Masked),
        };
        assert!(matches!(
            assign_named("game.play", vec![masked]),
            Err(Problem::Masked(_))
        ));
    }

    #[test]
    fn a_ui_variable_is_one_the_manifest_declares() {
        assert!(assign_named("ui.set", vec![v(None, "tab"), v(None, "builds")]).is_ok());
        assert!(assign_named("ui.set", vec![v(None, "nope"), v(None, "builds")]).is_err());
    }

    fn pending() -> BoundTerm {
        BoundTerm {
            name: None,
            val: TermVal::Pending(PendingArg::new(Template::Text(String::new()), Vec::new())),
        }
    }

    #[test]
    fn a_name_the_skin_writes_out_cannot_wait_for_the_input() {
        // S16, S21: a settings key, a `ui` variable and a route are checked by name.
        for (action, terms) in [
            ("settings.set", vec![pending(), v(None, "on")]),
            ("ui.set", vec![pending(), v(None, "builds")]),
            ("nav.go", vec![pending()]),
        ] {
            let r = assign_named(action, terms);
            assert!(matches!(r, Err(Problem::Other(_))), "{action}");
        }
    }

    #[test]
    fn a_value_for_a_ui_variable_that_waits_for_the_input_keeps_the_variables_type() {
        let out = assign_named("ui.set", vec![v(None, "tab"), pending()]).unwrap();
        assert!(matches!(out[1].value, ArgValue::OnInput(_)));
        // A value known now is checked against the variable's type too.
        assert!(assign_named("ui.set", vec![v(None, "tab"), v(None, "builds")]).is_ok());
        assert!(assign_named("ui.set", vec![v(None, "tab"), v(None, "nothing")]).is_err());
    }
}

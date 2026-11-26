//! What a skin asks for, and the checked form of it (design 26 §4, §7 S3, S16, S21).
//!
//! `gogo-gui` turns the binder's action call into an [`Invocation`]. The model trusts
//! none of it: [`Call::check`] looks the action up in the contract again and checks every
//! argument, and only a [`Call`] reaches the backend.

use std::collections::BTreeMap;

use gogo_contract::actions::{self, Param, ParamType};
use gogo_contract::{ActionSpec, Value, routes, settings};

use crate::error::ErrorView;

/// An action as a skin invoked it.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// Only its name is used: the contract's own entry decides kind, tier and parameters.
    pub spec: &'static ActionSpec,
    /// The arguments, in any order.
    pub args: Vec<Arg>,
}

/// One argument of an [`Invocation`].
#[derive(Debug, Clone)]
pub struct Arg {
    /// The parameter's name.
    pub name: String,
    /// For a `nav.go` route parameter, which parameter of the route it is.
    pub route_param: Option<String>,
    /// What was given.
    pub value: Value,
}

impl Invocation {
    /// An invocation with no arguments yet.
    pub fn new(spec: &'static ActionSpec) -> Self {
        Invocation {
            spec,
            args: Vec::new(),
        }
    }

    /// Add an argument.
    #[must_use]
    pub fn arg(mut self, name: &str, value: Value) -> Self {
        self.args.push(Arg {
            name: name.to_owned(),
            route_param: None,
            value,
        });
        self
    }

    /// Add a parameter of the route a `nav.go` goes to.
    #[must_use]
    pub fn route_arg(mut self, param: &str, value: Value) -> Self {
        self.args.push(Arg {
            name: "params".to_owned(),
            route_param: Some(param.to_owned()),
            value,
        });
        self
    }
}

/// A checked action call: a skin-bindable action whose arguments fit its parameters.
/// Values are normalised (ids are text, indices numbers, lists lists).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    spec: &'static ActionSpec,
    args: BTreeMap<&'static str, Value>,
    route_params: BTreeMap<&'static str, Value>,
}

impl Call {
    /// Check an invocation against the contract.
    ///
    /// # Errors
    /// An action a skin may not bind; an unknown, repeated, missing or ill-typed
    /// argument; a masked value (S1, S2); or a settings key a skin may not set (S21). The
    /// message never quotes a value.
    pub fn check(invocation: &Invocation) -> Result<Call, ErrorView> {
        let name = invocation.spec.name;
        let refuse = |why: String| ErrorView::usage("This action can't run", why);
        let spec = actions::skin_action(name)
            .ok_or_else(|| refuse(format!("`{name}` is not an action a skin may use")))?;
        let mut args = BTreeMap::new();
        let mut route_args = Vec::new();
        for arg in &invocation.args {
            let param = spec.param(&arg.name).ok_or_else(|| {
                refuse(format!(
                    "`{name}` has no argument `{}`",
                    arg.name.escape_debug()
                ))
            })?;
            if holds_masked(&arg.value) {
                return Err(refuse(format!(
                    "a masked value can't be the `{}` of `{name}`",
                    param.name
                )));
            }
            if let Some(route_param) = &arg.route_param {
                if param.ty != ParamType::RouteParams {
                    return Err(refuse(format!("`{name}` takes no route parameters")));
                }
                route_args.push((route_param.as_str(), &arg.value));
                continue;
            }
            if param.ty == ParamType::RouteParams {
                return Err(refuse("a route parameter needs its name".to_owned()));
            }
            if arg.value == Value::Null {
                continue;
            }
            if param.ty == ParamType::SettingKey
                && !arg.value.as_str().is_some_and(settings::skin_may_set)
            {
                return Err(refuse("a skin may not change that setting".to_owned()));
            }
            let value = normalise(param, &arg.value).ok_or_else(|| {
                refuse(format!("`{}` of `{name}` has the wrong type", param.name))
            })?;
            if args.insert(param.name, value).is_some() {
                return Err(refuse(format!(
                    "`{}` of `{name}` is given twice",
                    param.name
                )));
            }
        }
        for param in spec.params {
            let needed = !param.optional && param.ty != ParamType::RouteParams;
            if needed && !args.contains_key(param.name) {
                return Err(refuse(format!("`{name}` needs `{}`", param.name)));
            }
        }
        let route_params = check_route(&args, &route_args).map_err(refuse)?;
        Ok(Call {
            spec,
            args,
            route_params,
        })
    }

    /// The contract's entry for the action.
    pub fn spec(&self) -> &'static ActionSpec {
        self.spec
    }

    /// The action's name.
    pub fn name(&self) -> &'static str {
        self.spec.name
    }

    /// An argument, normalised; `None` if it was left out.
    pub fn arg(&self, name: &str) -> Option<&Value> {
        self.args.get(name)
    }

    /// An argument that is text.
    pub fn text(&self, name: &str) -> Option<&str> {
        self.arg(name).and_then(Value::as_str)
    }

    /// An argument that is a yes or no.
    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.arg(name) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    /// The parameters of the route a `nav.go` goes to.
    pub fn route_params(&self) -> &BTreeMap<&'static str, Value> {
        &self.route_params
    }
}

fn check_route(
    args: &BTreeMap<&'static str, Value>,
    given: &[(&str, &Value)],
) -> Result<BTreeMap<&'static str, Value>, String> {
    let mut out = BTreeMap::new();
    let Some(route) = args
        .get("route")
        .and_then(Value::as_str)
        .and_then(routes::route_spec)
    else {
        return if given.is_empty() {
            Ok(out)
        } else {
            Err("route parameters need a route".to_owned())
        };
    };
    for (name, value) in given {
        let param = route
            .params
            .iter()
            .find(|p| p.name == *name)
            .ok_or_else(|| format!("the `{}` route has no such parameter", route.name))?;
        if **value == Value::Null {
            continue;
        }
        let text = id(value).ok_or_else(|| format!("`{}` has the wrong type", param.name))?;
        let section_ok =
            param.name != "section" || text.as_str().is_some_and(|s| route.sections.contains(&s));
        if !section_ok {
            return Err(format!("the `{}` route has no such section", route.name));
        }
        if out.insert(param.name, text).is_some() {
            return Err(format!("`{}` is given twice", param.name));
        }
    }
    match route
        .params
        .iter()
        .find(|p| !p.optional && !out.contains_key(p.name))
    {
        Some(p) => Err(format!("the `{}` route needs `{}`", route.name, p.name)),
        None => Ok(out),
    }
}

fn holds_masked(v: &Value) -> bool {
    match v {
        Value::Masked => true,
        Value::List(l) => l.iter().any(holds_masked),
        Value::Map(m) => m.values().any(holds_masked),
        _ => false,
    }
}

/// A non-empty piece of text, or a whole number written as one.
fn id(v: &Value) -> Option<Value> {
    match v {
        Value::Str(s) if !s.is_empty() && !s.chars().any(char::is_control) => Some(v.clone()),
        Value::Num(_) => index(v).map(|n| Value::Str(n.display())),
        _ => None,
    }
}

/// A whole, non-negative number, possibly written as text.
fn index(v: &Value) -> Option<Value> {
    // The largest integer an f64 holds exactly.
    const MAX: u64 = 1 << 53;
    match v {
        Value::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= MAX as f64 => Some(v.clone()),
        Value::Str(s) if s.bytes().all(|b| b.is_ascii_digit()) => s
            .parse::<u64>()
            .ok()
            .filter(|n| *n <= MAX)
            .map(|n| Value::Num(n as f64)),
        _ => None,
    }
}

/// A non-empty list; a single item is a list of one.
fn list(v: &Value, item: fn(&Value) -> Option<Value>) -> Option<Value> {
    let items = match v {
        Value::List(l) => l.iter().map(item).collect::<Option<Vec<_>>>()?,
        one => vec![item(one)?],
    };
    (!items.is_empty()).then_some(Value::List(items))
}

fn scalar(v: &Value) -> Option<Value> {
    match v {
        Value::Bool(_) | Value::Str(_) => Some(v.clone()),
        Value::Num(n) if n.is_finite() => Some(v.clone()),
        _ => None,
    }
}

/// `v` as a value of `param`, or `None`. A literal in a skin arrives as text.
fn normalise(param: &Param, v: &Value) -> Option<Value> {
    use ParamType as T;
    match param.ty {
        T::GameId
        | T::JobId
        | T::CatalogId
        | T::Version
        | T::BuildId
        | T::Lang
        | T::Monitor
        | T::EnvName
        | T::PaletteScope
        | T::UiVar
        | T::UnusedOrCatalogId => id(v),
        T::Route => v
            .as_str()
            .filter(|s| routes::route_spec(s).is_some())
            .map(Value::str),
        // Handled by `check_route`.
        T::RouteParams => None,
        T::UiValue | T::BoundField | T::SettingValue => scalar(v),
        T::Bool => match v {
            Value::Bool(_) => Some(v.clone()),
            Value::Str(s) if s == "true" => Some(Value::Bool(true)),
            Value::Str(s) if s == "false" => Some(Value::Bool(false)),
            _ => None,
        },
        T::Index | T::Bytes => index(v),
        T::Indices => list(v, index),
        T::GameIds | T::DlcIds => list(v, id),
        T::DlcIdsOrAll if v.as_str() == Some("all") => Some(v.clone()),
        T::DlcIdsOrAll => list(v, id),
        T::OneOf(words) => v.as_str().filter(|s| words.contains(s)).map(Value::str),
        T::Selection => match v {
            Value::Map(m)
                if m.keys()
                    .all(|k| ["os", "lang", "kinds", "dlc"].contains(&k.as_str())) =>
            {
                Some(v.clone())
            }
            _ => None,
        },
        T::SettingKey => v
            .as_str()
            .filter(|k| settings::skin_may_set(k))
            .map(Value::str),
    }
}

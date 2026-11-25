//! Evaluation of a parsed [`Expr`]. No assignment, no I/O, no user functions, and the
//! work is bounded by the tree and the lists the scope returns. **Nothing here parses
//! text**: the loader parses once (design 27 §9 step 3).
//!
//! Evaluation returns a `Result`: a failure at run time fails the binding of the route
//! that evaluated the expression, and the route falls back (design 26 §8).

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BTreeMap;

use gogo_contract::{Status, Value};

use super::MAX_LIST;
use super::error::{CallError, EvalError, FieldList, Span};
use super::format;
use super::parse::{BinOp, Expr, Stage};

/// What expressions can read. The binder implements it. Lookups lend their values, so
/// evaluating a variable or a field path copies nothing.
pub trait Scope {
    /// A bound variable (`let`, a parameter, an input variable).
    fn var(&self, name: &str) -> Option<&Value>;
    /// A route parameter, `$name`.
    fn param(&self, name: &str) -> Option<&Value>;
    /// A token, `@name`: a colour (`#rrggbb[aa]`) or any other token value.
    fn token(&self, name: &str) -> Option<&Value>;
    /// A source or helper call such as `game($id)`; `None` if the name is unknown. The
    /// fixed formatters and status helpers are handled before the scope is asked.
    fn call(&self, name: &str, args: &[&Value]) -> Option<Result<Value, CallError>>;
    /// "Now" for `fmt.ago`, in unix seconds.
    fn now(&self) -> f64;
}

/// A scope that sees an item's fields first (for `where`).
struct ItemScope<'a> {
    item: &'a Value,
    up: &'a dyn Scope,
}

impl Scope for ItemScope<'_> {
    fn var(&self, name: &str) -> Option<&Value> {
        if let Value::Map(m) = self.item
            && let Some(v) = m.get(name)
        {
            return Some(v);
        }
        self.up.var(name)
    }
    fn param(&self, name: &str) -> Option<&Value> {
        self.up.param(name)
    }
    fn token(&self, name: &str) -> Option<&Value> {
        self.up.token(name)
    }
    fn call(&self, name: &str, args: &[&Value]) -> Option<Result<Value, CallError>> {
        self.up.call(name, args)
    }
    fn now(&self) -> f64 {
        self.up.now()
    }
}

static NULL: Value = Value::Null;

/// Evaluate a parsed expression. The result borrows from the expression and the scope
/// where it can (a literal, a variable, a field path) and is owned only where the
/// evaluation computes something.
///
/// An `Err` is a runtime failure: it fails the binding of the route that evaluated the
/// expression, and the route falls back (design 26 §8).
pub fn eval<'a>(e: &'a Expr, s: &'a dyn Scope) -> Result<Cow<'a, Value>, EvalError> {
    Ok(match e {
        Expr::Lit(v) => Cow::Borrowed(v),
        Expr::Name { name, .. } => match s.var(name) {
            Some(v) => Cow::Borrowed(v),
            None => Cow::Owned(Value::Str(name.clone())),
        },
        Expr::Param(p) => Cow::Borrowed(
            s.param(p)
                .ok_or_else(|| EvalError::UnknownParam(p.clone()))?,
        ),
        Expr::Token { name, alpha } => token(name, alpha.as_deref(), s)?,
        Expr::Field {
            base,
            name,
            optional,
            span,
        } => field(eval(base, s)?, name, *optional, *span)?,
        Expr::Call {
            name, args, span, ..
        } => Cow::Owned(call(name, args, *span, s)?),
        Expr::Not(e) => Cow::Owned(Value::Bool(!eval(e, s)?.truthy())),
        Expr::Neg(e) => match &*eval(e, s)? {
            Value::Num(n) => Cow::Owned(Value::Num(-n)),
            v => return Err(EvalError::NegNeedsNumber(v.type_name())),
        },
        Expr::Bin(BinOp::And, a, b) => {
            Cow::Owned(Value::Bool(eval(a, s)?.truthy() && eval(b, s)?.truthy()))
        }
        Expr::Bin(BinOp::Or, a, b) => {
            Cow::Owned(Value::Bool(eval(a, s)?.truthy() || eval(b, s)?.truthy()))
        }
        Expr::Bin(op, a, b) => {
            let (a, b) = (eval(a, s)?, eval(b, s)?);
            Cow::Owned(compare(*op, &a, &b)?)
        }
        Expr::Ternary(c, a, b) => {
            if eval(c, s)?.truthy() {
                eval(a, s)?
            } else {
                eval(b, s)?
            }
        }
        Expr::Else(a, b) => {
            let v = eval(a, s)?;
            if matches!(*v, Value::Null) {
                eval(b, s)?
            } else {
                v
            }
        }
        Expr::Pipe(base, st) => stage(eval(base, s)?, st, s)?,
    })
}

fn token<'a>(
    name: &str,
    alpha: Option<&'a Expr>,
    s: &'a dyn Scope,
) -> Result<Cow<'a, Value>, EvalError> {
    let v = s
        .token(name)
        .ok_or_else(|| EvalError::UnknownToken(name.to_string()))?;
    let Some(a) = alpha else {
        return Ok(Cow::Borrowed(v));
    };
    let a = eval(a, s)?.as_num().ok_or(EvalError::AlphaNotNumber)?;
    format::with_alpha(v, a)
        .map(Cow::Owned)
        .ok_or_else(|| EvalError::NotAColour(name.to_string()))
}

/// A field of `base`, by reference when `base` is borrowed.
fn field<'a>(
    base: Cow<'a, Value>,
    name: &str,
    optional: bool,
    span: Span,
) -> Result<Cow<'a, Value>, EvalError> {
    match base {
        Cow::Borrowed(b) => field_ref(b, name, optional, span),
        Cow::Owned(Value::Map(mut m)) => match m.remove(name) {
            Some(v) => Ok(Cow::Owned(v)),
            None => Err(no_field(name, &m, span)),
        },
        Cow::Owned(other) => Ok(Cow::Owned(
            field_ref(&other, name, optional, span)?.into_owned(),
        )),
    }
}

fn field_ref<'a>(
    base: &'a Value,
    name: &str,
    optional: bool,
    span: Span,
) -> Result<Cow<'a, Value>, EvalError> {
    match (base, optional) {
        (Value::Null, true) => Ok(Cow::Borrowed(&NULL)),
        (Value::Map(m), _) => m
            .get(name)
            .map(Cow::Borrowed)
            .ok_or_else(|| no_field(name, m, span)),
        (Value::List(l), _) if name == "len" => Ok(Cow::Owned(Value::Num(l.len() as f64))),
        _ => Err(EvalError::FieldOnType {
            name: name.to_string(),
            ty: base.type_name(),
            span,
        }),
    }
}

fn no_field(name: &str, m: &BTreeMap<String, Value>, span: Span) -> EvalError {
    EvalError::NoField {
        name: name.to_string(),
        fields: FieldList(m.keys().cloned().collect()),
        span,
    }
}

fn call(name: &str, args: &[Expr], span: Span, s: &dyn Scope) -> Result<Value, EvalError> {
    let vals = args
        .iter()
        .map(|a| eval(a, s))
        .collect::<Result<Vec<_>, _>>()?;
    let args: Vec<&Value> = vals.iter().map(|v| &**v).collect();
    if let Some(v) = builtin(name, &args, span, s) {
        return v;
    }
    match s.call(name, &args) {
        Some(Ok(v)) => Ok(v),
        Some(Err(source)) => Err(EvalError::Call {
            name: name.to_string(),
            span,
            source,
        }),
        None => Err(EvalError::UnknownCall {
            name: name.to_string(),
            span,
        }),
    }
}

fn compare(op: BinOp, a: &Value, b: &Value) -> Result<Value, EvalError> {
    Ok(Value::Bool(match op {
        BinOp::Eq => same(a, b),
        BinOp::Ne => !same(a, b),
        _ => {
            let o = order(a, b).ok_or(EvalError::Compare {
                left: a.type_name(),
                right: b.type_name(),
            })?;
            match op {
                BinOp::Lt => o.is_lt(),
                BinOp::Le => o.is_le(),
                BinOp::Gt => o.is_gt(),
                _ => o.is_ge(),
            }
        }
    }))
}

/// Equality. A record equals another by `id`, and equals a plain value that is its `id`.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Map(x), Value::Map(y)) => match (x.get("id"), y.get("id")) {
            (Some(i), Some(j)) => same(i, j),
            _ => a == b,
        },
        (Value::Map(x), other) | (other, Value::Map(x)) => {
            x.get("id").is_some_and(|i| same(i, other))
        }
        _ => a == b,
    }
}

/// The ordering `<` `<=` `>` `>=` use. Null sorts before everything else.
fn order(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        (Value::Null, _) => Some(Ordering::Less),
        (_, Value::Null) => Some(Ordering::Greater),
        _ => None,
    }
}

/// A total order for `sort`: by kind first, then within a kind. `std`'s sort may panic
/// on an order that is not total, so unlike [`order`] this never gives up.
fn sort_order(a: &Value, b: &Value) -> Ordering {
    fn rank(v: &Value) -> u8 {
        match v {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Num(_) => 2,
            Value::Str(_) => 3,
            Value::List(_) => 4,
            Value::Map(_) => 5,
            Value::Masked => 6,
        }
    }
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Num(x), Value::Num(y)) => x.total_cmp(y),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// The items of a list; null is an empty list. Borrowed when `v` is.
fn items<'a>(v: Cow<'a, Value>, stage: &'static str) -> Result<Cow<'a, [Value]>, EvalError> {
    match v {
        Cow::Borrowed(Value::List(l)) => Ok(Cow::Borrowed(l.as_slice())),
        Cow::Owned(Value::List(l)) => Ok(Cow::Owned(l)),
        Cow::Borrowed(Value::Null) | Cow::Owned(Value::Null) => Ok(Cow::Owned(Vec::new())),
        v => Err(EvalError::NeedsList {
            stage,
            got: v.type_name(),
        }),
    }
}

/// A stage's count argument. A negative count, or NaN, saturates to 0.
fn count(n: &Expr, stage: &'static str, s: &dyn Scope) -> Result<usize, EvalError> {
    match &*eval(n, s)? {
        Value::Num(n) => Ok(*n as usize),
        v => Err(EvalError::StageNeedsNumber {
            stage,
            got: v.type_name(),
        }),
    }
}

fn stage<'a>(v: Cow<'a, Value>, st: &Stage, s: &dyn Scope) -> Result<Cow<'a, Value>, EvalError> {
    Ok(match st {
        Stage::Len => Cow::Owned(Value::Num(items(v, "len")?.len() as f64)),
        Stage::First => match items(v, "first")? {
            Cow::Borrowed(l) => l.first().map_or(Cow::Owned(Value::Null), Cow::Borrowed),
            Cow::Owned(l) => Cow::Owned(l.into_iter().next().unwrap_or(Value::Null)),
        },
        Stage::Upper => Cow::Owned(Value::Str(v.display().to_uppercase())),
        Stage::Lower => Cow::Owned(Value::Str(v.display().to_lowercase())),
        Stage::Trim(n) => {
            let n = count(n, "trim", s)?;
            let text = v.display();
            if text.chars().count() <= n {
                Cow::Owned(Value::Str(text))
            } else {
                // The ellipsis takes the last of the `n` characters.
                let mut out: String = text.chars().take(n.saturating_sub(1)).collect();
                if n > 0 {
                    out.push('…');
                }
                Cow::Owned(Value::Str(out))
            }
        }
        Stage::Take(n) => {
            let n = count(n, "take", s)?.min(MAX_LIST);
            Cow::Owned(Value::List(match items(v, "take")? {
                Cow::Borrowed(l) => l.iter().take(n).cloned().collect(),
                Cow::Owned(mut l) => {
                    l.truncate(n);
                    l
                }
            }))
        }
        Stage::Where(p) => {
            let mut out = Vec::new();
            for item in items(v, "where")?.iter() {
                if eval(p, &ItemScope { item, up: s })?.truthy() {
                    out.push(item.clone());
                }
            }
            Cow::Owned(Value::List(out))
        }
        Stage::Sort { key, desc } => {
            let mut l = items(v, "sort")?.into_owned();
            l.sort_by(|a, b| {
                let o = sort_order(sort_key(a, key), sort_key(b, key));
                if *desc { o.reverse() } else { o }
            });
            Cow::Owned(Value::List(l))
        }
        Stage::Before(x) => {
            let x = eval(x, s)?;
            around(items(v, "before")?, &x, false)
        }
        Stage::After(x) => {
            let x = eval(x, s)?;
            around(items(v, "after")?, &x, true)
        }
    })
}

/// The value at `key` inside `v`, or null if any step is missing.
fn sort_key<'a>(v: &'a Value, key: &[String]) -> &'a Value {
    let mut cur = v;
    for k in key {
        cur = match cur {
            Value::Map(m) => m.get(k).unwrap_or(&NULL),
            _ => &NULL,
        };
    }
    cur
}

/// The item before or after `x`, wrapping at the ends. Not found: the first item; an
/// empty list: null (design 27 §7).
fn around<'a>(l: Cow<'a, [Value]>, x: &Value, forward: bool) -> Cow<'a, Value> {
    let n = l.len();
    if n == 0 {
        return Cow::Owned(Value::Null);
    }
    let pick = match l.iter().position(|i| same(i, x)) {
        None => 0,
        Some(p) if forward => (p + 1) % n,
        Some(p) => (p + n - 1) % n,
    };
    match l {
        Cow::Borrowed(l) => l.get(pick).map_or(Cow::Owned(Value::Null), Cow::Borrowed),
        Cow::Owned(l) => Cow::Owned(l.into_iter().nth(pick).unwrap_or(Value::Null)),
    }
}

/// The fixed formatters and status helpers (design 26 §2). `None` if `name` is not one:
/// the scope gets the call instead.
fn builtin(
    name: &str,
    args: &[&Value],
    span: Span,
    s: &dyn Scope,
) -> Option<Result<Value, EvalError>> {
    let (name, f): (&'static str, fn(f64) -> String) = match name {
        "fmt.bytes" => ("fmt.bytes", format::bytes),
        "fmt.rate" => ("fmt.rate", format::rate),
        "fmt.hours" => ("fmt.hours", format::hours),
        "fmt.pct" => ("fmt.pct", format::pct),
        "fmt.date" => ("fmt.date", format::date),
        "fmt.eta" => ("fmt.eta", format::eta),
        "fmt.ago" => {
            return Some(one_arg("fmt.ago", args, span).and_then(|v| {
                Ok(num_arg("fmt.ago", v, span)?
                    .map_or(Value::Null, |t| Value::Str(format::ago(s.now() - t))))
            }));
        }
        "status.word" => {
            return Some(
                status_arg("status.word", args, span).map(|st| Value::Str(st.word().to_string())),
            );
        }
        "status.tone" => {
            return Some(
                status_arg("status.tone", args, span).map(|st| Value::Str(st.tone().to_string())),
            );
        }
        _ => return None,
    };
    Some(
        one_arg(name, args, span)
            .and_then(|v| Ok(num_arg(name, v, span)?.map_or(Value::Null, |n| Value::Str(f(n))))),
    )
}

/// A number argument of a formatter; null passes through as `None`.
fn num_arg(name: &'static str, v: &Value, span: Span) -> Result<Option<f64>, EvalError> {
    match v {
        Value::Num(n) => Ok(Some(*n)),
        Value::Null => Ok(None),
        v => Err(EvalError::FmtNeedsNumber {
            name,
            got: v.type_name(),
            span,
        }),
    }
}

fn one_arg<'a>(name: &str, args: &[&'a Value], span: Span) -> Result<&'a Value, EvalError> {
    match args {
        [v] => Ok(v),
        _ => Err(EvalError::Arity {
            name: name.to_string(),
            expected: 1,
            got: args.len(),
            span,
        }),
    }
}

fn status_arg(name: &str, args: &[&Value], span: Span) -> Result<Status, EvalError> {
    let v = one_arg(name, args, span)?;
    v.as_str()
        .and_then(Status::from_name)
        .ok_or_else(|| EvalError::UnknownStatus {
            name: v.display(),
            span,
        })
}

//! Evaluation of a parsed [`Expr`]. Total: no assignment, no I/O, no user functions, and
//! the work is bounded by the tree and the lists the scope returns. **Nothing here
//! parses text**: the loader parses once (design 27 §9 step 3).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use gogo_contract::{Status, Value};

use super::MAX_LIST;
use super::error::{CallError, EvalError};
use super::format;
use super::parse::{BinOp, Expr, Stage};

/// What expressions can read. The binder implements it.
pub trait Scope {
    /// A bound variable (`let`, a parameter, an input variable).
    fn var(&self, name: &str) -> Option<Value>;
    /// A route parameter, `$name`.
    fn param(&self, name: &str) -> Option<Value>;
    /// A token, `@name`: a colour (`#rrggbb[aa]`) or any other token value.
    fn token(&self, name: &str) -> Option<Value>;
    /// A source or helper call such as `game($id)`; `None` if the name is unknown. The
    /// fixed formatters and status helpers are handled before the scope is asked.
    fn call(&self, name: &str, args: &[Value]) -> Option<Result<Value, CallError>>;
    /// "Now" for `fmt.ago`, in unix seconds.
    fn now(&self) -> f64;
}

/// A scope that sees an item's fields first (for `where`).
struct ItemScope<'a> {
    item: &'a Value,
    up: &'a dyn Scope,
}

impl Scope for ItemScope<'_> {
    fn var(&self, name: &str) -> Option<Value> {
        if let Value::Map(m) = self.item
            && let Some(v) = m.get(name)
        {
            return Some(v.clone());
        }
        self.up.var(name)
    }
    fn param(&self, name: &str) -> Option<Value> {
        self.up.param(name)
    }
    fn token(&self, name: &str) -> Option<Value> {
        self.up.token(name)
    }
    fn call(&self, name: &str, args: &[Value]) -> Option<Result<Value, CallError>> {
        self.up.call(name, args)
    }
    fn now(&self) -> f64 {
        self.up.now()
    }
}

/// Evaluate a parsed expression.
pub fn eval(e: &Expr, s: &dyn Scope) -> Result<Value, EvalError> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Name(n) => s.var(n).unwrap_or_else(|| Value::Str(n.clone())),
        Expr::Param(p) => s
            .param(p)
            .ok_or_else(|| EvalError::UnknownParam(p.clone()))?,
        Expr::Token { name, alpha } => token(name, alpha.as_deref(), s)?,
        Expr::Field {
            base,
            name,
            optional,
        } => field(eval(base, s)?, name, *optional)?,
        Expr::Call { name, args } => call(name, args, s)?,
        Expr::Not(e) => Value::Bool(!eval(e, s)?.truthy()),
        Expr::Neg(e) => match eval(e, s)? {
            Value::Num(n) => Value::Num(-n),
            v => return Err(EvalError::NegNeedsNumber(v.type_name())),
        },
        Expr::Bin(BinOp::And, a, b) => Value::Bool(eval(a, s)?.truthy() && eval(b, s)?.truthy()),
        Expr::Bin(BinOp::Or, a, b) => Value::Bool(eval(a, s)?.truthy() || eval(b, s)?.truthy()),
        Expr::Bin(op, a, b) => compare(*op, &eval(a, s)?, &eval(b, s)?)?,
        Expr::Ternary(c, a, b) => {
            if eval(c, s)?.truthy() {
                eval(a, s)?
            } else {
                eval(b, s)?
            }
        }
        Expr::Else(a, b) => match eval(a, s)? {
            Value::Null => eval(b, s)?,
            v => v,
        },
        Expr::Pipe(base, st) => stage(eval(base, s)?, st, s)?,
    })
}

fn token(name: &str, alpha: Option<&Expr>, s: &dyn Scope) -> Result<Value, EvalError> {
    let v = s
        .token(name)
        .ok_or_else(|| EvalError::UnknownToken(name.to_string()))?;
    let Some(a) = alpha else { return Ok(v) };
    let a = eval(a, s)?.as_num().ok_or(EvalError::AlphaNotNumber)?;
    format::with_alpha(&v, a).ok_or_else(|| EvalError::NotAColour(name.to_string()))
}

fn field(base: Value, name: &str, optional: bool) -> Result<Value, EvalError> {
    match (&base, optional) {
        (Value::Null, true) => Ok(Value::Null),
        (Value::Map(m), _) => m.get(name).cloned().ok_or_else(|| EvalError::NoField {
            name: name.to_string(),
            fields: keys(m),
        }),
        (Value::List(l), _) if name == "len" => Ok(Value::Num(l.len() as f64)),
        _ => Err(EvalError::FieldOnType {
            name: name.to_string(),
            ty: base.type_name(),
        }),
    }
}

fn call(name: &str, args: &[Expr], s: &dyn Scope) -> Result<Value, EvalError> {
    let args = args
        .iter()
        .map(|a| eval(a, s))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(v) = builtin(name, &args, s) {
        return v;
    }
    match s.call(name, &args) {
        Some(Ok(v)) => Ok(v),
        Some(Err(source)) => Err(EvalError::Call {
            name: name.to_string(),
            source,
        }),
        None => Err(EvalError::UnknownCall(name.to_string())),
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

fn keys(m: &BTreeMap<String, Value>) -> String {
    m.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
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

fn list(v: Value, stage: &'static str) -> Result<Vec<Value>, EvalError> {
    match v {
        Value::List(l) => Ok(l),
        Value::Null => Ok(Vec::new()),
        v => Err(EvalError::NeedsList {
            stage,
            got: v.type_name(),
        }),
    }
}

/// A stage's count argument. A negative count, or NaN, saturates to 0.
fn count(n: &Expr, stage: &'static str, s: &dyn Scope) -> Result<usize, EvalError> {
    match eval(n, s)? {
        Value::Num(n) => Ok(n as usize),
        v => Err(EvalError::StageNeedsNumber {
            stage,
            got: v.type_name(),
        }),
    }
}

fn stage(v: Value, st: &Stage, s: &dyn Scope) -> Result<Value, EvalError> {
    Ok(match st {
        Stage::Len => Value::Num(list(v, "len")?.len() as f64),
        Stage::First => list(v, "first")?.into_iter().next().unwrap_or(Value::Null),
        Stage::Upper => Value::Str(v.display().to_uppercase()),
        Stage::Lower => Value::Str(v.display().to_lowercase()),
        Stage::Trim(n) => {
            let n = count(n, "trim", s)?;
            let text = v.display();
            if text.chars().count() <= n {
                Value::Str(text)
            } else {
                // The ellipsis takes the last of the `n` characters.
                let mut out: String = text.chars().take(n.saturating_sub(1)).collect();
                if n > 0 {
                    out.push('…');
                }
                Value::Str(out)
            }
        }
        Stage::Take(n) => {
            let n = count(n, "take", s)?;
            let n = n.min(MAX_LIST);
            Value::List(list(v, "take")?.into_iter().take(n).collect())
        }
        Stage::Where(p) => {
            let mut out = Vec::new();
            for item in list(v, "where")? {
                if eval(p, &ItemScope { item: &item, up: s })?.truthy() {
                    out.push(item);
                }
            }
            Value::List(out)
        }
        Stage::Sort { key, desc } => {
            let mut l = list(v, "sort")?;
            l.sort_by(|a, b| {
                let o = sort_order(sort_key(a, key), sort_key(b, key));
                if *desc { o.reverse() } else { o }
            });
            Value::List(l)
        }
        Stage::Before(x) => around(list(v, "before")?, &eval(x, s)?, false),
        Stage::After(x) => around(list(v, "after")?, &eval(x, s)?, true),
    })
}

static NULL: Value = Value::Null;

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
fn around(l: Vec<Value>, x: &Value, forward: bool) -> Value {
    let n = l.len();
    let pick = match l.iter().position(|i| same(i, x)) {
        None => 0,
        Some(p) if forward => (p + 1) % n,
        Some(p) => (p + n - 1) % n,
    };
    l.into_iter().nth(pick).unwrap_or(Value::Null)
}

/// A number argument of a formatter; null passes through as `None`.
fn num_arg(name: &'static str, v: &Value) -> Result<Option<f64>, EvalError> {
    match v {
        Value::Num(n) => Ok(Some(*n)),
        Value::Null => Ok(None),
        v => Err(EvalError::FmtNeedsNumber {
            name,
            got: v.type_name(),
        }),
    }
}

/// The fixed formatters and status helpers (design 26 §2). `None` if `name` is not one:
/// the scope gets the call instead.
fn builtin(name: &str, args: &[Value], s: &dyn Scope) -> Option<Result<Value, EvalError>> {
    let (name, f): (&'static str, fn(f64) -> String) = match name {
        "fmt.bytes" => ("fmt.bytes", format::bytes),
        "fmt.rate" => ("fmt.rate", format::rate),
        "fmt.hours" => ("fmt.hours", format::hours),
        "fmt.pct" => ("fmt.pct", format::pct),
        "fmt.date" => ("fmt.date", format::date),
        "fmt.eta" => ("fmt.eta", format::eta),
        "fmt.ago" => {
            return Some(one_arg("fmt.ago", args).and_then(|v| {
                Ok(num_arg("fmt.ago", v)?
                    .map_or(Value::Null, |t| Value::Str(format::ago(s.now() - t))))
            }));
        }
        "status.word" => {
            return Some(
                status_arg("status.word", args).map(|st| Value::Str(st.word().to_string())),
            );
        }
        "status.tone" => {
            return Some(
                status_arg("status.tone", args).map(|st| Value::Str(st.tone().to_string())),
            );
        }
        _ => return None,
    };
    Some(
        one_arg(name, args)
            .and_then(|v| Ok(num_arg(name, v)?.map_or(Value::Null, |n| Value::Str(f(n))))),
    )
}

fn one_arg<'a>(name: &str, args: &'a [Value]) -> Result<&'a Value, EvalError> {
    match args {
        [v] => Ok(v),
        _ => Err(EvalError::Arity {
            name: name.to_string(),
            expected: 1,
            got: args.len(),
        }),
    }
}

fn status_arg(name: &str, args: &[Value]) -> Result<Status, EvalError> {
    let v = one_arg(name, args)?;
    v.as_str()
        .and_then(Status::from_name)
        .ok_or_else(|| EvalError::UnknownStatus(v.display()))
}

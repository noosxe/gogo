//! Source values, built field by field from typed data (design 26 §3): nothing from
//! gogo-core is serialised as it is.

use std::collections::BTreeMap;

use gogo::gog::catalog::Os;
use gogo_contract::{ContractVersion, FieldType, Value, shapes};

use super::text;

pub(crate) fn str(s: impl Into<String>) -> Value {
    Value::str(s)
}

/// Server text on one line.
pub(crate) fn line(s: &str) -> Value {
    Value::Str(text::line(s))
}

pub(crate) fn num(n: u64) -> Value {
    Value::Num(n as f64)
}

pub(crate) fn opt(v: Option<Value>) -> Value {
    v.unwrap_or(Value::Null)
}

pub(crate) fn list(items: impl IntoIterator<Item = Value>) -> Value {
    Value::List(items.into_iter().collect())
}

pub(crate) fn map<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Map(BTreeMap::from(fields.map(|(k, v)| (k.to_string(), v))))
}

pub(crate) fn os_word(os: Os) -> Value {
    str(match os {
        Os::Windows => "windows",
        Os::Mac => "mac",
        Os::Linux => "linux",
    })
}

/// The art of a product: handles into the app's cover store, never addresses (S3).
pub(crate) fn art(id: u64) -> Value {
    map([
        ("cover", str(format!("art:{id}:cover"))),
        ("background", str(format!("art:{id}:background"))),
        ("logo", str(format!("art:{id}:logo"))),
        ("colors", list([str("#2a2f3a"), str("#14161b")])),
    ])
}

/// The `account` source.
pub(crate) fn account(signed_in: bool, username: Option<&str>) -> Value {
    map([
        ("signed_in", Value::Bool(signed_in)),
        ("username", opt(username.map(line))),
        ("avatar", Value::Null),
    ])
}

/// The `app` source.
pub(crate) fn app() -> Value {
    map([
        ("version", str(env!("CARGO_PKG_VERSION"))),
        ("contract", str(ContractVersion::CURRENT.to_string())),
        (
            "os",
            os_word(if cfg!(target_os = "macos") {
                Os::Mac
            } else {
                Os::Linux
            }),
        ),
        ("reduced_motion", Value::Bool(false)),
        ("high_contrast", Value::Bool(false)),
        ("big_screen", Value::Bool(false)),
        ("locale", str("en-US")),
    ])
}

/// A well-shaped value that says nothing, for a source with no data: empty lists and
/// text, zeroes, no optionals.
pub(crate) fn empty(ty: &FieldType) -> Value {
    match ty {
        FieldType::Bool => Value::Bool(false),
        FieldType::Number
        | FieldType::Bytes
        | FieldType::Time
        | FieldType::Duration
        | FieldType::Fraction => Value::Num(0.0),
        FieldType::OneOf(words) => str(words.first().copied().unwrap_or_default()),
        FieldType::Status => str("not_installed"),
        FieldType::Os => str("linux"),
        FieldType::Color => str("#000000"),
        FieldType::List(_) => Value::List(Vec::new()),
        FieldType::Record(shape) => Value::Map(
            shape
                .all_fields()
                .into_iter()
                .map(|f| (f.name.to_string(), empty(&f.ty)))
                .collect(),
        ),
        FieldType::Option(_) | FieldType::ActionRef | FieldType::Opaque(_) | FieldType::Ui => {
            Value::Null
        }
        _ => Value::Str(String::new()),
    }
}

/// What `source` says when there is nothing to say. Where a first word would claim
/// something good ("in sync"), the careful one is given instead.
pub(crate) fn nothing(source: &str) -> Value {
    let mut value = shapes::source_shape(source).map_or(Value::Null, |t| empty(&t));
    if let Value::Map(fields) = &mut value {
        match source {
            "game.saves" => {
                fields.insert("state".to_string(), str("disabled"));
                fields.insert("checked_at".to_string(), Value::Null);
            }
            "game.runner" => {
                let platform = if cfg!(target_os = "macos") {
                    "mac"
                } else {
                    "linux"
                };
                fields.insert("platform".to_string(), str(platform));
            }
            _ => {}
        }
    }
    value
}

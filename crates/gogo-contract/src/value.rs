//! The dynamic value that flows from a source or a token through an expression into the
//! IR (design 26 §2: the types a skin sees, design 27 §7: expressions over them).
//!
//! It is the one runtime value type of the skin system. [`ValueType`](crate::ValueType)
//! names the contract's static types; a [`Value`] is what a field of that type holds at
//! run time. Bytes, times, durations and fractions are all [`Value::Num`]; the formatters
//! (`fmt.bytes`, `fmt.ago`, …) give them their meaning.

use std::collections::BTreeMap;

/// A value from a source, a token or an expression.
///
/// With the `serde` feature it serializes as plain JSON-like data (the dev inspector).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(untagged))]
pub enum Value {
    /// No value: an absent optional, or a field that is `null`.
    Null,
    Bool(bool),
    Num(f64),
    /// Text, also the representation of enum words, colours and ids.
    Str(String),
    List(Vec<Value>),
    /// A record. Keys are in sorted order, so iteration is deterministic.
    Map(BTreeMap<String, Value>),
    /// A secret: the clear value never enters the binding scope (S2).
    #[cfg_attr(feature = "serde", serde(serialize_with = "serialize_masked"))]
    Masked,
}

#[cfg(feature = "serde")]
fn serialize_masked<S: serde::Serializer>(s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut m = s.serialize_map(Some(1))?;
    m.serialize_entry("masked", &true)?;
    m.end()
}

impl Value {
    /// A text value.
    pub fn str(s: impl Into<String>) -> Self {
        Value::Str(s.into())
    }

    /// Whether the value counts as true in `and`, `or`, `not` and `?:`: null, `false`,
    /// zero, empty text and empty lists are false; everything else is true.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Map(_) | Value::Masked => true,
        }
    }

    /// The number, if this is one.
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// The text, if this is some.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The name of the kind of value, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Num(_) => "number",
            Value::Str(_) => "text",
            Value::List(_) => "list",
            Value::Map(_) => "record",
            Value::Masked => "masked",
        }
    }

    /// How a value reads when interpolated into text. A masked value never shows its
    /// content, because it has none.
    pub fn display(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => fmt_num(*n),
            Value::Str(s) => s.clone(),
            Value::List(l) => l.iter().map(Value::display).collect::<Vec<_>>().join(", "),
            Value::Map(_) => "[record]".into(),
            Value::Masked => "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}".into(),
        }
    }
}

/// A number as text: whole numbers without a decimal point, others with at most three
/// decimals and no trailing zeros.
pub fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{n:.3}");
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness() {
        assert!(!Value::Null.truthy());
        assert!(!Value::Bool(false).truthy());
        assert!(!Value::Num(0.0).truthy());
        assert!(!Value::str("").truthy());
        assert!(!Value::List(vec![]).truthy());
        assert!(Value::Num(-1.0).truthy());
        assert!(Value::str("x").truthy());
        assert!(Value::List(vec![Value::Null]).truthy());
        assert!(Value::Map(BTreeMap::new()).truthy());
        assert!(Value::Masked.truthy());
    }

    #[test]
    fn accessors_and_names() {
        assert_eq!(Value::Num(2.0).as_num(), Some(2.0));
        assert_eq!(Value::str("a").as_num(), None);
        assert_eq!(Value::str("a").as_str(), Some("a"));
        assert_eq!(Value::Null.as_str(), None);
        let names: Vec<_> = [
            Value::Null,
            Value::Bool(true),
            Value::Num(1.0),
            Value::str(""),
            Value::List(vec![]),
            Value::Map(BTreeMap::new()),
            Value::Masked,
        ]
        .iter()
        .map(Value::type_name)
        .collect();
        assert_eq!(
            names,
            ["null", "bool", "number", "text", "list", "record", "masked"]
        );
    }

    #[test]
    fn numbers_read_cleanly() {
        assert_eq!(fmt_num(3.0), "3");
        assert_eq!(fmt_num(-3.0), "-3");
        assert_eq!(fmt_num(0.5), "0.5");
        assert_eq!(fmt_num(1.23456), "1.235");
        assert_eq!(fmt_num(2.1), "2.1");
        assert_eq!(fmt_num(1e20), "100000000000000000000");
        assert_eq!(fmt_num(f64::NAN), "NaN");
        assert_eq!(fmt_num(f64::INFINITY), "inf");
    }

    #[test]
    fn display_of_each_kind() {
        assert_eq!(Value::Null.display(), "");
        assert_eq!(Value::Bool(true).display(), "true");
        assert_eq!(Value::Num(7.0).display(), "7");
        assert_eq!(
            Value::List(vec![Value::Num(1.0), Value::str("b")]).display(),
            "1, b"
        );
        assert_eq!(Value::Map(BTreeMap::new()).display(), "[record]");
        assert!(!Value::Masked.display().is_empty());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serializes_as_plain_data() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), Value::Num(1.5));
        m.insert("b".to_string(), Value::Masked);
        let v = Value::List(vec![
            Value::Null,
            Value::Bool(true),
            Value::str("x"),
            Value::Map(m),
        ]);
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, r#"[null,true,"x",{"a":1.5,"b":{"masked":true}}]"#);
    }
}

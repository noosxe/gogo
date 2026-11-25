//! A skin's tokens as the dynamic values expressions and style properties read
//! (design 27 §3, §7; design 26 §6.1).

use std::collections::BTreeMap;

use gogo_contract::{Status, Value};

use crate::tokens::{Rgba, TokenSet, TokenValue};

/// The tokens of one mode by name. A colour is its `#rrggbb` (or `#rrggbbaa`) text, a
/// number a number, a type role a record `{family, size, weight, stretch?}`, an easing
/// curve its name.
///
/// The fields of a type role are tokens too (`font.body.size`), and a status tone the
/// skin leaves out reads as its fallback (design 26 §6.1).
pub(crate) type Values = BTreeMap<String, Value>;

pub(crate) fn values(set: &TokenSet) -> Values {
    let mut out = Values::new();
    for (name, t) in set.iter() {
        match &t.value {
            TokenValue::Colour(c) => {
                out.insert(name.to_owned(), Value::Str(hex(*c)));
            }
            TokenValue::Number(n) => {
                out.insert(name.to_owned(), Value::Num(*n));
            }
            TokenValue::Text(s) => {
                out.insert(name.to_owned(), Value::Str(s.clone()));
            }
            TokenValue::Font(f) => {
                let mut m = BTreeMap::new();
                m.insert("family".to_owned(), Value::Str(f.family.clone()));
                m.insert("size".to_owned(), Value::Num(f.size));
                m.insert("weight".to_owned(), Value::Num(f.weight));
                if let Some(s) = f.stretch {
                    m.insert("stretch".to_owned(), Value::Num(s));
                }
                out.insert(format!("{name}.family"), Value::Str(f.family.clone()));
                out.insert(format!("{name}.size"), Value::Num(f.size));
                out.insert(format!("{name}.weight"), Value::Num(f.weight));
                out.insert(name.to_owned(), Value::Map(m));
            }
        }
    }
    for s in Status::ALL {
        if let Some(fallback) = s.tone_fallback()
            && !out.contains_key(s.tone())
            && let Some(v) = out.get(fallback).cloned()
        {
            out.insert(s.tone().to_owned(), v);
        }
    }
    out
}

/// `#rrggbb`, or `#rrggbbaa` when not opaque.
pub(crate) fn hex(c: Rgba) -> String {
    if c.a == 255 {
        format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
    }
}

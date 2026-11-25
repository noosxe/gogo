//! The skin's own state: the values of the manifest's `ui { var … }` (design 26 §6.4,
//! design 27 §2).

use std::collections::BTreeMap;

use gogo_contract::Value;

use crate::manifest::{Manifest, UiDefault, UiType};

/// The current value of each `ui` variable of one skin.
///
/// Only `ui.set` from user input writes a variable (S4, S20), and the values are only
/// enum words, booleans and numbers, never data from a source (design 26 §6.4). The app
/// persists this per skin (design 25 §9) and hands it to the binder, which exposes it to
/// expressions as the `ui` source.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UiState {
    values: BTreeMap<String, Value>,
}

/// Why a value is not one of a `ui` variable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UiStateError {
    #[error("the skin has no `ui` variable `{0}`")]
    UnknownVar(String),
    #[error("`{value}` is not a value of `ui.{var}`: {why}")]
    BadValue {
        var: String,
        value: String,
        why: String,
    },
}

impl UiState {
    /// Every variable at the default its manifest declares.
    pub fn defaults(manifest: &Manifest) -> Self {
        UiState {
            values: manifest
                .ui
                .iter()
                .map(|v| (v.name.clone(), default_value(&v.default)))
                .collect(),
        }
    }

    /// The state saved earlier. A variable that is missing, or whose saved value no longer
    /// fits its declaration, takes the declared default; a saved variable the manifest no
    /// longer declares is dropped (design 25 §9: unreadable state falls back to the
    /// defaults).
    pub fn restore(manifest: &Manifest, saved: impl IntoIterator<Item = (String, Value)>) -> Self {
        let mut s = UiState::defaults(manifest);
        for (name, value) in saved {
            let _ = s.set(manifest, &name, value);
        }
        s
    }

    /// The value of `name`.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// Every variable and its value.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Set a variable, as `ui.set` does.
    ///
    /// # Errors
    /// [`UiStateError`] if the manifest has no such variable or the value is not one of
    /// its type. The state is unchanged then.
    pub fn set(
        &mut self,
        manifest: &Manifest,
        name: &str,
        value: Value,
    ) -> Result<(), UiStateError> {
        let var = manifest
            .ui
            .iter()
            .find(|v| v.name == name)
            .ok_or_else(|| UiStateError::UnknownVar(name.to_owned()))?;
        if let Some(why) = why_not(&var.ty, &value) {
            return Err(UiStateError::BadValue {
                var: name.to_owned(),
                value: value.display(),
                why,
            });
        }
        self.values.insert(name.to_owned(), value);
        Ok(())
    }

    /// The state as the `ui` source: a record of the variables.
    pub(crate) fn as_value(&self) -> Value {
        Value::Map(self.values.clone())
    }
}

fn default_value(d: &UiDefault) -> Value {
    match d {
        UiDefault::Bool(b) => Value::Bool(*b),
        UiDefault::Number(n) => Value::Num(*n),
        UiDefault::Word(w) => Value::Str(w.clone()),
    }
}

/// Why `value` is not a value of `ty`, if it isn't.
fn why_not(ty: &UiType, value: &Value) -> Option<String> {
    match (ty, value) {
        (UiType::Bool, Value::Bool(_)) | (UiType::Number, Value::Num(_)) => None,
        (UiType::Enum(words), Value::Str(s)) if words.contains(s) => None,
        (UiType::Section, Value::Str(s))
            if gogo_contract::routes::ROUTES
                .iter()
                .any(|r| r.sections.contains(&s.as_str())) =>
        {
            None
        }
        (UiType::Bool, _) => Some("it is a bool".to_owned()),
        (UiType::Number, _) => Some("it is a number".to_owned()),
        (UiType::Enum(words), _) => Some(format!("its values are {}", words.join(", "))),
        (UiType::Section, _) => Some("it is one of the contract's sections".to_owned()),
    }
}

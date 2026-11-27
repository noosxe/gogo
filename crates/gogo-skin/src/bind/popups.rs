//! What a `menu` or a `select` opens (design 27 §5.3): the tree holds only the control,
//! and the app draws the popup from these.

use gogo_contract::Value;
use gogo_ui_ir::{ActionRef, NodeId};

/// The popup behind one control of the tree.
#[derive(Debug, Clone)]
pub struct PopupSlot {
    /// The `menu` or `select` node.
    pub node: NodeId,
    pub content: PopupContent,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PopupContent {
    /// A `menu`'s items whose `when` holds, in document order.
    Menu(Vec<MenuEntry>),
    /// A `select`'s options, in the order the skin or the data gave them. Choosing one
    /// runs the node's own action with that option as `{value}`.
    Select(Vec<SelectOption>),
}

/// One `item` of a `menu`.
#[derive(Debug, Clone)]
pub struct MenuEntry {
    /// The visible text.
    pub label: String,
    /// The accessible name: `label=`, or the text (S14).
    pub name: String,
    pub icon: Option<String>,
    /// A key hint to show. It is not a shortcut.
    pub key: Option<String>,
    /// What choosing the item does; `None` for an item without an action.
    pub action: Option<ActionRef>,
}

/// One option of a `select`.
#[derive(Debug, Clone)]
pub struct SelectOption {
    /// What `{value}` is when this one is chosen: an option's `id`, or the option itself.
    pub value: Value,
    pub label: String,
    /// Whether the select shows this one.
    pub current: bool,
}

/// The options of a select whose value is `raw`.
pub(crate) fn select_options(raw: &Value, options: &[Value]) -> Vec<SelectOption> {
    let id_of = |v: &Value| match v {
        Value::Map(m) => m.get("id").cloned().unwrap_or(Value::Null),
        v => v.clone(),
    };
    let chosen = id_of(raw).display();
    options
        .iter()
        .map(|option| {
            let value = id_of(option);
            let id = value.display();
            let label = match option {
                Value::Map(m) => m.get("name").map_or_else(|| id.clone(), Value::display),
                _ => id.clone(),
            };
            // Without regard to case: a skin may show `Wayland` for the word `wayland`.
            let current = !chosen.is_empty() && id.eq_ignore_ascii_case(&chosen);
            SelectOption {
                value,
                label,
                current,
            }
        })
        .collect()
}

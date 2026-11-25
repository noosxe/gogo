//! What assistive technology is told about a node (design 24 §2; design 27 §5.3 on names).

/// What a node is, for assistive technology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Role {
    /// No semantics of its own. The default; a screen reader skips it.
    #[default]
    Generic,
    Group,
    Region,
    Heading,
    /// Static text.
    Label,
    Image,
    Button,
    /// An on/off control; see [`A11y::checked`].
    Switch,
    ComboBox,
    TabList,
    Tab,
    MenuButton,
    Table,
    Row,
    Cell,
    /// A live readout, announced when it changes.
    Status,
    /// A progress bar; its value is [`A11y::value`].
    ProgressBar,
    TextInput,
}

/// A node's accessibility facts. [`Default`] is a plain, unnamed, non-focusable generic
/// node.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct A11y {
    pub role: Role,
    /// The accessible name. `None`: the node's own text, if any.
    pub name: Option<String>,
    /// The current value as text (a select's choice, a progress percentage).
    pub value: Option<String>,
    /// Reachable with the keyboard, in document order.
    pub focusable: bool,
    /// An on/off state. `None`: the node has none.
    pub checked: Option<bool>,
    /// A selected tab, row or option. `None`: the node has no such state.
    pub selected: Option<bool>,
    /// Not operable: it cannot be focused or activated, and is announced as unavailable.
    pub disabled: bool,
    /// The keyboard shortcut announced with the node (`P`, `Enter`).
    pub key: Option<String>,
}

impl A11y {
    /// A focusable node of `role` with a name.
    pub fn control(role: Role, name: impl Into<String>) -> Self {
        A11y {
            role,
            name: Some(name.into()),
            focusable: true,
            ..Self::default()
        }
    }
}

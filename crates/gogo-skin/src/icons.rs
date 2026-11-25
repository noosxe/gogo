//! The icon names a skin may use (design 27 §5.2): vector icons shipped with the engine.
//!
//! The shapes belong to the scene crate that draws them. The loader only needs the names,
//! to reject an unknown one at load.
// follow-up: take the names from the icon set once the scene crate owns it.

/// Every icon name, in the order design 27 lists them.
pub const ICON_NAMES: &[&str] = &["play", "square", "circle", "chevron-down", "dot"];

/// Whether `name` is an icon the engine ships.
pub fn is_icon(name: &str) -> bool {
    ICON_NAMES.contains(&name)
}

//! gogo-skins: built-in skins as data (docs/design/24-gui-architecture.md §2).
//!
//! Built-in skins as data, embedded with `include_str!` and loaded through the same public
//! loader as any other skin (S17). The Default skin lands in PR B5 (#527).
#![deny(unsafe_code)]

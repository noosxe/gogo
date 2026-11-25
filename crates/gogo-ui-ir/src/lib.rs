//! gogo-ui-ir: the resolved, renderer-neutral tree between a skin and the engine (docs/design/24-gui-architecture.md §2).
//!
//! The resolved tree: the boundary between "a skin" and "an engine" (design 24 §2).
//! Layout styles, paint, effects, text runs, accessibility roles, action references and
//! whether a node is drawn by the app or the skin. Units: logical px, borders take layout
//! space, blur radii follow CSS (design 27 §6). Filled in by PR B4 (#526).
#![deny(unsafe_code)]

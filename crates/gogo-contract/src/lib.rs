//! gogo-contract: the skin contract as typed data: sources, actions, routes, app widgets, tokens, invariants (docs/design/24-gui-architecture.md §2).
//!
//! The skin contract (design 26) as typed data, with no I/O: value types, the source and
//! action catalogues (each action's kind and tier), routes and sections, app-owned widgets
//! with their parts, variants and depth, semantic tokens and the safety invariant ids.
//! Filled in by PR B2 (#524).
#![deny(unsafe_code)]

//! gogo-skin: the KDL skin format: load, validate, bind to the IR (docs/design/24-gui-architecture.md §2).
//!
//! The skin format (design 27): loading a skin package, validating it against the contract,
//! the expression language, and binding a route to the IR. No rendering, layout, text,
//! window or async dependency (design 24 §2.1 rule 2). Filled in by PRs B3 (#525) and
//! B4 (#526).
#![deny(unsafe_code)]

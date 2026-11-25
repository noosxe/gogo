//! The skin format (design 27): loading a skin package, validating it against the contract,
//! the expression language, and binding a route to the IR. No rendering, layout, text,
//! window or async dependency (design 24 §2.1 rule 2).
//!
//! - [`expr`]: the expression language (design 27 §7), added by PR B3a: parsed once at
//!   load, evaluated many times.
//!
//! Filled in further by PRs B3 (#525) and B4 (#526).
#![deny(unsafe_code)]

pub mod expr;

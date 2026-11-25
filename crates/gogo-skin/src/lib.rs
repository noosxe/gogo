//! The skin format (design 27): loading a skin package, validating it against the contract,
//! the expression language, and binding a route to the IR. No rendering, layout, text,
//! window or async dependency (design 24 §2.1 rule 2).
//!
//! - [`expr`]: the expression language (design 27 §7), added by PR B3a: parsed once at
//!   load, evaluated many times.
//! - [`SkinPackage`]: the loader and structural validator (design 27 §1–§6, §8, §9),
//!   added by PR B3b. [`SkinPackage::load`] reads a folder and
//!   [`SkinPackage::from_files`] takes the files of a zip; both run one validation path
//!   and return a [`Report`] of [`Finding`]s, each a [`miette::Diagnostic`] with a stable
//!   [`Code`].
//! - [`ast`]: the skin's own tree, with spans, in which every template and expression is
//!   already parsed.
//!
//! - The contract check (PR B3c, in `resolve` and `typing`): every name resolves once to
//!   its `&'static` contract spec, every field path is checked against the contract's
//!   shapes, and the types an element needs are checked (design 27 §7).
//!
//! Binding and the typed styles are PR B4's.
//!
//! Filled in further by PRs B3 (#525) and B4 (#526).
#![deny(unsafe_code)]

pub mod assets;
pub mod ast;
pub mod diag;
pub mod expr;
pub mod fonts;
pub mod icons;
pub mod limits;
pub mod manifest;
pub mod package;
pub mod source;
pub mod styles;
pub mod tokens;

mod elements;
mod names;
mod props;
mod raw;
mod resolve;
mod typing;

pub use assets::{Asset, AssetKind};
pub use ast::{Arg, Node, Prop, Term, Val};
pub use diag::{Code, Finding, Level, Report};
pub use manifest::{Manifest, Mode, SkinVersion, UiDefault, UiType, UiVar};
pub use package::{Component, Route, SkinPackage};
pub use source::{SourceFile, Sources, Span};
pub use styles::{State, Style};
pub use tokens::{FontToken, Rgba, Token, TokenKind, TokenSet, TokenValue};

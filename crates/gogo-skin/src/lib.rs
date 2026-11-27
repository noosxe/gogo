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
//! - [`bind`] (PR B4b): [`SkinPackage::bind`] evaluates a route against the app's data and
//!   the skin's `ui` state into the IR tree and the tables behind its ids, with per-route
//!   fallback to the default skin (design 26 §8).
#![deny(unsafe_code)]

pub mod assets;
pub mod ast;
pub mod bind;
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
pub use ast::{Arg, Node, Operand, Prop, Term, Val};
pub use bind::{
    ActionCall, ActionTable, ArgValue, BindError, BindErrorKind, Binding, Bound, CallArg,
    DefaultSkin, Fallback, FallbackWhy, Input, Lists, MAX_ITEMS, OVERSCAN, PartStyle, PendingArg,
    PendingError, UiState, UiStateError, WidgetSlot,
};
pub use diag::{Code, Finding, Level, Report};
/// The dynamic value expressions compute with: what a source answers and a scope lends.
pub use gogo_contract::Value;
/// The IR the binder builds (design 24 §2), for code that only depends on this crate.
pub use gogo_ui_ir as ir;
pub use manifest::{Manifest, Mode, SkinVersion, UiDefault, UiType, UiVar};
pub use package::{Component, Route, SkinPackage};
pub use source::{SourceFile, Sources, Span};
pub use styles::{State, Style};
pub use tokens::{FontToken, Rgba, Token, TokenKind, TokenSet, TokenValue};

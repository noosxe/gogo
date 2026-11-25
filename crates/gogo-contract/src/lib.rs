//! The skin contract (design 26) as typed data: what the app guarantees every skin, and
//! what every skin has to go through.
//!
//! - [`version`]: the contract version and which skin versions it accepts (§8).
//! - [`types`]: the value types a skin sees (§2), [`Status`] and [`Os`].
//! - [`sources`]: the data sources (§3).
//! - [`actions`]: the actions with their kind and tier (§4), and how the plan decides the
//!   tier an action really gets.
//! - [`settings`]: the keys a skin may set (§4.9, S21).
//! - [`widgets`]: the app-owned widgets with their depth, parts and variants (§5).
//! - [`tokens`], [`routes`]: semantic tokens and routes (§6).
//! - [`value`]: the dynamic [`Value`] an expression computes with (§2, design 27 §7).
//! - [`invariants`]: the safety invariants S1–S21 (§7).
//!
//! Lookups by name (`action`, `source`, `widget_spec`, …) scan their tables. The
//! validator resolves every name once at load to a `&'static` spec (S16), so binding a
//! route never looks a name up per frame.
//!
//! No I/O. The tables are `static` data; `tests/doc26.rs` checks them against the doc,
//! row by row. Serialization is behind the `serde` feature.
#![deny(unsafe_code)]

pub mod actions;
pub mod invariants;
pub mod routes;
pub mod settings;
pub mod sources;
pub mod tokens;
pub mod types;
pub mod value;
pub mod version;
pub mod widgets;

pub use actions::{
    ActionSpec, Binding, Decision, Escalation, Gate, Gates, Kind, Param, ParamType, PlanFacts, Tier,
};
pub use invariants::Invariant;
pub use routes::RouteSpec;
pub use sources::{Live, SourceParam, SourceSpec};
pub use types::{Os, Status, ValueType};
pub use value::Value;
pub use version::{ContractVersion, VersionError};
pub use widgets::{AppWidget, Depth, Part, Placement, Variant};

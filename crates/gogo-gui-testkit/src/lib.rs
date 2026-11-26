//! Test support for the GUI crates, only ever a dev-dependency (design 24 §2.1 rule 7):
//! fixture data, procedural covers, test scenes and golden-image compare. The fake backend
//! lives with the model (PR D1, #531).
//!
//! - [`FixtureScope`]: fictional data a skin's expressions read, for binding tests.
//! - [`scenes`]: the display lists the renderers are tested on.
//! - [`covers`]: procedural art for their images.
//! - [`golden`]: compare renders within a tolerance.
#![deny(unsafe_code)]

pub mod covers;
mod fixture;
pub mod golden;
pub mod scenes;

pub use fixture::{FixtureScope, GAME_ID, NOW};

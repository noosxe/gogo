//! gogo-gui-testkit: test support for the GUI crates (dev-dependency only) (docs/design/24-gui-architecture.md §2).
//!
//! Test support for the GUI crates, only ever a dev-dependency (design 24 §2.1 rule 7):
//! fixture data, a fake backend, procedural covers, golden-image compare and headless
//! rendering. Filled in with PR C4 (#529) and PR D1 (#531).
#![deny(unsafe_code)]

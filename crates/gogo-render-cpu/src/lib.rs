//! gogo-render-cpu: vello_cpu renderer: reference, no-GPU path, golden tests (docs/design/24-gui-architecture.md §2).
//!
//! The vello_cpu renderer (design 28): the reference for golden tests and the no-GPU path.
//! Sees only the display list (design 24 §2.1 rule 3). Filled in by PR C4 (#529).
#![deny(unsafe_code)]

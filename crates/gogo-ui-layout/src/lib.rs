//! gogo-ui-layout: iR to taffy to positioned boxes (docs/design/24-gui-architecture.md §2).
//!
//! Layout: IR → taffy → positioned boxes, with hit-testing (design 24 §2). Text leaves are
//! measured through `gogo-ui-text`. Filled in by PR C2 (#528).
#![deny(unsafe_code)]

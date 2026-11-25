//! Layout: IR → taffy → positioned boxes, with hit-testing (design 24 §2). Text leaves are
//! measured through `gogo-ui-text`. Filled in by PR C2 (#528).
#![deny(unsafe_code)]

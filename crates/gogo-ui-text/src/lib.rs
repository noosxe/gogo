//! gogo-ui-text: fonts, shaping, line breaking and the edit buffer (parley) (docs/design/24-gui-architecture.md §2).
//!
//! Fonts, shaping, line breaking and the text-edit buffer, on parley (design 24 §2, §3).
//! Fonts come from the bundled set; system fonts are a fallback only. Filled in by PR C1
//! (#528).
#![deny(unsafe_code)]

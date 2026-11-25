//! The app layer (design 25): latest-value sources, action dispatch, the safety flow and the
//! job table, behind a `Backend` trait. Depends only on `gogo-contract`: no IR, skin,
//! renderer, window or async runtime (design 24 §2.1 rule 5). Filled in by PR D1 (#531).
#![deny(unsafe_code)]

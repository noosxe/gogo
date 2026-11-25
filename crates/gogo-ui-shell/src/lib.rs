//! gogo-ui-shell: the window: winit loop, surfaces, frame pacing, IME, clipboard (docs/design/24-gui-architecture.md §2).
//!
//! The window: the winit event loop, surfaces, frame pacing and damage, IME, clipboard,
//! platform accessibility and hot-reload watching (design 24 §2). Knows the `Renderer`
//! trait, never a renderer (design 24 §2.1 rule 4). Filled in by PR E3 (#534).
#![deny(unsafe_code)]

//! gogo-tui — v2 ratatui terminal frontend (`docs/design/09` §2).
//!
//! This member reserves the workspace slot so the core/cli/tui boundary exists from
//! day one. Implementation (v2) consumes `gogo-core`: the same plan/execute APIs and
//! the `ProgressEvent` broadcast stream the CLI renders — no engine changes required.

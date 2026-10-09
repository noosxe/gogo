//! Human tables / JSON rendering (`docs/design/03` §4, §6) — presentation lives in the
//! frontend; pure formatting helpers (human sizes) live in `gogo_core`'s `fmt` module
//! so the v2 TUI renders them identically.

/// Render the owned-games table: `ID  TITLE  OS  INSTALLERS  SIZE  INSTALLED`.
pub fn list_table(
    _rows: &[gogo::state::records::ListRow],
    _color: crate::cli::ColorWhen,
) -> String {
    todo!("comfy-table — docs/design/03 §1")
}

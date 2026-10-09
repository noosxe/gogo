//! Human tables / JSON rendering, size formatting (`docs/design/03` §4, §6).
//!
//! Results on stdout, human progress on stderr — rendering helpers here must respect
//! `--color` and `--json`.

/// "724.2 MB" style human sizes (`docs/design/03` §1 list table).
pub fn human_size(bytes: u64) -> String {
    bytesize::ByteSize::b(bytes).to_string()
}

/// Render the owned-games table: `ID  TITLE  OS  INSTALLERS  SIZE  INSTALLED`.
pub fn list_table(
    _rows: &[crate::state::records::ListRow],
    _color: crate::cli::ColorWhen,
) -> String {
    todo!("comfy-table — docs/design/03 §1")
}

//! Human tables / JSON rendering (`docs/design/03` §4, §6) — presentation lives in the
//! frontend; pure formatting helpers (human sizes) live in `gogo_core`'s `fmt` module
//! so the v2 TUI renders them identically.

/// Render the owned-games table: `ID  TITLE  OS  INSTALLERS  SIZE  DOWNLOADED`
/// (03 §1 — the column is DOWNLOADED, not INSTALLED: gogo never installs).
pub fn list_table(rows: &[gogo::state::records::ListRow], _color: crate::cli::ColorWhen) -> String {
    use comfy_table::{Cell, Color, Table};

    let mut table = Table::new();
    table.set_header(vec!["ID", "TITLE", "OS", "SIZE", "DOWNLOADED"]);
    for row in rows {
        let os = row
            .os
            .map(|o| match o {
                gogo::gog::catalog::Os::Windows => "win",
                gogo::gog::catalog::Os::Mac => "mac",
                gogo::gog::catalog::Os::Linux => "linux",
            })
            .unwrap_or("—");
        let installed = match &row.installed_version {
            Some(v) => Cell::new(format!("✓ {v}")).fg(Color::Green),
            None => Cell::new("–"),
        };
        table.add_row(vec![
            Cell::new(row.id),
            Cell::new(&row.title),
            Cell::new(os),
            Cell::new(gogo::fmt::human_size(row.total_size)),
            installed,
        ]);
    }
    table.to_string()
}

//! Human tables / JSON rendering (`docs/design/03` §4, §6) — presentation lives in the
//! frontend; pure formatting helpers (human sizes) live in `gogo_core`'s `fmt` module
//! so the v2 TUI renders them identically.

/// Render the owned-games table: `ID  TITLE  OS  INSTALLERS  SIZE  DOWNLOADED`
/// (03 §1 — the column is DOWNLOADED, not INSTALLED: gogo never installs).
pub fn list_table(rows: &[gogo::state::records::ListRow], _color: crate::cli::ColorWhen) -> String {
    use comfy_table::{Cell, Color, Table};

    let mut table = Table::new();
    table.set_header(vec![
        "ID",
        "TITLE",
        "OS",
        "INSTALLERS",
        "SIZE",
        "DOWNLOADED",
    ]);
    for row in rows {
        let os = if row.os.is_empty() {
            "—".to_string()
        } else {
            row.os
                .iter()
                .map(|o| match o {
                    gogo::gog::catalog::Os::Windows => "win",
                    gogo::gog::catalog::Os::Mac => "mac",
                    gogo::gog::catalog::Os::Linux => "linux",
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        // DLC rows indent under their game (03 §1: never library entries).
        let title = if row.depth > 0 {
            format!(
                "{}↳ {}",
                "  ".repeat(row.depth.saturating_sub(1) as usize),
                row.title
            )
        } else {
            row.title.clone()
        };
        let installed = match &row.installed_version {
            Some(v) => Cell::new(format!("✓ {v}")).fg(Color::Green),
            None => Cell::new("–"),
        };
        table.add_row(vec![
            Cell::new(row.id),
            Cell::new(title),
            Cell::new(os),
            Cell::new(row.installers),
            Cell::new(gogo::fmt::human_size(row.total_size)),
            installed,
        ]);
    }
    table.to_string()
}

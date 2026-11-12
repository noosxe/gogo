//! Human tables / JSON rendering (`docs/design/03` §4, §6) — presentation lives in the
//! frontend; pure formatting helpers (human sizes) live in `gogo_core`'s `fmt` module
//! so the v2 TUI renders them identically.

/// A comfy-table `Table` that prints without trailing whitespace on any line.
pub struct Table(comfy_table::Table);

impl std::ops::Deref for Table {
    type Target = comfy_table::Table;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Table {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::fmt::Display for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rendered = self.0.to_string();
        for (i, line) in rendered.lines().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{}", line.trim_end())?;
        }
        Ok(())
    }
}

/// The one table style every list-style command uses: no outer border, but the dividers
/// between columns and rows (and under the header) stay.
pub fn table() -> Table {
    use comfy_table::{LineStyle, presets::ASCII_NO_BORDERS};
    let mut t = comfy_table::Table::new();
    *t.style_mut() = ASCII_NO_BORDERS.header_separator(LineStyle::none().fill('=').junction('+'));
    Table(t)
}

/// Render the owned-games table: `ID  TITLE  OS  INSTALLERS  SIZE  LOCAL`
/// (03 §1; LOCAL is what is set up here, see `gogo::state::local::Local::label`).
pub fn list_table(rows: &[gogo::state::records::ListRow], color: crate::cli::ColorWhen) -> String {
    use comfy_table::{Cell, Color};

    let mut table = table();
    table.set_header(vec!["ID", "TITLE", "OS", "INSTALLERS", "SIZE", "LOCAL"]);
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
        let installed = match row.local.label() {
            Some(l) if color != crate::cli::ColorWhen::Never => Cell::new(l).fg(Color::Green),
            Some(l) => Cell::new(l),
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

fn os_label(os: &[gogo::gog::catalog::Os]) -> String {
    os.iter()
        .map(|o| match o {
            gogo::gog::catalog::Os::Windows => "win",
            gogo::gog::catalog::Os::Mac => "mac",
            gogo::gog::catalog::Os::Linux => "linux",
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rows folded into a summary line in the human output (design 17 §5): goodies and
/// not-owned DLC that matched only through their parent, unless asked for.
fn folded(hit: &gogo::search::Hit, fold_goodies: bool, fold_dlc: bool) -> bool {
    use gogo::search::{Kind, MatchedBy};
    hit.matched_by == MatchedBy::Parent
        && match hit.item.kind {
            Kind::Goodies => fold_goodies,
            Kind::Dlc => fold_dlc && !hit.item.owned,
            _ => false,
        }
}

/// Per-kind cells after id and title (design 17 §7): `[info, size, status]`.
/// info is the OS list, the goodie subtype or the raw type; size is shown for
/// owned rows only; status holds `✓ <version>`, `(product)` and `not owned`.
fn search_cells(item: &gogo::search::Item) -> [String; 3] {
    use gogo::search::Kind;
    let size = if item.owned && matches!(item.kind, Kind::Game | Kind::Dlc | Kind::Goodies) {
        gogo::fmt::human_size(item.size)
    } else {
        String::new()
    };
    let info = match item.kind {
        Kind::Game | Kind::Dlc if item.owned => os_label(&item.os),
        Kind::Goodies => item.subtype.clone().unwrap_or_else(|| "-".into()),
        Kind::Other => format!("type: {}", item.game_type.as_deref().unwrap_or("unknown")),
        _ => String::new(),
    };
    let mut status = Vec::new();
    if let Some(v) = item.installed_version.as_ref().filter(|_| item.owned) {
        status.push(format!("✓ {v}"));
    }
    if item.kind == Kind::Goodies && item.product {
        status.push("(product)".into());
    }
    if !item.owned {
        status.push("not owned".into());
    }
    [info, size, status.join(" ")]
}

/// Summary line for one parent's folded rows: `+ 14 goodies (audio 3, …)` or
/// `+ 23 DLC not owned`.
fn fold_summary(rows: &[&gogo::search::Hit]) -> Vec<String> {
    use gogo::search::Kind;
    let mut out = Vec::new();
    let goodies: Vec<_> = rows
        .iter()
        .filter(|h| h.item.kind == Kind::Goodies)
        .collect();
    if !goodies.is_empty() {
        let mut by_type: Vec<(String, usize)> = Vec::new();
        for h in &goodies {
            let t = h.item.subtype.clone().unwrap_or_else(|| "unknown".into());
            match by_type.iter_mut().find(|(k, _)| *k == t) {
                Some((_, n)) => *n += 1,
                None => by_type.push((t, 1)),
            }
        }
        by_type.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let types = by_type
            .iter()
            .map(|(t, n)| format!("{t} {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let noun = if goodies.len() == 1 {
            "goodie"
        } else {
            "goodies"
        };
        out.push(format!("+ {} {noun} ({types})", goodies.len()));
    }
    let dlc = rows.iter().filter(|h| h.item.kind == Kind::Dlc).count();
    if dlc > 0 {
        out.push(format!("+ {dlc} DLC not owned"));
    }
    out
}

/// One line of the human `search` output, before column widths are known.
enum Line {
    /// A blank line before every group but the first.
    Gap,
    /// A group header or a parent title, already indented.
    Text(String),
    /// `indent + id`, title, then [`search_cells`].
    Row([String; 5]),
}

/// Human `search` output (design 17 §7): groups in fixed order, DLC and goodies
/// under their parent. `hits` come ordered from [`gogo::search::search`]. Every
/// row shares one set of column widths, across groups and indent levels.
pub fn search_human(hits: &[gogo::search::Hit], fold_goodies: bool, fold_dlc: bool) -> String {
    use gogo::search::Kind;
    let mut lines = Vec::new();
    let mut rest = hits;
    while let Some(first) = rest.first() {
        let kind = first.item.kind;
        let len = rest.iter().take_while(|h| h.item.kind == kind).count();
        let (group, tail) = rest.split_at(len);
        rest = tail;

        let label = match kind {
            Kind::Game => "Games",
            Kind::Dlc => "DLC",
            Kind::Pack => "Packs",
            Kind::Goodies => "Goodies",
            Kind::Other => "Other",
        };
        if !lines.is_empty() {
            lines.push(Line::Gap);
        }
        lines.push(Line::Text(format!("{label} ({})", group.len())));

        let mut i = 0;
        while i < group.len() {
            let parent = group[i].item.parent.as_ref().map(|p| p.id);
            let n = group[i..]
                .iter()
                .take_while(|h| h.item.parent.as_ref().map(|p| p.id) == parent)
                .count();
            let sub = &group[i..i + n];
            i += n;
            let indent = match &sub[0].item.parent {
                Some(p) if matches!(kind, Kind::Dlc | Kind::Goodies) => {
                    lines.push(Line::Text(format!("  {}", p.title)));
                    "    "
                }
                _ => "  ",
            };
            let mut hidden = Vec::new();
            for h in sub {
                if folded(h, fold_goodies, fold_dlc) {
                    hidden.push(h);
                } else {
                    let [info, size, status] = search_cells(&h.item);
                    lines.push(Line::Row([
                        format!("{indent}{}", h.item.id),
                        h.item.title.clone(),
                        info,
                        size,
                        status,
                    ]));
                }
            }
            for line in fold_summary(&hidden) {
                lines.push(Line::Text(format!("{indent}{line}")));
            }
        }
    }

    let width = console::measure_text_width;
    let mut w = [0usize; 5];
    for line in &lines {
        if let Line::Row(cells) = line {
            for (w, c) in w.iter_mut().zip(cells) {
                *w = (*w).max(width(c));
            }
        }
    }
    let mut out = String::new();
    for line in lines {
        match line {
            Line::Gap => {}
            Line::Text(t) => out.push_str(&t),
            Line::Row(cells) => {
                let mut row = String::new();
                for (i, (c, w)) in cells.iter().zip(w).enumerate() {
                    // Columns with nothing in them take no room.
                    if w == 0 {
                        continue;
                    }
                    let pad = " ".repeat(w - width(c));
                    match i {
                        3 => row.push_str(&format!("{pad}{c}  ")),
                        _ => row.push_str(&format!("{c}{pad}  ")),
                    }
                }
                out.push_str(row.trim_end());
            }
        }
        out.push('\n');
    }
    out
}

/// One `search --json` row (design 17 §7).
fn search_row_json(hit: &gogo::search::Hit) -> serde_json::Value {
    use gogo::search::Kind;
    let item = &hit.item;
    let id = item
        .id
        .parse::<u64>()
        .map(serde_json::Value::from)
        .unwrap_or_else(|_| serde_json::Value::from(item.id.clone()));
    let mut v = serde_json::json!({
        "kind": item.kind,
        "id": id,
        "title": item.title,
        "owned": item.owned,
        "matched_by": hit.matched_by,
    });
    let entry = item.kind == Kind::Goodies && !item.product;
    if !entry && item.kind != Kind::Goodies {
        v["slug"] = serde_json::json!(item.slug);
    }
    if matches!(item.kind, Kind::Game | Kind::Dlc) && item.owned {
        v["os"] = serde_json::json!(item.os);
        v["size"] = serde_json::json!(item.size);
        v["installed_version"] = serde_json::json!(item.installed_version);
    }
    if item.kind == Kind::Goodies {
        v["subtype"] = serde_json::json!(item.subtype);
        v["product"] = serde_json::json!(item.product);
        if item.owned {
            v["size"] = serde_json::json!(item.size);
        }
    }
    if let Some(p) = &item.parent {
        v["parent"] = serde_json::json!(p.id);
        if entry {
            v["parent_kind"] = serde_json::json!(p.kind);
        }
    }
    if item.kind == Kind::Other {
        v["game_type"] = serde_json::json!(item.game_type);
    }
    v
}

/// `search --json` document: every row (no folding), plus the suggestion.
pub fn search_json(query: &str, hits: &[gogo::search::Hit], suggestion: Option<&str>) -> String {
    serde_json::json!({
        "query": query,
        "results": hits.iter().map(search_row_json).collect::<Vec<_>>(),
        "suggestion": suggestion,
    })
    .to_string()
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use gogo::search::{Filters, Kind, Library, Query, classify, search};

    const FIX: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gogo-core/tests/fixtures/search"
    );

    fn fixture(name: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(format!("{FIX}/{name}")).unwrap()).unwrap()
    }

    /// The core fixture library (games, details, batch products).
    fn items() -> Vec<gogo::search::Item> {
        let page = fixture("library.json");
        let games: Vec<gogo::gog::library::LibraryProduct> =
            serde_json::from_value(page["products"].clone()).unwrap();
        let details: Vec<gogo::gog::catalog::Product> = [
            "product-1979.json",
            "product-hot-brass.json",
            "product.json",
        ]
        .iter()
        .map(|f| serde_json::from_value(fixture(f)).unwrap())
        .collect();
        let owned = fixture("owned.json");
        let batch: Vec<gogo::gog::catalog::Product> =
            serde_json::from_value(fixture("products-batch.json")).unwrap();
        let lib = Library {
            games: games
                .into_iter()
                .zip(details.into_iter().map(Some))
                .collect(),
            others: batch.into_iter().map(|p| (p.id, Some(p))).collect(),
            owned: serde_json::from_value(owned["owned"].clone()).unwrap(),
        };
        classify(&lib, &|p, d| match (p, d) {
            (1000000001, None) => Some("2.2.2".into()),
            (1000000002, Some(1000000101)) => Some("1.32".into()),
            _ => None,
        })
    }

    fn run(raw: &str, kinds: Vec<Kind>) -> String {
        let f = Filters::new(kinds, None, None, false);
        let hits = search(&items(), &Query::parse(raw).unwrap(), &f);
        search_human(&hits, !f.asked(Kind::Goodies), !f.asked(Kind::Dlc))
    }

    #[test]
    fn hot_brass_example() {
        assert_eq!(
            run("hot brass", vec![]),
            "\
Games (1)
  1000000001    Hot Brass                      win mac  690.7 MiB  ✓ 2.2.2

Goodies (1)
  Hot Brass
    1000000201  Hot Brass Original Soundtrack  audio      1.3 GiB  (product)
"
        );
    }

    #[test]
    fn witcher_dlc_example() {
        assert_eq!(
            run("witcher 3", vec![Kind::Dlc]),
            "\
DLC (2)
  The Witcher 3: Wild Hunt
    1000000102  Blood and Wine                 not owned
    1000000101  Hearts of Stone  win  3.6 GiB  ✓ 1.32
"
        );
    }

    #[test]
    fn folding_on_and_off() {
        let folded = run("witcher", vec![]);
        assert!(
            folded.contains("    + 2 goodies (audio 1, manuals 1)\n"),
            "{folded}"
        );
        assert!(folded.contains("    + 1 goodie (artworks 1)\n"), "{folded}");
        assert!(folded.contains("    + 1 DLC not owned\n"), "{folded}");
        assert!(!folded.contains("Manual") && !folded.contains("Blood and Wine"));
        // Matched on its own title: listed even when folding.
        assert!(folded.contains("Free DLC Soundtrack"));

        let full = run("witcher", vec![Kind::Dlc, Kind::Goodies]);
        assert!(!full.contains("+ "), "{full}");
        assert!(full.contains("Manual") && full.contains("Blood and Wine"));
        assert!(full.contains("Hearts of Stone Artbook"));
    }

    /// Every row starts each column at the same place, across groups, parents
    /// and indent levels, whatever the subtype or title lengths.
    #[test]
    fn columns_line_up_across_groups() {
        let out = run("witcher", vec![Kind::Dlc, Kind::Goodies]);
        let width = console::measure_text_width;
        let rows: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("  10") || l.contains("  48"))
            .collect();
        assert!(rows.len() > 4, "{out}");
        // Titles start at one column: past the indent, the id and the gap.
        let titles: Vec<usize> = rows
            .iter()
            .map(|l| {
                let id = l.trim_start();
                let gap = &id[id.find(' ').unwrap()..];
                width(&l[..l.len() - gap.trim_start().len()])
            })
            .collect();
        assert!(titles.windows(2).all(|w| w[0] == w[1]), "{out}");
        // Sizes end at one column.
        let sizes: Vec<usize> = rows
            .iter()
            .filter_map(|l| l.rfind("iB").map(|i| width(&l[..i + 2])))
            .collect();
        assert!(sizes.len() > 2, "{out}");
        assert!(sizes.windows(2).all(|w| w[0] == w[1]), "{out}");
    }

    #[test]
    fn json_shape() {
        let hits = search(
            &items(),
            &Query::parse("witcher").unwrap(),
            &Filters::default(),
        );
        let doc: serde_json::Value =
            serde_json::from_str(&search_json("witcher", &hits, None)).unwrap();
        assert_eq!(doc["query"], "witcher");
        assert!(doc["suggestion"].is_null());
        let rows = doc["results"].as_array().unwrap();
        // JSON never folds.
        assert_eq!(rows.len(), hits.len());
        let row = |id: u64| rows.iter().find(|r| r["id"] == id).unwrap();
        let game = row(1000000002);
        assert_eq!(game["kind"], "game");
        assert_eq!(game["os"], serde_json::json!(["windows"]));
        assert_eq!(game["owned"], true);
        let bw = row(1000000102);
        assert_eq!(
            (bw["kind"].as_str(), bw["owned"].as_bool()),
            (Some("dlc"), Some(false))
        );
        assert_eq!(bw["matched_by"], "parent");
        assert!(bw.get("os").is_none() && bw.get("size").is_none());
        let manual = row(48214);
        assert_eq!(manual["kind"], "goodies");
        assert_eq!(manual["product"], false);
        assert_eq!(manual["parent"], 1000000002u64);
        assert_eq!(manual["parent_kind"], "game");
        assert_eq!(row(48300)["parent_kind"], "dlc");
        let pack = row(1000000301);
        assert_eq!(pack["kind"], "pack");

        let none = search_json("witchr", &[], Some("witcher"));
        let doc: serde_json::Value = serde_json::from_str(&none).unwrap();
        assert_eq!(doc["results"], serde_json::json!([]));
        assert_eq!(doc["suggestion"], "witcher");
    }

    #[test]
    fn other_rows_show_game_type() {
        let out = run("mystery", vec![]);
        assert_eq!(
            out,
            "Other (1)\n  1000000303  Mystery Bundle  type: bundle\n"
        );
    }
}

#[cfg(test)]
mod table_tests {
    #[test]
    fn table_has_no_outer_border_but_keeps_dividers() {
        let mut t = super::table();
        t.set_header(vec!["GOGID", "TITLE", "DLC"]);
        t.add_row(vec!["1423049311", "Cyberpunk 2077", "2"]);
        t.add_row(vec!["1438075172", "Anno 1701 A.D.", "-"]);
        let out = t.to_string();
        let want = [
            " GOGID      | TITLE          | DLC",
            "============+================+=====",
            " 1423049311 | Cyberpunk 2077 | 2",
            "------------+----------------+-----",
            " 1438075172 | Anno 1701 A.D. | -",
        ]
        .join("\n");
        assert_eq!(out, want);
        assert!(out.lines().all(|l| l == l.trim_end()));
    }
}

#[cfg(test)]
mod list_tests {
    use super::list_table;
    use crate::cli::ColorWhen;
    use gogo::gog::content::{Content, Lane};
    use gogo::macapp::apps::App;
    use gogo::state::local::{Built, Local, read_cache};
    use gogo::state::records::ListRow;

    fn row(id: u64, title: &str, local: Local) -> ListRow {
        ListRow {
            id,
            title: title.into(),
            os: vec![gogo::gog::catalog::Os::Linux],
            installers: 1,
            total_size: 0,
            installed_version: None,
            local,
            depth: 0,
        }
    }

    /// One built app, one cache-only game, one installer download and one bare game.
    fn fixture() -> Vec<ListRow> {
        let app = App {
            gogid: "1".into(),
            linux: true,
            dlcs: vec![
                gogo::gog::content::DlcRef {
                    id: 10,
                    title: "A".into(),
                },
                gogo::gog::content::DlcRef {
                    id: 11,
                    title: "B".into(),
                },
            ],
            content: Some(Content {
                schema: 1,
                lane: Lane::Depot,
                build_id: Some("900".into()),
                version: Some("2.31".into()),
                language: Some("en-US".into()),
                dlc: Vec::new(),
            }),
            ..App::default()
        };
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            cache.path().join(".gogo-depot.json"),
            r#"{"buildId":"77","language":"en-US","files":[]}"#,
        )
        .unwrap();
        vec![
            row(
                1,
                "Built",
                Local {
                    built: Some(Built::from_app(&app)),
                    ..Local::default()
                },
            ),
            row(
                2,
                "Cached",
                Local {
                    cache: read_cache(cache.path()),
                    ..Local::default()
                },
            ),
            row(
                3,
                "Installer",
                Local {
                    installer: Some("3.00".into()),
                    ..Local::default()
                },
            ),
            row(4, "Bare", Local::default()),
        ]
    }

    #[test]
    fn table_shows_what_is_local() {
        let out = list_table(&fixture(), ColorWhen::Never);
        let cells: Vec<String> = out
            .lines()
            .filter_map(|l| l.rsplit('|').next().map(|c| c.trim().to_string()))
            .collect();
        assert_eq!(cells[0], "LOCAL", "{out}");
        assert!(
            cells.contains(&"linux depot 2.31 en-US +2 DLC".to_string()),
            "{out}"
        );
        assert!(cells.contains(&"cache en-US".to_string()), "{out}");
        assert!(cells.contains(&"✓ installer 3.00".to_string()), "{out}");
        assert!(cells.contains(&"–".to_string()), "{out}");
        assert!(!out.contains("DOWNLOADED"));
    }

    #[test]
    fn local_json_and_filter_predicate() {
        let rows = fixture();
        let any: Vec<u64> = rows
            .iter()
            .filter(|r| r.local.any())
            .map(|r| r.id)
            .collect();
        assert_eq!(any, [1, 2, 3]);
        let j: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| serde_json::to_value(&r.local).unwrap())
            .collect();
        assert_eq!(
            j[0],
            serde_json::json!({"built": {"platform": "linux", "lane": "depot",
                "version": "2.31", "build_id": "900", "language": "en-US",
                "dlc_count": 2}})
        );
        assert_eq!(
            j[1],
            serde_json::json!({"cache": {"build_id": "77", "language": "en-US"}})
        );
        assert_eq!(j[2], serde_json::json!({"installer": "3.00"}));
        assert_eq!(j[3], serde_json::json!({}));
    }
}

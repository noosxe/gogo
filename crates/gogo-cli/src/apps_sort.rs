//! `gogo apps list --sort`: the column names, parsing and ordering.
//!
//! Kept free of the table code so `gogo list` can reuse it for its own columns later: a
//! command supplies its columns and a comparison per column, and `parse` / `sort_by`
//! do the rest.

use std::cmp::Ordering;

use gogo::macapp::apps::App;

/// A sortable column of `apps list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Gogid,
    Title,
    Entry,
    Runtime,
    Renderer,
    Dlc,
    Size,
    LastLaunch,
}

/// The names `--sort` accepts, as shown in the error for an unknown one.
pub const NAMES: &str =
    "gogid, title, menu-entry (or app), runtime, renderer, dlc, size, last-launch";

/// One `--sort` key: a column and its direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub column: Column,
    pub descending: bool,
}

/// The default order: by title.
pub fn default_keys() -> Vec<Key> {
    vec![Key {
        column: Column::Title,
        descending: false,
    }]
}

fn column(name: &str) -> Option<Column> {
    let norm: String = name
        .trim()
        .chars()
        .map(|c| {
            if c == '-' || c == '_' {
                ' '
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    Some(match norm.as_str() {
        "gogid" | "id" => Column::Gogid,
        "title" => Column::Title,
        "menu entry" | ".app" | "app" => Column::Entry,
        "runtime" => Column::Runtime,
        "renderer" => Column::Renderer,
        "dlc" => Column::Dlc,
        "size" => Column::Size,
        "last launch" => Column::LastLaunch,
        _ => return None,
    })
}

/// Parse `--sort`'s comma-separated column list. A `-` prefix sorts that column
/// descending (`-size`); an unknown column is an error listing the valid ones.
pub fn parse(spec: &str) -> Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (descending, name) = match part.strip_prefix('-') {
            // `-` followed by a name is descending; `last-launch` has its dash inside.
            Some(rest) if !rest.is_empty() => (true, rest),
            _ => (false, part),
        };
        let Some(column) = column(name) else {
            return Err(format!(
                "unknown sort column `{part}`; valid columns: {NAMES} \
                 (prefix one with `-` for descending)"
            ));
        };
        keys.push(Key { column, descending });
    }
    if keys.is_empty() {
        return Err(format!("--sort needs at least one column: {NAMES}"));
    }
    Ok(keys)
}

fn gogid_num(a: &App) -> u64 {
    a.gogid.parse().unwrap_or(u64::MAX)
}

fn compare(a: &App, b: &App, column: Column) -> Ordering {
    // Missing values (`-` in the table) sort first, ascending.
    let text = |s: &Option<String>| s.as_deref().map(str::to_lowercase);
    match column {
        Column::Gogid => gogid_num(a).cmp(&gogid_num(b)),
        Column::Title => text(&a.title).cmp(&text(&b.title)),
        Column::Entry => crate::app_column(a).cmp(&crate::app_column(b)),
        Column::Runtime => text(&a.runtime).cmp(&text(&b.runtime)),
        Column::Renderer => text(&a.renderer).cmp(&text(&b.renderer)),
        Column::Dlc => a.dlcs.len().cmp(&b.dlcs.len()),
        Column::Size => a.bytes.cmp(&b.bytes),
        // `YYYY-MM-DD HH:MM` sorts as time; never launched (`None`) comes first.
        Column::LastLaunch => a.last_launch.cmp(&b.last_launch),
    }
}

/// Order `apps` by `keys`, then by GOGID (numerically) so the order is stable.
pub fn sort(apps: &mut [App], keys: &[Key]) {
    apps.sort_by(|a, b| {
        for k in keys {
            let o = compare(a, b, k.column);
            let o = if k.descending { o.reverse() } else { o };
            if o != Ordering::Equal {
                return o;
            }
        }
        gogid_num(a).cmp(&gogid_num(b))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(gogid: &str, title: Option<&str>, bytes: u64, last: Option<&str>) -> App {
        let mut a = App {
            gogid: gogid.to_string(),
            ..App::default()
        };
        a.title = title.map(str::to_string);
        a.bytes = bytes;
        a.last_launch = last.map(str::to_string);
        a
    }

    fn ids(apps: &[App]) -> Vec<&str> {
        apps.iter().map(|a| a.gogid.as_str()).collect()
    }

    fn sample() -> Vec<App> {
        vec![
            app("10", Some("banana"), 5, Some("2026-10-24 09:15")),
            app("9", Some("Apple"), 50, None),
            app("100", Some("apple"), 5, Some("2026-01-01 00:00")),
            app("2", Some("Cherry"), 500, Some("2026-10-25 08:00")),
        ]
    }

    #[test]
    fn default_is_title_case_insensitive_then_numeric_gogid() {
        let mut v = sample();
        sort(&mut v, &default_keys());
        // "Apple" (9) and "apple" (100) tie on title; 9 < 100 numerically.
        assert_eq!(ids(&v), ["9", "100", "10", "2"]);
    }

    #[test]
    fn multiple_keys() {
        let mut v = sample();
        sort(&mut v, &parse("size,title").unwrap());
        // size 5: apple(100) before banana(10); then 50; then 500.
        assert_eq!(ids(&v), ["100", "10", "9", "2"]);
    }

    #[test]
    fn size_is_numeric_and_descending_works() {
        let mut v = sample();
        sort(&mut v, &parse("-size").unwrap());
        assert_eq!(ids(&v), ["2", "9", "10", "100"]);
        sort(&mut v, &parse("SIZE").unwrap());
        assert_eq!(ids(&v), ["10", "100", "9", "2"]);
    }

    #[test]
    fn last_launch_by_time_never_launched_first() {
        let mut v = sample();
        sort(&mut v, &parse("last-launch").unwrap());
        assert_eq!(ids(&v), ["9", "100", "10", "2"]);
        sort(&mut v, &parse("last_launch").unwrap());
        assert_eq!(ids(&v), ["9", "100", "10", "2"]);
        sort(&mut v, &parse("LAST LAUNCH").unwrap());
        assert_eq!(ids(&v), ["9", "100", "10", "2"]);
    }

    #[test]
    fn gogid_is_numeric() {
        let mut v = sample();
        sort(&mut v, &parse("gogid").unwrap());
        assert_eq!(ids(&v), ["2", "9", "10", "100"]);
    }

    #[test]
    fn unknown_column_is_refused_with_the_valid_names() {
        let err = parse("size,colour").unwrap_err();
        assert!(err.contains("colour"), "{err}");
        assert!(err.contains("last-launch"), "{err}");
        assert!(parse("").is_err());
    }
}

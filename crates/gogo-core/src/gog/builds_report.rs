//! What `gogo builds` shows: the table, the footer and `--json` of one game's
//! builds, and the `--installed` overview (design 21 §2). Pure: the command
//! fetches the listing and the manifests and hands the facts over.

use chrono::{DateTime, SecondsFormat, Utc};
use gog_warp::content_system::types::Manifest;
use serde_json::{Value, json};

use super::builds::BuildEntry;
use super::language;

/// What a build manifest says that the table needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// Download bytes of the base game in the language, no DLC. `None`: the
    /// build has no such language.
    pub download_size: Option<u64>,
    pub disk_size: Option<u64>,
    /// DLC product ids in the build, owned or not.
    pub dlcs: Vec<u64>,
}

/// Read `manifest` for `language` (a code, or a short form of one).
pub fn facts(manifest: &Manifest, language: &str) -> Facts {
    let dlcs: Vec<u64> = manifest
        .dlcs()
        .iter()
        .filter_map(|id| id.parse().ok())
        .collect();
    let langs = language::of_manifest(manifest);
    let sized = langs.resolve(language).ok().filter(|code| {
        let codes = langs.codes();
        codes.is_empty() || codes.contains(code)
    });
    let (download_size, disk_size) = match sized {
        Some(code) => {
            let none: Vec<String> = Vec::new();
            let (d, k) = manifest.install_size(&code, &none, None);
            (Some(d), Some(k))
        }
        None => (None, None),
    };
    Facts {
        download_size,
        disk_size,
        dlcs,
    }
}

/// Every listed build's manifest, fetched in parallel (design 21 §2), read for
/// `language`. A build whose manifest can't be fetched is left out.
pub async fn fetch_facts(
    core: &gog_warp::Core,
    listing: &super::builds::Listing,
    language: &str,
) -> std::collections::HashMap<String, Facts> {
    let fetched =
        futures::future::join_all(listing.builds.iter().map(|b| core.get_manifest(b))).await;
    let mut out = std::collections::HashMap::new();
    for (b, m) in listing.builds.iter().zip(fetched) {
        match m {
            Ok(m) => {
                out.insert(b.build_id().clone(), facts(&m, language));
            }
            Err(e) => {
                tracing::debug!(build = %b.build_id(), error = %e, "manifest fetch failed");
            }
        }
    }
    out
}

/// A DLC's name for the table's JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlcName {
    pub id: u64,
    pub title: String,
    pub owned: bool,
}

/// One listed build with what was read about it.
#[derive(Debug, Clone)]
pub struct Row {
    pub entry: BuildEntry,
    /// `None`: the manifest couldn't be fetched (`?` in the table).
    pub facts: Option<Facts>,
    /// `game`, `cache`: where this machine uses the build.
    pub here: Vec<&'static str>,
}

/// The installed build, when GOG doesn't list it any more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlisted {
    pub build_id: String,
    pub version: String,
    pub here: Vec<&'static str>,
}

/// One of the catalog's patch installers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub name: String,
    pub version: String,
    pub language: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub gogid: String,
    pub title: String,
    pub os: String,
    pub language: String,
    pub total_count: u32,
    pub with_password: bool,
    pub rows: Vec<Row>,
    /// The newest public build on the default branch.
    pub newest: Option<String>,
    pub installed_game: Option<String>,
    pub installed_cache: Option<String>,
    /// Installed builds (game, cache) that GOG doesn't list any more.
    pub unlisted: Vec<Unlisted>,
    /// The catalog installer's version (the installer lane).
    pub installer: Option<String>,
    /// `Some` with `--patches`.
    pub patches: Option<Vec<Patch>>,
    pub dlc_names: Vec<DlcName>,
}

impl Report {
    fn dlc_name(&self, id: u64) -> DlcName {
        self.dlc_names
            .iter()
            .find(|d| d.id == id)
            .cloned()
            .unwrap_or(DlcName {
                id,
                title: id.to_string(),
                owned: false,
            })
    }

    /// Version names that two or more listed builds share, for the footer.
    pub fn duplicate_versions(&self) -> Vec<(String, usize)> {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for r in &self.rows {
            let v = &r.entry.version;
            if v.is_empty() {
                continue;
            }
            match seen.iter_mut().find(|(s, _)| s == v) {
                Some((_, n)) => *n += 1,
                None => seen.push((v.clone(), 1)),
            }
        }
        seen.retain(|(_, n)| *n > 1);
        seen
    }

    /// The builds whose manifest couldn't be read, for a warning each.
    pub fn failed_manifests(&self) -> Vec<&BuildEntry> {
        self.rows
            .iter()
            .filter(|r| r.facts.is_none())
            .map(|r| &r.entry)
            .collect()
    }

    /// The row the installer's version names (display only, §6): the newest
    /// build with that version.
    fn installer_build(&self) -> Option<&str> {
        let v = self.installer.as_deref()?;
        self.rows
            .iter()
            .find(|r| r.entry.version == v)
            .map(|r| r.entry.build_id.as_str())
    }

    /// The table and its footer (design 21 §2.2).
    pub fn render(&self) -> String {
        let mut out = format!(
            "{} ({}), {}, {}\n",
            self.title, self.gogid, self.os, self.language
        );
        let access = self.rows.iter().any(|r| r.entry.password_protected());
        let mut table: Vec<Vec<String>> = Vec::new();
        let mut head = vec!["  BUILD", "VERSION", "PUBLISHED", "GEN", "BRANCH"];
        if access {
            head.push("ACCESS");
        }
        head.extend(["SIZE", "DLC", "HERE"]);
        table.push(head.into_iter().map(String::from).collect());
        for r in &self.rows {
            let e = &r.entry;
            let mut line = vec![
                format!(
                    "{}{}",
                    if self.newest.as_deref() == Some(e.build_id.as_str()) {
                        "* "
                    } else {
                        "  "
                    },
                    e.build_id
                ),
                dash(&e.version),
                e.published.format("%Y-%m-%d").to_string(),
                e.generation.to_string(),
                e.branch.clone().unwrap_or_default(),
            ];
            if access {
                line.push(
                    if e.password_protected() {
                        "password"
                    } else {
                        "public"
                    }
                    .to_string(),
                );
            }
            line.push(match &r.facts {
                None => "?".into(),
                Some(f) => f
                    .download_size
                    .map_or("-".to_string(), crate::fmt::human_size),
            });
            line.push(
                r.facts
                    .as_ref()
                    .map_or("?".into(), |f| f.dlcs.len().to_string()),
            );
            line.push(r.here.join(", "));
            table.push(line);
        }
        for u in &self.unlisted {
            let mut line = vec![
                format!("  {}", u.build_id),
                dash(&u.version),
                "-".into(),
                "-".into(),
                String::new(),
            ];
            if access {
                line.push(String::new());
            }
            line.extend(["-".into(), "-".into(), u.here.join(", ")]);
            table.push(line);
        }
        out.push_str(&columns(&table));
        for u in &self.unlisted {
            out.push_str(&format!(
                "{} {} is installed here; GOG no longer lists it.\n",
                u.build_id,
                dash(&u.version)
            ));
        }
        let n = self.rows.len();
        if self.with_password {
            out.push_str(&format!("with a password: GOG lists {n} builds.\n"));
        } else {
            out.push_str(&format!(
                "{n} of the {} builds GOG counts {} listed; only listed builds can be downloaded.\n",
                self.total_count,
                if n == 1 { "is" } else { "are" }
            ));
        }
        for (v, n) in self.duplicate_versions() {
            out.push_str(&format!("{v} names {n} builds; pick by id.\n"));
        }
        let mut notes = Vec::new();
        if self.newest.is_some() {
            notes.push("* newest on the default branch.".to_string());
        }
        if let Some(v) = &self.installer {
            notes.push(match self.installer_build() {
                Some(b) => format!("installer: {v}, the version of build {b} (offline lane)."),
                None => format!("installer: {v} (offline lane)."),
            });
        }
        if !notes.is_empty() {
            out.push_str(&notes.join(" "));
            out.push('\n');
        }
        if let Some(p) = &self.patches {
            if p.is_empty() {
                out.push_str("patch installers: none (GOG offers no patch files for this game).\n");
            } else {
                out.push_str("patch installers (GOG's; gogo doesn't run them):\n");
                let mut t: Vec<Vec<String>> = vec![
                    ["NAME", "LEADS TO", "LANGUAGE", "SIZE"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                ];
                for x in p {
                    t.push(vec![
                        x.name.clone(),
                        dash(&x.version),
                        dash(&x.language),
                        x.size.map_or("-".into(), crate::fmt::human_size),
                    ]);
                }
                out.push_str(&columns(&t));
            }
        }
        out
    }

    /// `--json` (design 21 §2.3).
    pub fn to_json(&self) -> Value {
        let builds: Vec<Value> = self
            .rows
            .iter()
            .map(|r| {
                let e = &r.entry;
                json!({
                    "buildId": e.build_id,
                    "version": e.version,
                    "published": rfc3339(&e.published),
                    "generation": e.generation,
                    "branch": e.branch,
                    "public": e.public,
                    "listed": true,
                    "downloadSize": r.facts.as_ref().and_then(|f| f.download_size),
                    "diskSize": r.facts.as_ref().and_then(|f| f.disk_size),
                    "dlcs": r.facts.as_ref().map(|f| f.dlcs.iter().map(|id| {
                        let d = self.dlc_name(*id);
                        json!({"id": d.id, "title": d.title, "owned": d.owned})
                    }).collect::<Vec<_>>()),
                    "here": r.here,
                    "installer": self.installer_build() == Some(e.build_id.as_str()),
                })
            })
            .chain(self.unlisted.iter().map(|u| {
                json!({
                    "buildId": u.build_id,
                    "version": u.version,
                    "published": Value::Null,
                    "generation": Value::Null,
                    "branch": Value::Null,
                    "public": Value::Null,
                    "listed": false,
                    "downloadSize": Value::Null,
                    "diskSize": Value::Null,
                    "dlcs": Value::Null,
                    "here": u.here,
                    "installer": false,
                })
            }))
            .collect();
        let mut installer = self.installer.as_ref().map(|v| json!({"version": v}));
        if let (Some(p), Some(i)) = (&self.patches, installer.as_mut()) {
            i["patches"] = patches_json(p);
        } else if let Some(p) = &self.patches {
            installer = Some(json!({"version": Value::Null, "patches": patches_json(p)}));
        }
        json!({
            "gogid": self.gogid,
            "title": self.title,
            "os": self.os,
            "language": self.language,
            "totalCount": self.total_count,
            "withPassword": self.with_password,
            "newest": self.newest,
            "installed": {"game": self.installed_game, "cache": self.installed_cache},
            "installer": installer,
            "builds": builds,
        })
    }
}

fn patches_json(p: &[Patch]) -> Value {
    Value::Array(
        p.iter()
            .map(|x| {
                json!({"name": x.name, "version": x.version,
                       "language": x.language, "size": x.size})
            })
            .collect(),
    )
}

fn rfc3339(d: &DateTime<Utc>) -> String {
    d.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn dash(s: &str) -> String {
    if s.is_empty() {
        "-".into()
    } else {
        s.to_string()
    }
}

/// Left-aligned columns, two spaces apart, the last one unpadded.
fn columns(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for r in rows {
        let mut line = String::new();
        for (c, cell) in r.iter().enumerate() {
            if c + 1 == r.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<w$}  ", w = widths[c]));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

// ---- `--installed` ----

/// One built game in the overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRow {
    pub gogid: String,
    pub title: String,
    /// `depot` or `installer`.
    pub lane: &'static str,
    pub installed_id: Option<String>,
    pub installed_version: Option<String>,
    pub newest_id: Option<String>,
    pub newest_version: Option<String>,
    /// The installed build is still in GOG's list (installer lane: true).
    pub listed: bool,
    /// The check failed or wasn't possible: the reason.
    pub error: Option<String>,
}

impl InstalledRow {
    pub fn status(&self) -> &'static str {
        if self.error.is_some() {
            return "unchecked";
        }
        if !self.listed {
            return "no longer listed";
        }
        let same = match (&self.installed_id, &self.newest_id) {
            (Some(i), Some(n)) => i == n,
            _ => match (&self.installed_version, &self.newest_version) {
                (Some(i), Some(n)) => i == n,
                _ => return "unknown",
            },
        };
        if same {
            "up to date"
        } else {
            "newer available"
        }
    }
}

fn version_cell(id: &Option<String>, version: &Option<String>) -> String {
    match (version.as_deref().filter(|v| !v.is_empty()), id) {
        (Some(v), _) => v.to_string(),
        (None, Some(id)) => id.clone(),
        (None, None) => "-".into(),
    }
}

/// The `--installed` table.
pub fn render_installed(rows: &[InstalledRow]) -> String {
    if rows.is_empty() {
        return "no built games on this machine (see `gogo linux-app`)\n".into();
    }
    let mut t: Vec<Vec<String>> = vec![
        ["GAME", "LANE", "INSTALLED", "NEWEST", "STATUS"]
            .into_iter()
            .map(String::from)
            .collect(),
    ];
    for r in rows {
        t.push(vec![
            format!("{} ({})", r.title, r.gogid),
            r.lane.to_string(),
            version_cell(&r.installed_id, &r.installed_version),
            version_cell(&r.newest_id, &r.newest_version),
            match &r.error {
                Some(e) => format!("{} ({e})", r.status()),
                None => r.status().to_string(),
            },
        ]);
    }
    columns(&t)
}

/// `--installed --json` (design 21 §2.3).
pub fn installed_json(rows: &[InstalledRow], checked: DateTime<Utc>) -> Value {
    Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "gogid": r.gogid,
                    "title": r.title,
                    "lane": r.lane,
                    "installed": {"buildId": r.installed_id, "version": r.installed_version},
                    "newest": if r.newest_id.is_some() || r.newest_version.is_some() {
                        json!({"buildId": r.newest_id, "version": r.newest_version})
                    } else { Value::Null },
                    "listed": r.listed,
                    "checked": if r.error.is_some() { Value::Null } else { json!(rfc3339(&checked)) },
                    "error": r.error,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        id: &str,
        version: &str,
        date: &str,
        branch: Option<&str>,
        public: bool,
    ) -> BuildEntry {
        BuildEntry {
            build_id: id.into(),
            version: version.into(),
            published: date.parse().unwrap(),
            generation: 2,
            branch: branch.map(String::from),
            public,
        }
    }

    fn words(line: &str) -> Vec<&str> {
        line.split_whitespace().collect()
    }

    fn facts_of(dl: Option<u64>, dlcs: &[u64]) -> Option<Facts> {
        Some(Facts {
            download_size: dl,
            disk_size: dl.map(|d| d + 1000),
            dlcs: dlcs.to_vec(),
        })
    }

    fn cyberpunk() -> Report {
        let gib = 1024u64 * 1024 * 1024;
        Report {
            gogid: "1423049311".into(),
            title: "Cyberpunk 2077".into(),
            os: "windows".into(),
            language: "en-US".into(),
            total_count: 270,
            with_password: false,
            rows: vec![
                Row {
                    entry: entry(
                        "60152385347545148",
                        "2.5_RedModFix",
                        "2026-10-22T15:28:42Z",
                        None,
                        true,
                    ),
                    facts: facts_of(Some(60 * gib), &[1, 2]),
                    here: vec!["game", "cache"],
                },
                Row {
                    entry: entry(
                        "60148683633405433",
                        "2.5",
                        "2026-10-21T10:00:00Z",
                        None,
                        true,
                    ),
                    facts: facts_of(Some(60 * gib), &[1]),
                    here: vec![],
                },
                Row {
                    entry: entry(
                        "56624919703786522",
                        "1.63_Hotfix",
                        "2023-06-23T10:00:00Z",
                        Some("1_63_legacy_patch"),
                        true,
                    ),
                    facts: None,
                    here: vec![],
                },
            ],
            newest: Some("60152385347545148".into()),
            installed_game: Some("60152385347545148".into()),
            installed_cache: Some("60152385347545148".into()),
            unlisted: vec![],
            installer: Some("2.5_RedModFix".into()),
            patches: None,
            dlc_names: vec![
                DlcName {
                    id: 1,
                    title: "Phantom Liberty".into(),
                    owned: true,
                },
                DlcName {
                    id: 2,
                    title: "Bonus".into(),
                    owned: false,
                },
            ],
        }
    }

    #[test]
    fn the_table_marks_newest_here_and_failed_manifests() {
        let text = cyberpunk().render();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Cyberpunk 2077 (1423049311), windows, en-US");
        assert!(lines[1].starts_with("  BUILD"));
        assert!(lines[1].contains("PUBLISHED") && lines[1].ends_with("HERE"));
        assert!(lines[2].starts_with("* 60152385347545148  2.5_RedModFix"));
        assert!(lines[2].contains("2026-10-22"));
        assert!(lines[2].contains("60.0 GiB"));
        assert!(lines[2].ends_with("game, cache"));
        assert!(lines[3].starts_with("  60148683633405433"));
        // a manifest that failed: `?` for size and DLC
        assert!(lines[4].contains("1_63_legacy_patch"));
        assert!(words(lines[4]).ends_with(&["?", "?"]));
        assert!(text.contains("3 of the 270 builds GOG counts are listed"));
        assert!(text.contains(
            "* newest on the default branch. installer: 2.5_RedModFix, the version of build 60152385347545148 (offline lane)."
        ));
        assert!(!text.contains("ACCESS"));
        assert_eq!(cyberpunk().failed_manifests().len(), 1);
    }

    #[test]
    fn the_header_lines_up_with_the_data_columns() {
        let text = cyberpunk().render();
        let lines: Vec<&str> = text.lines().collect();
        let col = |line: &str, word: &str| line.find(word).unwrap();
        assert_eq!(col(lines[1], "BUILD"), col(lines[2], "60152385347545148"));
        assert_eq!(col(lines[1], "VERSION"), col(lines[2], "2.5_RedModFix"));
        assert_eq!(col(lines[1], "PUBLISHED"), col(lines[2], "2026-10-22"));
        assert_eq!(col(lines[1], "SIZE"), col(lines[2], "60.0 GiB"));
        assert_eq!(col(lines[1], "HERE"), col(lines[2], "game, cache"));
    }

    #[test]
    fn a_protected_build_adds_the_access_column() {
        let mut r = cyberpunk();
        r.rows[1].entry.public = false;
        let text = r.render();
        assert!(text.lines().nth(1).unwrap().contains("ACCESS"));
        assert!(text.lines().nth(3).unwrap().contains("password"));
        assert!(text.lines().nth(2).unwrap().contains("public"));
    }

    #[test]
    fn a_password_footer_and_duplicate_versions_and_empty_versions() {
        let mut r = cyberpunk();
        r.with_password = true;
        r.rows[1].entry.version = "2.5_RedModFix".into();
        r.rows[2].entry.version = String::new();
        let text = r.render();
        assert!(text.contains("with a password: GOG lists 3 builds."));
        assert!(text.contains("2.5_RedModFix names 2 builds; pick by id."));
        assert!(words(text.lines().nth(4).unwrap()).contains(&"-"));
    }

    #[test]
    fn a_language_a_build_lacks_shows_a_dash() {
        let mut r = cyberpunk();
        r.rows[1].facts = facts_of(None, &[1]);
        assert!(words(r.render().lines().nth(3).unwrap()).ends_with(&["-", "1"]));
    }

    #[test]
    fn an_unlisted_installed_build_gets_its_own_row() {
        let mut r = cyberpunk();
        r.unlisted = vec![Unlisted {
            build_id: "111".into(),
            version: "2.0".into(),
            here: vec!["game"],
        }];
        let text = r.render();
        assert!(words(text.lines().nth(5).unwrap()).starts_with(&["111", "2.0"]));
        assert!(text.contains("111 2.0 is installed here; GOG no longer lists it."));
        let j = r.to_json();
        let last = j["builds"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["listed"], false);
        assert_eq!(last["buildId"], "111");
    }

    #[test]
    fn the_patches_list_follows_the_footer() {
        let mut r = cyberpunk();
        r.patches = Some(vec![Patch {
            name: "Patch 2.5".into(),
            version: "2.5".into(),
            language: "English".into(),
            size: Some(1024),
        }]);
        let text = r.render();
        assert!(text.contains("patch installers (GOG's; gogo doesn't run them):"));
        assert!(text.contains("Patch 2.5"));
        r.patches = Some(vec![]);
        assert!(r.render().contains("patch installers: none"));
        assert!(!cyberpunk().render().contains("patch installers"));
    }

    #[test]
    fn json_has_the_designed_shape() {
        let j = cyberpunk().to_json();
        assert_eq!(j["gogid"], "1423049311");
        assert_eq!(j["totalCount"], 270);
        assert_eq!(j["newest"], "60152385347545148");
        assert_eq!(j["installed"]["game"], "60152385347545148");
        assert_eq!(j["installed"]["cache"], "60152385347545148");
        assert_eq!(j["installer"]["version"], "2.5_RedModFix");
        let b = &j["builds"][0];
        assert_eq!(b["published"], "2026-10-22T15:28:42Z");
        assert_eq!(b["generation"], 2);
        assert!(b["branch"].is_null());
        assert_eq!(b["public"], true);
        assert_eq!(b["listed"], true);
        assert_eq!(b["downloadSize"], 60u64 * 1024 * 1024 * 1024);
        assert_eq!(b["dlcs"][0]["title"], "Phantom Liberty");
        assert_eq!(b["dlcs"][0]["owned"], true);
        assert_eq!(b["dlcs"][1]["owned"], false);
        assert_eq!(b["here"], json!(["game", "cache"]));
        assert_eq!(b["installer"], true);
        // a failed manifest: null sizes and DLC, not zero
        let c = &j["builds"][2];
        assert!(c["downloadSize"].is_null());
        assert!(c["dlcs"].is_null());
        assert_eq!(c["branch"], "1_63_legacy_patch");
    }

    #[test]
    fn json_without_an_installer_or_game() {
        let mut r = cyberpunk();
        r.installer = None;
        r.installed_game = None;
        r.installed_cache = None;
        let j = r.to_json();
        assert!(j["installer"].is_null());
        assert!(j["installed"]["game"].is_null());
    }

    fn row(lane: &'static str, inst: &str, newest: &str, listed: bool) -> InstalledRow {
        InstalledRow {
            gogid: "1".into(),
            title: "Game".into(),
            lane,
            installed_id: Some(inst.into()),
            installed_version: Some(format!("v{inst}")),
            newest_id: Some(newest.into()),
            newest_version: Some(format!("v{newest}")),
            listed,
            error: None,
        }
    }

    #[test]
    fn installed_statuses() {
        assert_eq!(row("depot", "5", "5", true).status(), "up to date");
        assert_eq!(row("depot", "4", "5", true).status(), "newer available");
        assert_eq!(row("depot", "4", "5", false).status(), "no longer listed");
        let mut r = row("depot", "4", "5", true);
        r.error = Some("offline".into());
        assert_eq!(r.status(), "unchecked");
        let mut inst = row("installer", "", "", true);
        inst.installed_id = None;
        inst.newest_id = None;
        inst.newest_version = Some("v2".into());
        inst.installed_version = Some("v1".into());
        assert_eq!(inst.status(), "newer available");
        inst.installed_version = Some("v2".into());
        assert_eq!(inst.status(), "up to date");
    }

    #[test]
    fn installed_table_and_json() {
        let rows = vec![row("depot", "4", "5", true)];
        let t = render_installed(&rows);
        assert!(t.lines().next().unwrap().contains("INSTALLED"));
        assert_eq!(
            words(t.lines().nth(1).unwrap()),
            ["Game", "(1)", "depot", "v4", "v5", "newer", "available"]
        );
        let when: DateTime<Utc> = "2026-11-13T09:00:00Z".parse().unwrap();
        let j = installed_json(&rows, when);
        assert_eq!(j[0]["installed"]["buildId"], "4");
        assert_eq!(j[0]["newest"]["version"], "v5");
        assert_eq!(j[0]["checked"], "2026-11-13T09:00:00Z");
        assert_eq!(j[0]["listed"], true);
        assert!(render_installed(&[]).contains("no built games"));
    }
}

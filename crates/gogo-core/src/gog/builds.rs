//! The builds GOG lists for a game: the listing type, which build is "newest",
//! resolving a `--build` value, and the per-game listing cache
//! (design 21 §3.2–3.3, §4.2, D2, D3, D7).

use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use gog_warp::Core;
use gog_warp::content_system::types::{Build, Platform};
use serde::{Deserialize, Serialize};

/// Schema of the cached listing file.
const CACHE_SCHEMA: u32 = 1;

/// Pure: the newest public build on the default branch (`branch` null),
/// tiebreaking on `build_id` (design 14 D3, design 21 D3). Builds on a named
/// branch are never picked on their own. `get_builds` already filters by platform.
pub fn pick_build<'a>(items: impl IntoIterator<Item = &'a Build>) -> Option<&'a Build> {
    items
        .into_iter()
        .filter(|b| *b.public() && b.branch().is_none())
        .max_by(|a, b| {
            a.date_published()
                .cmp(b.date_published())
                .then_with(|| a.build_id().cmp(b.build_id()))
        })
}

/// One row of a listing: what the table and the error messages show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildEntry {
    pub build_id: String,
    /// The version name; may be empty (old generation-1 builds).
    pub version: String,
    pub published: DateTime<Utc>,
    pub generation: u32,
    /// `None` on the default branch.
    pub branch: Option<String>,
    /// `false`: a private build, only listed with its password.
    pub public: bool,
}

impl BuildEntry {
    pub fn from_build(b: &Build) -> Self {
        Self {
            build_id: b.build_id().clone(),
            version: b.version_name().clone(),
            published: *b.date_published(),
            generation: *b.generation(),
            branch: b.branch().clone(),
            public: *b.public(),
        }
    }

    pub fn password_protected(&self) -> bool {
        !self.public
    }
}

/// The builds GOG lists for one game and OS, with the count GOG says it holds.
#[derive(Debug, Clone)]
pub struct Listing {
    /// GOG's `total_count`: far more than the listed builds.
    pub total_count: u32,
    pub builds: Vec<Build>,
    /// Fetched with a password: such a listing is never cached.
    pub with_password: bool,
}

impl Listing {
    /// Rows newest first by date, then build id (as [`pick_build`] breaks ties).
    pub fn entries(&self) -> Vec<BuildEntry> {
        let mut v: Vec<BuildEntry> = self.builds.iter().map(BuildEntry::from_build).collect();
        v.sort_by(|a, b| {
            b.published
                .cmp(&a.published)
                .then_with(|| b.build_id.cmp(&a.build_id))
        });
        v
    }

    /// The newest public build on the default branch (D3).
    pub fn newest(&self) -> Option<&Build> {
        pick_build(&self.builds)
    }

    /// Resolve a `--build` value (design 21 §3.2): an exact build id, else a
    /// version name equal to it (exactly, then case-insensitively) that only one
    /// listed build has. No partial match.
    pub fn resolve(&self, value: &str, ctx: &ResolveCtx<'_>) -> Result<&Build, BuildResolveError> {
        let value = value.trim();
        if let Some(b) = self.builds.iter().find(|b| b.build_id() == value) {
            return Ok(b);
        }
        // An empty version name is no name: it never matches.
        if !value.is_empty() {
            let exact: Vec<&Build> = self
                .builds
                .iter()
                .filter(|b| b.version_name() == value)
                .collect();
            let hits = if exact.is_empty() {
                self.builds
                    .iter()
                    .filter(|b| b.version_name().eq_ignore_ascii_case(value))
                    .collect()
            } else {
                exact
            };
            match hits.as_slice() {
                [] => {}
                [one] => return Ok(one),
                many => {
                    let mut matches: Vec<BuildEntry> =
                        many.iter().map(|b| BuildEntry::from_build(b)).collect();
                    matches.sort_by_key(|e| std::cmp::Reverse(e.published));
                    return Err(BuildResolveError::Ambiguous {
                        value: value.to_string(),
                        title: ctx.title.to_string(),
                        matches,
                    });
                }
            }
        }
        Err(BuildResolveError::Unknown {
            value: value.to_string(),
            title: ctx.title.to_string(),
            total_count: self.total_count,
            with_password: self.with_password,
            is_installed: ctx.installed == Some(value),
            listed: self.entries(),
        })
    }
}

/// What the error messages name.
pub struct ResolveCtx<'a> {
    /// The game's title.
    pub title: &'a str,
    /// The installed build's id, when the game is built.
    pub installed: Option<&'a str>,
}

/// A `--build` value that names no single listed build: a usage error (exit 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildResolveError {
    /// Not an id or a version name of any listed build.
    Unknown {
        value: String,
        title: String,
        total_count: u32,
        with_password: bool,
        /// The value is the installed build's id, which GOG no longer lists.
        is_installed: bool,
        listed: Vec<BuildEntry>,
    },
    /// A version name that several listed builds share (Anno 1701's `2.03`).
    Ambiguous {
        value: String,
        title: String,
        matches: Vec<BuildEntry>,
    },
}

impl BuildResolveError {
    /// The builds to show with the error, for `--json`.
    pub fn builds(&self) -> &[BuildEntry] {
        match self {
            Self::Unknown { listed, .. } => listed,
            Self::Ambiguous { matches, .. } => matches,
        }
    }
}

fn row(e: &BuildEntry) -> String {
    let version = if e.version.is_empty() {
        "-"
    } else {
        &e.version
    };
    format!(
        "  {}  {}  {}",
        e.build_id,
        version,
        e.published.format("%Y-%m-%d")
    )
}

impl fmt::Display for BuildResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown {
                value,
                title,
                total_count,
                with_password,
                is_installed,
                listed,
            } => {
                let n = listed.len();
                if *with_password {
                    write!(
                        f,
                        "no build \"{value}\" of {title} among the {n} builds GOG lists with that password:"
                    )?;
                } else {
                    write!(
                        f,
                        "no build \"{value}\" of {title} among the {n} GOG lists (of {total_count}):"
                    )?;
                }
                for e in listed {
                    write!(f, "\n{}", row(e))?;
                }
                if *is_installed {
                    write!(
                        f,
                        "\n  this is the installed build; GOG no longer lists it."
                    )?;
                }
                write!(
                    f,
                    "\n  older builds can't be downloaded: GOG no longer lists them."
                )
            }
            Self::Ambiguous {
                value,
                title,
                matches,
            } => {
                write!(
                    f,
                    "\"{value}\" names {} builds of {title}; pick one by id:",
                    matches.len()
                )?;
                for e in matches {
                    write!(f, "\n{}", row(e))?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for BuildResolveError {}

// ---- what a run does with the build (design 21 §3.4, D9) ----

impl BuildEntry {
    /// `2.31a (58989493373906337)`, or the id alone for a build with no version name.
    pub fn label(&self) -> String {
        if self.version.is_empty() {
            self.build_id.clone()
        } else {
            format!("{} ({})", self.version, self.build_id)
        }
    }

    /// Newest first: by date, then by id (as [`pick_build`] breaks ties).
    fn order(&self) -> (DateTime<Utc>, &str) {
        (self.published, self.build_id.as_str())
    }
}

/// The build a built game records in `content.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRef {
    pub build_id: String,
    pub version: Option<String>,
    /// `None` on the default branch.
    pub branch: Option<String>,
}

/// Why `--build` can't be honoured by a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickError {
    /// The value names no single listed build (exit 2).
    Resolve(BuildResolveError),
    /// GOG lists no build: the game has no depot lane, and installers come in the
    /// newest version only (D9, exit 2).
    NoDepotBuild { title: String },
}

impl fmt::Display for PickError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolve(e) => e.fmt(f),
            Self::NoDepotBuild { title } => write!(
                f,
                "--build picks a depot build, and {title} has none; {INSTALLER_NEWEST_ONLY}"
            ),
        }
    }
}

impl std::error::Error for PickError {}

/// The offline-installer lane has no builds to pick (design 21 §6, D9).
pub const INSTALLER_NEWEST_ONLY: &str =
    "installers come in the newest version only; --from-depot --build X picks a depot build";

/// Pure: the build a `--build <value>` run takes, or why it can't.
pub fn pick_for_run<'a>(
    listing: &'a Listing,
    value: &str,
    ctx: &ResolveCtx<'_>,
) -> Result<&'a Build, PickError> {
    if listing.builds.is_empty() {
        return Err(PickError::NoDepotBuild {
            title: ctx.title.to_string(),
        });
    }
    listing.resolve(value, ctx).map_err(PickError::Resolve)
}

/// Pure: the plan's lines about the build (design 21 §3.4): the first is the
/// `build:` line, any others follow it. `target` is the build the run takes;
/// `picked` says `--build` chose it; `installed` is the built game's build, for a
/// rebuild. Empty when there is nothing to say (a first build of the newest).
pub fn plan_lines(
    listing: &Listing,
    target: &Build,
    picked: bool,
    installed: Option<&InstalledRef>,
) -> Vec<String> {
    let entries = listing.entries();
    let t = BuildEntry::from_build(target);
    let newest = listing.newest().map(BuildEntry::from_build);
    let listed = installed.and_then(|i| entries.iter().find(|e| e.build_id == i.build_id));
    let same = installed.is_some_and(|i| i.build_id == t.build_id);
    let installed_name = |i: &InstalledRef| match i.version.as_deref() {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => i.build_id.clone(),
    };
    let mut why: Vec<String> = Vec::new();
    if picked {
        let mut p = String::from("picked");
        if let Some(b) = &t.branch {
            p.push_str(&format!(", on branch {b}"));
        }
        match &newest {
            Some(n) if n.build_id == t.build_id => p.push_str("; the newest"),
            Some(n) => p.push_str(&format!("; newest is {}", n.label_short())),
            None => {}
        }
        why.push(p);
    } else if !same
        && let (Some(i), Some(l)) = (installed, listed)
        && l.order() < t.order()
    {
        // `--build` takes a version name only when it is unique.
        let keep = if l.version.is_empty()
            || entries.iter().filter(|e| e.version == l.version).count() > 1
        {
            l.build_id.clone()
        } else {
            l.version.clone()
        };
        why.push(format!(
            "newer than the installed {}; --build {keep} keeps it",
            installed_name(i)
        ));
    }
    if let Some(i) = installed
        && !same
        && i.branch != t.branch
    {
        why.push(format!(
            "the installed one is on {}, this on {}",
            i.branch
                .as_deref()
                .map_or("the default branch".to_string(), |b| format!("branch {b}")),
            t.branch
                .as_deref()
                .map_or("the default branch".to_string(), |b| format!("branch {b}")),
        ));
    }
    let mut out = Vec::new();
    if !why.is_empty() {
        let why = why.join("; ");
        out.push(if t.version.is_empty() {
            format!("{} ({why})", t.build_id)
        } else {
            format!("{} ({}, {why})", t.version, t.build_id)
        });
    }
    // A build older than the installed one: saves may not load (D2, §3.4).
    if let (Some(i), Some(l)) = (installed, listed)
        && !same
        && t.order() < l.order()
    {
        out.push(format!(
            "downgrade from {}: saves made by a newer version may not load",
            installed_name(i)
        ));
    }
    out
}

impl BuildEntry {
    /// The version name, or the id for a build with none.
    fn label_short(&self) -> &str {
        if self.version.is_empty() {
            &self.build_id
        } else {
            &self.version
        }
    }
}

// ---- the listing cache (design 21 §4.2, D7) ----

/// A listing as the cache keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedListing {
    pub schema: u32,
    /// When the listing was fetched.
    pub checked: DateTime<Utc>,
    pub total_count: u32,
    /// gog-warp's `Build`, serialized as it is (manifest URLs included).
    pub items: Vec<Build>,
}

impl CachedListing {
    pub fn into_listing(self) -> Listing {
        Listing {
            total_count: self.total_count,
            builds: self.items,
            with_password: false,
        }
    }
}

/// `<cache>/gogo/builds/<os>/<gogid>.json`, beside `depot/` and `dependencies/`.
pub fn cache_path(cache_base: &Path, os: Platform, product_id: u64) -> PathBuf {
    cache_base
        .join("gogo")
        .join("builds")
        .join(os.to_string())
        .join(format!("{product_id}.json"))
}

/// Keep `listing`, fetched at `checked`: a temp file, then a rename. Never
/// expires. A listing fetched with a password is not kept (the hint must never
/// show a protected build): `Ok(false)` then.
pub fn write_cache(
    cache_base: &Path,
    os: Platform,
    product_id: u64,
    listing: &Listing,
    checked: DateTime<Utc>,
) -> std::io::Result<bool> {
    if listing.with_password {
        return Ok(false);
    }
    let path = cache_path(cache_base, os, product_id);
    let dir = path.parent().expect("cache path has a parent");
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_string(&CachedListing {
        schema: CACHE_SCHEMA,
        checked,
        total_count: listing.total_count,
        items: listing.builds.clone(),
    })
    .map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{product_id}.json.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(true)
}

/// The cached listing, or `None` when there is none, or it isn't one this gogo
/// can read (the cache is disposable: the next fetch rewrites it).
pub fn read_cache(cache_base: &Path, os: Platform, product_id: u64) -> Option<CachedListing> {
    let text = std::fs::read_to_string(cache_path(cache_base, os, product_id)).ok()?;
    let c: CachedListing = serde_json::from_str(&text).ok()?;
    (c.schema == CACHE_SCHEMA).then_some(c)
}

/// Fetch the builds list and keep it in the listing cache (best effort: a cache
/// that can't be written never fails the fetch). Not cached with a password.
pub async fn fetch_listing(
    core: &Core,
    product_id: u64,
    os: Platform,
    password: Option<String>,
) -> Result<Listing, gog_warp::errors::Error> {
    let with_password = password.is_some();
    let resp = core
        .get_builds(&product_id.to_string(), os.clone(), password)
        .await?;
    let listing = Listing {
        total_count: *resp.total_count(),
        builds: resp.items().to_vec(),
        with_password,
    };
    if let Some(base) = dirs::cache_dir() {
        let _ = write_cache(&base, os, product_id, &listing, Utc::now());
    }
    Ok(listing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(id: &str, version: &str, date: &str, branch: Option<&str>, public: bool) -> Build {
        serde_json::from_value(serde_json::json!({
            "build_id": id,
            "product_id": "1423049311",
            "os": "windows",
            "branch": branch,
            "version_name": version,
            "tags": [],
            "public": public,
            "date_published": date,
            "generation": 2,
            "urls": []
        }))
        .unwrap()
    }

    fn listing(builds: Vec<Build>, total: u32) -> Listing {
        Listing {
            total_count: total,
            builds,
            with_password: false,
        }
    }

    /// Cyberpunk's shape: a legacy branch build published *after* the default
    /// branch's older ones, and newer than some.
    fn cyberpunk() -> Listing {
        listing(
            vec![
                b(
                    "60152385347545148",
                    "2.5_RedModFix",
                    "2026-10-22T15:28:42Z",
                    None,
                    true,
                ),
                b(
                    "60148683633405433",
                    "2.5",
                    "2026-10-21T10:00:00Z",
                    None,
                    true,
                ),
                b(
                    "58989493373906337",
                    "2.31a",
                    "2025-09-16T10:00:00Z",
                    None,
                    true,
                ),
                b(
                    "58809037770927044",
                    "2.3",
                    "2025-07-16T10:00:00Z",
                    None,
                    true,
                ),
                b(
                    "56624919703786522",
                    "1.63_Hotfix",
                    "2026-12-01T10:00:00Z",
                    Some("1_63_legacy_patch"),
                    true,
                ),
            ],
            270,
        )
    }

    fn ctx() -> ResolveCtx<'static> {
        ResolveCtx {
            title: "Cyberpunk 2077",
            installed: None,
        }
    }

    #[test]
    fn a_named_branch_is_never_picked_even_when_newest() {
        let l = cyberpunk();
        assert_eq!(l.newest().unwrap().build_id(), "60152385347545148");
    }

    #[test]
    fn games_without_a_named_branch_pick_as_before() {
        let items = vec![
            b("b1", "1", "2024-01-01T00:00:00Z", None, true),
            b("b2", "2", "2025-06-01T00:00:00Z", None, true),
            b("b3", "3", "2030-01-01T00:00:00Z", None, false),
        ];
        assert_eq!(pick_build(&items).unwrap().build_id(), "b2");
        let tie = vec![
            b("aaa", "1", "2025-01-01T00:00:00Z", None, true),
            b("zzz", "2", "2025-01-01T00:00:00Z", None, true),
        ];
        assert_eq!(pick_build(&tie).unwrap().build_id(), "zzz");
    }

    #[test]
    fn only_a_branch_build_means_no_pick() {
        let items = vec![b("x", "1", "2025-01-01T00:00:00Z", Some("beta"), true)];
        assert!(pick_build(&items).is_none());
    }

    #[test]
    fn entries_are_newest_first_and_carry_the_fields() {
        let e = cyberpunk().entries();
        assert_eq!(e[0].build_id, "56624919703786522");
        assert_eq!(e[0].branch.as_deref(), Some("1_63_legacy_patch"));
        assert_eq!(e[1].version, "2.5_RedModFix");
        assert_eq!(e[1].generation, 2);
        assert!(!e[1].password_protected());
    }

    #[test]
    fn resolves_an_id_and_a_unique_version() {
        let l = cyberpunk();
        assert_eq!(
            l.resolve("58989493373906337", &ctx())
                .unwrap()
                .version_name(),
            "2.31a"
        );
        assert_eq!(
            l.resolve("2.5", &ctx()).unwrap().build_id(),
            "60148683633405433"
        );
        // Exact beats a case-insensitive near match; case-insensitive works alone.
        assert_eq!(l.resolve("2.31A", &ctx()).unwrap().version_name(), "2.31a");
        // The legacy-branch build can be picked explicitly.
        assert_eq!(
            l.resolve("1.63_Hotfix", &ctx()).unwrap().build_id(),
            "56624919703786522"
        );
    }

    #[test]
    fn no_partial_match() {
        let e = cyberpunk().resolve("2.", &ctx()).unwrap_err();
        assert!(matches!(e, BuildResolveError::Unknown { .. }));
    }

    #[test]
    fn unknown_lists_the_builds() {
        let e = cyberpunk().resolve("2.2", &ctx()).unwrap_err();
        let s = e.to_string();
        assert!(
            s.starts_with("no build \"2.2\" of Cyberpunk 2077 among the 5 GOG lists (of 270):"),
            "{s}"
        );
        assert!(
            s.contains("  60152385347545148  2.5_RedModFix  2026-10-22"),
            "{s}"
        );
        assert!(
            s.ends_with("older builds can't be downloaded: GOG no longer lists them."),
            "{s}"
        );
        assert!(!s.contains("installed build"));
        assert_eq!(e.builds().len(), 5);
    }

    #[test]
    fn the_installed_build_that_is_not_listed_is_said() {
        let c = ResolveCtx {
            title: "Cyberpunk 2077",
            installed: Some("1234"),
        };
        let s = cyberpunk().resolve("1234", &c).unwrap_err().to_string();
        assert!(
            s.contains("this is the installed build; GOG no longer lists it"),
            "{s}"
        );
    }

    #[test]
    fn a_password_changes_the_wording() {
        let mut l = cyberpunk();
        l.with_password = true;
        let s = l.resolve("nope", &ctx()).unwrap_err().to_string();
        assert!(
            s.contains("among the 5 builds GOG lists with that password"),
            "{s}"
        );
    }

    /// Anno 1701: two generation-1 builds, both `2.03`.
    #[test]
    fn a_shared_version_name_is_ambiguous_but_its_ids_work() {
        let l = listing(
            vec![
                b(
                    "48297303558037351",
                    "2.03",
                    "2015-08-12T00:00:00Z",
                    None,
                    true,
                ),
                b(
                    "48303309026040603",
                    "2.03",
                    "2015-08-14T00:00:00Z",
                    None,
                    true,
                ),
            ],
            2,
        );
        let c = ResolveCtx {
            title: "Anno 1701 A.D.",
            installed: None,
        };
        let e = l.resolve("2.03", &c).unwrap_err();
        assert_eq!(
            e.to_string(),
            "\"2.03\" names 2 builds of Anno 1701 A.D.; pick one by id:\n  \
             48303309026040603  2.03  2015-08-14\n  48297303558037351  2.03  2015-08-12"
        );
        assert_eq!(
            l.resolve("48297303558037351", &c).unwrap().build_id(),
            "48297303558037351"
        );
    }

    #[test]
    fn case_insensitive_duplicates_are_ambiguous_too() {
        let l = listing(
            vec![
                b("1", "Beta", "2025-01-01T00:00:00Z", None, true),
                b("2", "beta", "2025-02-01T00:00:00Z", None, true),
            ],
            2,
        );
        assert!(matches!(
            l.resolve("BETA", &ctx()).unwrap_err(),
            BuildResolveError::Ambiguous { .. }
        ));
        // An exact hit wins over the case-insensitive one.
        assert_eq!(l.resolve("beta", &ctx()).unwrap().build_id(), "2");
    }

    /// Old generation-1 builds have no version name: an empty value never
    /// matches them, they show as `-`, and their ids work.
    #[test]
    fn empty_version_names_match_nothing_but_the_id() {
        let l = listing(
            vec![
                b("111", "", "2015-01-01T00:00:00Z", None, true),
                b("222", "", "2015-02-01T00:00:00Z", None, true),
                b("333", "1.0", "2016-01-01T00:00:00Z", None, true),
            ],
            3,
        );
        let e = l.resolve("", &ctx()).unwrap_err();
        assert!(matches!(e, BuildResolveError::Unknown { .. }));
        assert!(e.to_string().contains("  111  -  2015-01-01"), "{e}");
        assert_eq!(l.resolve("111", &ctx()).unwrap().build_id(), "111");
        assert_eq!(l.resolve("1.0", &ctx()).unwrap().build_id(), "333");
    }

    #[test]
    fn a_password_protected_build_is_listed_and_pickable_but_never_newest() {
        let mut l = listing(
            vec![
                b("pub", "1.0", "2025-01-01T00:00:00Z", None, true),
                b("priv", "2.0-beta", "2025-06-01T00:00:00Z", None, false),
            ],
            2,
        );
        l.with_password = true;
        assert_eq!(l.newest().unwrap().build_id(), "pub");
        let e = l.entries();
        assert!(e[0].password_protected() && e[0].build_id == "priv");
        assert_eq!(l.resolve("2.0-beta", &ctx()).unwrap().build_id(), "priv");
    }

    // ---- cache ----

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn the_cache_lives_under_the_cache_dir_per_os_and_game() {
        let p = cache_path(Path::new("/c"), Platform::Windows, 42);
        assert_eq!(p, Path::new("/c/gogo/builds/windows/42.json"));
        assert_eq!(
            cache_path(Path::new("/c"), Platform::OsX, 42),
            Path::new("/c/gogo/builds/osx/42.json")
        );
    }

    #[test]
    fn the_cache_round_trips_builds_and_the_fetch_date() {
        let t = tempfile::tempdir().unwrap();
        assert!(read_cache(t.path(), Platform::Windows, 1423049311).is_none());
        let l = cyberpunk();
        let when = at("2026-11-12T09:14:03Z");
        assert!(write_cache(t.path(), Platform::Windows, 1423049311, &l, when).unwrap());
        let back = read_cache(t.path(), Platform::Windows, 1423049311).unwrap();
        assert_eq!(back.checked, when);
        assert_eq!(back.total_count, 270);
        let back = back.into_listing();
        assert_eq!(back.entries(), l.entries());
        assert_eq!(back.newest().unwrap().build_id(), "60152385347545148");
        // Another OS has its own file; no temp file is left.
        assert!(read_cache(t.path(), Platform::OsX, 1423049311).is_none());
        let dir = cache_path(t.path(), Platform::Windows, 1423049311);
        let names: Vec<_> = std::fs::read_dir(dir.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn a_newer_fetch_replaces_the_old_one_and_nothing_expires() {
        let t = tempfile::tempdir().unwrap();
        let old = listing(vec![b("a", "1", "2020-01-01T00:00:00Z", None, true)], 1);
        write_cache(
            t.path(),
            Platform::Windows,
            7,
            &old,
            at("2020-01-02T00:00:00Z"),
        )
        .unwrap();
        // Years later it still reads, with its own date.
        let c = read_cache(t.path(), Platform::Windows, 7).unwrap();
        assert_eq!(c.checked, at("2020-01-02T00:00:00Z"));
        write_cache(
            t.path(),
            Platform::Windows,
            7,
            &cyberpunk(),
            at("2026-11-12T00:00:00Z"),
        )
        .unwrap();
        let c = read_cache(t.path(), Platform::Windows, 7).unwrap();
        assert_eq!(c.checked, at("2026-11-12T00:00:00Z"));
        assert_eq!(c.items.len(), 5);
    }

    #[test]
    fn a_password_listing_is_not_cached() {
        let t = tempfile::tempdir().unwrap();
        let mut l = cyberpunk();
        l.with_password = true;
        assert!(!write_cache(t.path(), Platform::Windows, 7, &l, Utc::now()).unwrap());
        assert!(read_cache(t.path(), Platform::Windows, 7).is_none());
    }

    #[test]
    fn an_unreadable_or_foreign_cache_reads_as_none() {
        let t = tempfile::tempdir().unwrap();
        let p = cache_path(t.path(), Platform::Windows, 7);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "not json").unwrap();
        assert!(read_cache(t.path(), Platform::Windows, 7).is_none());
        std::fs::write(
            &p,
            r#"{"schema":99,"checked":"2026-01-01T00:00:00Z","totalCount":0,"items":[]}"#,
        )
        .unwrap();
        assert!(read_cache(t.path(), Platform::Windows, 7).is_none());
    }

    // ---- what a run does with the build (design 21 §3.4, D9) ----

    fn installed(id: &str, version: &str, branch: Option<&str>) -> InstalledRef {
        InstalledRef {
            build_id: id.into(),
            version: Some(version.into()),
            branch: branch.map(Into::into),
        }
    }

    #[test]
    fn a_picked_build_is_resolved_by_id_or_version() {
        let l = cyberpunk();
        assert_eq!(
            pick_for_run(&l, "2.31a", &ctx()).unwrap().build_id(),
            "58989493373906337"
        );
        assert_eq!(
            pick_for_run(&l, "58809037770927044", &ctx())
                .unwrap()
                .version_name(),
            "2.3"
        );
        // The newest on the default branch isn't what a legacy-branch pick means.
        assert_eq!(
            pick_for_run(&l, "1.63_Hotfix", &ctx()).unwrap().branch(),
            &Some("1_63_legacy_patch".to_string())
        );
        let e = pick_for_run(&l, "2.2", &ctx()).unwrap_err();
        assert!(matches!(
            e,
            PickError::Resolve(BuildResolveError::Unknown { .. })
        ));
        assert!(e.to_string().contains("no build \"2.2\""), "{e}");
    }

    #[test]
    fn a_game_with_no_builds_refuses_build_like_the_installer_lane() {
        let e = pick_for_run(&listing(vec![], 0), "2.5", &ctx()).unwrap_err();
        assert_eq!(
            e,
            PickError::NoDepotBuild {
                title: "Cyberpunk 2077".into()
            }
        );
        let msg = e.to_string();
        assert!(
            msg.contains("--build picks a depot build")
                && msg.contains("installers come in the newest version only"),
            "{msg}"
        );
    }

    #[test]
    fn the_plan_says_which_build_was_picked_and_what_is_newest() {
        let l = cyberpunk();
        let t = pick_for_run(&l, "2.31a", &ctx()).unwrap();
        assert_eq!(
            plan_lines(&l, t, true, None),
            ["2.31a (58989493373906337, picked; newest is 2.5_RedModFix)"]
        );
        let newest = pick_for_run(&l, "2.5_RedModFix", &ctx()).unwrap();
        assert_eq!(
            plan_lines(&l, newest, true, None),
            ["2.5_RedModFix (60152385347545148, picked; the newest)"]
        );
        // A build on a named branch says so, and the branch change.
        let legacy = pick_for_run(&l, "1.63_Hotfix", &ctx()).unwrap();
        let lines = plan_lines(&l, legacy, true, None);
        assert!(
            lines[0].contains("on branch 1_63_legacy_patch"),
            "{lines:?}"
        );
    }

    #[test]
    fn an_older_pick_than_the_installed_build_is_a_downgrade() {
        let l = cyberpunk();
        let inst = installed("60152385347545148", "2.5_RedModFix", None);
        let t = pick_for_run(&l, "2.31a", &ctx()).unwrap();
        let lines = plan_lines(&l, t, true, Some(&inst));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(
            lines[1],
            "downgrade from 2.5_RedModFix: saves made by a newer version may not load"
        );
        // Picking the installed build again is neither.
        let same = pick_for_run(&l, "2.5_RedModFix", &ctx()).unwrap();
        assert_eq!(plan_lines(&l, same, true, Some(&inst)).len(), 1);
    }

    #[test]
    fn a_build_newer_than_the_installed_one_says_how_to_keep_it() {
        let l = cyberpunk();
        let inst = installed("58989493373906337", "2.31a", None);
        let newest = l.newest().unwrap();
        assert_eq!(
            plan_lines(&l, newest, false, Some(&inst)),
            [
                "2.5_RedModFix (60152385347545148, newer than the installed 2.31a; --build 2.31a keeps it)"
            ]
        );
        // The installed build is the one taken (a warm cache): nothing to say.
        let same = pick_for_run(&l, "2.31a", &ctx()).unwrap();
        assert!(plan_lines(&l, same, false, Some(&inst)).is_empty());
        // A first build of the newest: nothing either.
        assert!(plan_lines(&l, newest, false, None).is_empty());
    }

    #[test]
    fn a_shared_version_name_keeps_the_installed_build_by_id() {
        let l = listing(
            vec![
                b("200", "2.03", "2015-08-14T10:00:00Z", None, true),
                b("100", "2.03", "2015-08-12T10:00:00Z", None, true),
                b("300", "2.04", "2015-09-01T10:00:00Z", None, true),
            ],
            2,
        );
        let inst = installed("100", "2.03", None);
        let lines = plan_lines(&l, l.newest().unwrap(), false, Some(&inst));
        assert!(lines[0].contains("--build 100 keeps it"), "{lines:?}");
    }

    #[test]
    fn a_game_on_a_named_branch_names_both_branches() {
        let l = cyberpunk();
        let inst = installed(
            "56624919703786522",
            "1.63_Hotfix",
            Some("1_63_legacy_patch"),
        );
        let lines = plan_lines(&l, l.newest().unwrap(), false, Some(&inst));
        assert!(
            lines[0].contains(
                "the installed one is on branch 1_63_legacy_patch, this on the default branch"
            ),
            "{lines:?}"
        );
    }
}

//! Installed vs newest build, read from caches only (design 21 §4, §6, §11 Q2/Q4).
//!
//! Nothing here touches the network: the installed build comes from
//! `content.toml`, the newest from the listing cache ([`super::builds`]) or, on
//! the installer lane, from the cached catalog entry. `apps list`, `apps show`
//! and `doctor` render the one [`BuildHint`].

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use gog_warp::content_system::types::{Build, Platform};
use serde::Serialize;

use super::builds::{self, CachedListing};
use super::catalog::Product;
use super::content::{Content, Lane};

/// Where the installed build stands against the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// No cache to compare with.
    Unknown,
    /// Installed is the newest on its branch (or newer: a private build).
    Current,
    /// A newer build is listed.
    Newer,
    /// The installed build isn't listed any more (depot lane).
    Gone,
}

/// A build named in a hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildRef {
    /// `None` on the installer lane: it has versions, not builds.
    pub id: Option<String>,
    pub version: String,
    pub published: Option<DateTime<Utc>>,
}

impl BuildRef {
    fn of(b: &Build) -> Self {
        Self {
            id: Some(b.build_id().clone()),
            version: b.version_name().clone(),
            published: Some(*b.date_published()),
        }
    }

    /// The version, or the id for a build with no version name.
    pub fn label(&self) -> String {
        label(&self.version, self.id.as_deref())
    }
}

fn label(version: &str, id: Option<&str>) -> String {
    if !version.is_empty() {
        version.to_string()
    } else {
        id.unwrap_or("-").to_string()
    }
}

/// What the caches say about one built game's build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildHint {
    pub lane: Lane,
    /// The installed build's id (depot lane).
    pub id: Option<String>,
    pub version: String,
    /// The installed build's branch: `None` is the default branch (and, for a
    /// gone build, unknown, so the default branch is compared).
    pub branch: Option<String>,
    pub published: Option<DateTime<Utc>>,
    pub state: State,
    /// Whether the installed build is in the cached listing; `None` with no cache.
    pub listed: Option<bool>,
    /// Newest on the game's own branch (§11 Q2); `null` when unknown.
    pub newest: Option<BuildRef>,
    /// A named-branch game only: the default branch's newest, when it is newer.
    pub default_newer: Option<BuildRef>,
    /// When the cache was written.
    pub checked: Option<DateTime<Utc>>,
    /// The cache is older than `catalog_ttl_hours`.
    pub stale: bool,
}

/// Newest public build on `branch` (`None`: the default branch), by date then id.
fn newest_on<'a>(builds: &'a [Build], branch: Option<&str>) -> Option<&'a Build> {
    builds
        .iter()
        .filter(|b| *b.public() && b.branch().as_deref() == branch)
        .max_by(|a, b| {
            a.date_published()
                .cmp(b.date_published())
                .then_with(|| a.build_id().cmp(b.build_id()))
        })
}

fn is_stale(checked: Option<DateTime<Utc>>, ttl_hours: u32, now: DateTime<Utc>) -> bool {
    checked.is_some_and(|c| now - c > Duration::hours(i64::from(ttl_hours)))
}

/// Depot lane: the installed build `id` (version `version` from `content.toml`)
/// against the cached `listing`.
pub fn depot_hint(
    id: &str,
    version: &str,
    listing: Option<&CachedListing>,
    ttl_hours: u32,
    now: DateTime<Utc>,
) -> BuildHint {
    let mut h = BuildHint {
        lane: Lane::Depot,
        id: Some(id.to_string()),
        version: version.to_string(),
        branch: None,
        published: None,
        state: State::Unknown,
        listed: None,
        newest: None,
        default_newer: None,
        checked: None,
        stale: false,
    };
    let Some(l) = listing else { return h };
    h.checked = Some(l.checked);
    h.stale = is_stale(h.checked, ttl_hours, now);
    let installed = l.items.iter().find(|b| b.build_id() == id);
    h.listed = Some(installed.is_some());
    match installed {
        Some(b) => {
            h.branch = b.branch().clone();
            h.published = Some(*b.date_published());
            if h.version.is_empty() {
                h.version = b.version_name().clone();
            }
            // A game follows its own branch (§11 Q2).
            let newest = newest_on(&l.items, h.branch.as_deref());
            let newer =
                |n: &Build| (n.date_published(), n.build_id()) > (b.date_published(), b.build_id());
            h.state = if newest.is_some_and(newer) {
                State::Newer
            } else {
                State::Current
            };
            h.newest = newest.map(BuildRef::of);
            if h.branch.is_some() {
                h.default_newer = builds::pick_build(&l.items)
                    .filter(|d| newer(d))
                    .map(BuildRef::of);
            }
        }
        None => {
            h.state = State::Gone;
            h.newest = builds::pick_build(&l.items).map(BuildRef::of);
        }
    }
    h
}

/// Installer lane (§6): the installed installer `version` against the cached
/// catalog entry's installer version. `catalog` is that version and when the
/// entry was cached. Versions can't be ordered, so any difference is "newer".
pub fn installer_hint(
    version: &str,
    catalog: Option<(String, DateTime<Utc>)>,
    ttl_hours: u32,
    now: DateTime<Utc>,
) -> BuildHint {
    let mut h = BuildHint {
        lane: Lane::Installer,
        id: None,
        version: version.to_string(),
        branch: None,
        published: None,
        state: State::Unknown,
        listed: None,
        newest: None,
        default_newer: None,
        checked: None,
        stale: false,
    };
    if let Some((v, at)) = catalog {
        h.checked = Some(at);
        h.stale = is_stale(h.checked, ttl_hours, now);
        h.state = if v == version {
            State::Current
        } else {
            State::Newer
        };
        h.newest = Some(BuildRef {
            id: None,
            version: v,
            published: None,
        });
    }
    h
}

impl BuildHint {
    /// `apps list`'s `build` column: the installed version, then `↑ newest`
    /// or `(gone)`. No cache: the version alone.
    pub fn column(&self) -> String {
        let me = label(&self.version, self.id.as_deref());
        match (self.state, &self.newest) {
            (State::Newer, Some(n)) => format!("{me} ↑ {}", n.label()),
            (State::Gone, _) => format!("{me} (gone)"),
            _ => me,
        }
    }

    fn checked_on(&self) -> String {
        self.checked.map_or(String::new(), |c| {
            format!(" (checked {})", c.format("%Y-%m-%d"))
        })
    }

    /// The update path for this lane, when the game is behind or gone.
    pub fn update_command(&self, gogid: &str) -> String {
        match self.lane {
            Lane::Depot => format!("gogo linux-app {gogid} --yes"),
            Lane::Installer => format!("gogo update {gogid}, then gogo linux-app {gogid} --yes"),
        }
    }

    /// `apps show`'s `build` section, one line each.
    pub fn show_lines(&self, gogid: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut inst = label(&self.version, self.id.as_deref());
        let mut extra = Vec::new();
        if let (Some(id), false) = (&self.id, self.version.is_empty()) {
            extra.push(format!("build {id}"));
        }
        if self.lane == Lane::Depot && self.listed == Some(true) {
            extra.push(
                self.branch
                    .as_ref()
                    .map_or("default branch".to_string(), |b| format!("branch {b}")),
            );
        }
        if let Some(p) = self.published {
            extra.push(p.format("%Y-%m-%d").to_string());
        }
        if !extra.is_empty() {
            inst = format!("{inst} ({})", extra.join(", "));
        }
        out.push(format!("installed {inst}"));
        let refresh = if self.stale && self.lane == Lane::Depot {
            " (gogo builds --installed refreshes)"
        } else {
            ""
        };
        match (&self.state, &self.newest) {
            (State::Unknown, _) => out.push("newest: unknown (gogo builds checks)".into()),
            (State::Gone, n) => {
                out.push(format!(
                    "installed build is no longer listed: dlc add and an update can't fetch its files{}",
                    self.checked_on()
                ));
                if let Some(n) = n {
                    out.push(format!("newest: {}{}", n.label(), self.checked_on()));
                }
                out.push(format!("rebuild: {}", self.update_command(gogid)));
            }
            (State::Newer, Some(n)) => {
                out.push(format!(
                    "newest: {} is newer{}{refresh}",
                    n.label(),
                    self.checked_on()
                ));
                out.push(format!("update: {}", self.update_command(gogid)));
            }
            (_, n) => out.push(format!(
                "newest: {}, up to date{}{refresh}",
                n.as_ref().map_or("-".into(), BuildRef::label),
                self.checked_on()
            )),
        }
        if let Some(d) = &self.default_newer {
            out.push(format!("default branch: {} is newer", d.label()));
        }
        out
    }

    /// `doctor`'s finding: `(is_warn, message, hint)`; `None` when there is
    /// nothing to say (up to date, or no cache).
    pub fn finding(&self, gogid: &str) -> Option<(bool, String, String)> {
        let me = label(&self.version, self.id.as_deref());
        match (self.state, &self.newest) {
            (State::Newer, Some(n)) => Some((
                false,
                format!(
                    "{} is newer than the installed {me}{}",
                    n.label(),
                    self.checked_on()
                ),
                self.update_command(gogid),
            )),
            (State::Gone, _) => Some((
                true,
                format!(
                    "the installed build {me} is no longer listed by GOG: dlc add and an \
                     update can't fetch its files{}",
                    self.checked_on()
                ),
                format!("rebuild: {}", self.update_command(gogid)),
            )),
            _ => None,
        }
    }
}

/// The hint for `content`, from the caches under `cache_base` (the platform
/// cache dir) and the catalog cache `catalog`. `None` when `content.toml`
/// names no build or version to compare.
pub fn for_content(
    content: &Content,
    gogid: &str,
    cache_base: Option<&Path>,
    catalog: Option<&Product>,
    catalog_cached_at: Option<DateTime<Utc>>,
    ttl_hours: u32,
    now: DateTime<Utc>,
) -> Option<BuildHint> {
    let version = content.version.clone().unwrap_or_default();
    match content.lane {
        Lane::Depot => {
            let id = content.build_id.as_deref()?;
            let listing = cache_base
                .zip(gogid.parse().ok())
                .and_then(|(b, n)| builds::read_cache(b, Platform::Windows, n));
            Some(depot_hint(id, &version, listing.as_ref(), ttl_hours, now))
        }
        Lane::Installer => {
            if version.is_empty() {
                return None;
            }
            let newest =
                catalog.and_then(|p| catalog_installer_version(p, content.language.as_deref()));
            Some(installer_hint(
                &version,
                newest.zip(catalog_cached_at),
                ttl_hours,
                now,
            ))
        }
    }
}

/// The catalog's Windows installer version, for the game's language when it is
/// known, else the first installer.
fn catalog_installer_version(p: &Product, language: Option<&str>) -> Option<String> {
    use super::catalog::Os;
    let installers = &p.downloads.as_ref()?.installers;
    let win = || installers.iter().filter(|i| i.os == Some(Os::Windows));
    let by_lang = language.and_then(|l| {
        win().find(|i| {
            i.language
                .as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(l))
        })
    });
    by_lang.or_else(|| win().next())?.version.clone()
}

/// The real thing: reads the platform cache dir, the catalog cache and
/// `catalog_ttl_hours`. No network.
pub fn for_game(content: &Content, gogid: &str) -> Option<BuildHint> {
    let ttl = crate::config::Config::load().map_or(24, |l| l.config.state.catalog_ttl_hours);
    let id: Option<u64> = gogid.parse().ok();
    let (product, at) = id
        .map(|n| {
            let p = crate::state::cache::product_path(n);
            let at = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .map(DateTime::<Utc>::from);
            (crate::state::cache::load_product(n).ok().flatten(), at)
        })
        .unwrap_or((None, None));
    for_content(
        content,
        gogid,
        dirs::cache_dir().as_deref(),
        product.as_ref(),
        at,
        ttl,
        Utc::now(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(id: &str, v: &str, date: &str, branch: Option<&str>, public: bool) -> Build {
        serde_json::from_value(serde_json::json!({
            "build_id": id, "product_id": "1", "os": "windows", "branch": branch,
            "version_name": v, "tags": [], "public": public,
            "date_published": date, "generation": 2, "urls": []
        }))
        .unwrap()
    }

    fn now() -> DateTime<Utc> {
        "2026-11-13T12:00:00Z".parse().unwrap()
    }

    fn cache(items: Vec<Build>, checked: &str) -> CachedListing {
        CachedListing {
            schema: 1,
            checked: checked.parse().unwrap(),
            total_count: items.len() as u32,
            items,
        }
    }

    fn cp() -> CachedListing {
        cache(
            vec![
                b("3", "2.5_RedModFix", "2026-10-22T00:00:00Z", None, true),
                b("2", "2.31a", "2025-09-16T00:00:00Z", None, true),
                b("1", "2.3", "2025-07-16T00:00:00Z", None, true),
                b(
                    "9",
                    "1.63_Hotfix",
                    "2025-08-01T00:00:00Z",
                    Some("legacy"),
                    true,
                ),
                b("10", "1.62", "2025-06-01T00:00:00Z", Some("legacy"), true),
            ],
            "2026-11-12T09:00:00Z",
        )
    }

    #[test]
    fn up_to_date_says_nothing_in_doctor() {
        let h = depot_hint("3", "2.5_RedModFix", Some(&cp()), 24, now());
        assert_eq!(h.state, State::Current);
        assert_eq!(h.column(), "2.5_RedModFix");
        assert!(h.finding("1").is_none());
        assert!(h.show_lines("1").iter().any(|l| l.contains("up to date")));
    }

    #[test]
    fn a_newer_build_is_an_info_hint() {
        let h = depot_hint("2", "2.31a", Some(&cp()), 24, now());
        assert_eq!(h.state, State::Newer);
        assert_eq!(h.column(), "2.31a ↑ 2.5_RedModFix");
        let (warn, msg, hint) = h.finding("1423049311").unwrap();
        assert!(!warn);
        assert_eq!(
            msg,
            "2.5_RedModFix is newer than the installed 2.31a (checked 2026-11-12)"
        );
        assert_eq!(hint, "gogo linux-app 1423049311 --yes");
    }

    #[test]
    fn a_build_no_longer_listed_is_a_warning() {
        let h = depot_hint("77", "2.0", Some(&cp()), 24, now());
        assert_eq!(h.state, State::Gone);
        assert_eq!(h.listed, Some(false));
        assert_eq!(h.column(), "2.0 (gone)");
        let (warn, msg, _) = h.finding("1").unwrap();
        assert!(warn && msg.contains("no longer listed"));
    }

    #[test]
    fn a_named_branch_is_compared_on_its_own_branch() {
        // 1.62 on `legacy`: 1.63_Hotfix is the branch's newest; the default
        // branch's 2.5_RedModFix is noted separately.
        let h = depot_hint("10", "1.62", Some(&cp()), 24, now());
        assert_eq!(h.branch.as_deref(), Some("legacy"));
        assert_eq!(h.newest.as_ref().unwrap().version, "1.63_Hotfix");
        assert_eq!(h.default_newer.as_ref().unwrap().version, "2.5_RedModFix");
        // Current on its branch: no update, but the default-branch note stays.
        let h = depot_hint("9", "1.63_Hotfix", Some(&cp()), 24, now());
        assert_eq!(h.state, State::Current);
        assert!(h.finding("1").is_none());
        let lines = h.show_lines("1");
        assert!(lines.contains(&"default branch: 2.5_RedModFix is newer".to_string()));
        assert!(lines[0].contains("branch legacy"));
    }

    #[test]
    fn no_cache_is_unknown_and_quiet() {
        let h = depot_hint("2", "2.31a", None, 24, now());
        assert_eq!(h.state, State::Unknown);
        assert_eq!(h.column(), "2.31a");
        assert!(h.finding("1").is_none());
        assert!(
            h.show_lines("1")
                .contains(&"newest: unknown (gogo builds checks)".to_string())
        );
        assert_eq!(h.listed, None);
    }

    #[test]
    fn an_old_cache_says_how_to_refresh() {
        let mut l = cp();
        l.checked = "2026-11-01T00:00:00Z".parse().unwrap();
        let h = depot_hint("2", "2.31a", Some(&l), 24, now());
        assert!(h.stale);
        assert!(
            h.show_lines("1")
                .iter()
                .any(|x| x.contains("gogo builds --installed refreshes"))
        );
    }

    #[test]
    fn the_installer_lane_compares_the_catalog_version() {
        let at: DateTime<Utc> = "2026-11-10T00:00:00Z".parse().unwrap();
        let h = installer_hint("2.31a", Some(("2.5".into(), at)), 24, now());
        assert_eq!(h.state, State::Newer);
        assert_eq!(h.column(), "2.31a ↑ 2.5");
        assert_eq!(
            h.update_command("7"),
            "gogo update 7, then gogo linux-app 7 --yes"
        );
        let h = installer_hint("2.5", Some(("2.5".into(), at)), 24, now());
        assert_eq!(h.state, State::Current);
        assert!(h.finding("7").is_none());
        assert_eq!(installer_hint("2.5", None, 24, now()).state, State::Unknown);
    }

    #[test]
    fn for_content_reads_the_listing_cache_and_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let listing = cp();
        builds::write_cache(
            dir.path(),
            Platform::Windows,
            42,
            &listing.clone().into_listing(),
            listing.checked,
        )
        .unwrap();
        let c = Content {
            schema: 1,
            lane: Lane::Depot,
            build_id: Some("2".into()),
            version: Some("2.31a".into()),
            language: None,
            dlc: vec![],
        };
        let h = for_content(&c, "42", Some(dir.path()), None, None, 24, now()).unwrap();
        assert_eq!(h.state, State::Newer);
        // Another game has no cache.
        let h = for_content(&c, "43", Some(dir.path()), None, None, 24, now()).unwrap();
        assert_eq!(h.state, State::Unknown);
        // No build id, nothing to compare.
        let mut none = c.clone();
        none.build_id = None;
        assert!(for_content(&none, "42", Some(dir.path()), None, None, 24, now()).is_none());
    }
}

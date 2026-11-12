//! DLC selection (`docs/design/19` §1.1, §2, §8): which DLC of a game the account
//! can take, and how a `--dlc <id|title>` names one.
//!
//! Lane-neutral and pure where it can be: [`available`] joins the catalog's
//! `expanded_dlcs`, the owned-id set and (on the depot lane) the build manifest's
//! products; [`resolve`] turns names into ids. A DLC must be **owned**: GOG refuses
//! the secure link for a product the account doesn't own (design 19 §11.2 R4), so an
//! unowned name is a usage error listing what would have worked.
//!
//! SECURITY: the secure-link refusal's `error_description` contains the account id.
//! Nothing here keeps, formats or logs a response body ([`probe_licence`]).

use std::collections::HashSet;

use gog_warp::content_system::types::Manifest;
use serde::Serialize;

use crate::gog::catalog::Product;

/// Which DLC a depot run takes (design 19 §8). Replaces `with_dlcs: bool`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DlcSet {
    /// Base game only.
    #[default]
    None,
    /// These product ids (already resolved by [`resolve`]); each must be owned and
    /// in the build.
    Ids(Vec<u64>),
    /// Every DLC of the build the account owns.
    AllOwned,
}

/// A DLC that is installed in a built game (filled by the record, #348/#350).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledDlc {
    pub build_id: Option<String>,
}

/// One row of a game's DLC (design 19 §1.3, §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DlcInfo {
    pub id: u64,
    pub title: String,
    pub owned: bool,
    pub installed: Option<InstalledDlc>,
    /// In the build manifest's products, so the depot lane can fetch it.
    pub in_build: bool,
    /// Depot download bytes; `Some(0)` for a DLC with no depot bytes (a
    /// valid, record-only DLC). `None` when unknown: unowned, or not in the build.
    pub download_size: Option<u64>,
    pub disk_size: Option<u64>,
}

/// A manifest product other than the base game, with its depot bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDlc {
    pub id: u64,
    pub download_size: u64,
    pub disk_size: u64,
}

/// Every non-base product of `manifest` with its size for `language`:
/// `install_size(lang, [dlc]) - install_size(lang, [])`, without fetching any depot
/// manifest (design 19 §1.3). v1 and v2.
pub fn manifest_dlcs(manifest: &Manifest, language: &str) -> Vec<ManifestDlc> {
    let lang = language.to_string();
    let none: Vec<String> = Vec::new();
    let (base_dl, base_disk) = manifest.install_size(&lang, &none, None);
    manifest
        .dlcs()
        .into_iter()
        .filter_map(|id| {
            let parsed = id.parse::<u64>().ok()?;
            let one = vec![id];
            let (dl, disk) = manifest.install_size(&lang, &one, None);
            Some(ManifestDlc {
                id: parsed,
                download_size: dl.saturating_sub(base_dl),
                disk_size: disk.saturating_sub(base_disk),
            })
        })
        .collect()
}

fn has_installers(p: &Product) -> bool {
    p.downloads
        .as_ref()
        .is_some_and(|d| !d.installers.is_empty())
}

/// The game's DLC: the catalog's `expanded_dlcs` joined with the owned set and,
/// when given, the build manifest's products (design 19 §1.3).
///
/// - Rows are the catalog DLC (catalog order) then manifest products the catalog
///   doesn't know (titled by id).
/// - A catalog DLC with neither depot presence nor installers (goodies-only,
///   design 17 §1) isn't a row.
/// - `owned` is membership in `owned`; `in_build` membership in `manifest`.
/// - Sizes are given for an owned DLC that is in the build (unowned sizes are
///   unverified, design 17 Q2); a DLC with no depot bytes has size 0.
pub fn available(
    catalog_dlcs: &[Product],
    owned: &[u64],
    manifest: Option<&[ManifestDlc]>,
) -> Vec<DlcInfo> {
    let owned: HashSet<u64> = owned.iter().copied().collect();
    let in_manifest = |id: u64| manifest.and_then(|m| m.iter().find(|d| d.id == id));
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for p in catalog_dlcs {
        let m = in_manifest(p.id);
        if m.is_none() && !has_installers(p) {
            continue;
        }
        if !seen.insert(p.id) {
            continue;
        }
        out.push(row(
            p.id,
            p.title.clone().unwrap_or_else(|| p.id.to_string()),
            owned.contains(&p.id),
            m,
        ));
    }
    for m in manifest.unwrap_or_default() {
        if seen.insert(m.id) {
            out.push(row(m.id, m.id.to_string(), owned.contains(&m.id), Some(m)));
        }
    }
    out
}

fn row(id: u64, title: String, owned: bool, m: Option<&ManifestDlc>) -> DlcInfo {
    DlcInfo {
        id,
        title,
        owned,
        installed: None,
        in_build: m.is_some(),
        download_size: m.filter(|_| owned).map(|m| m.download_size),
        disk_size: m.filter(|_| owned).map(|m| m.disk_size),
    }
}

/// Where the DLC will come from; decides whether the build must contain it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lane {
    /// Offline installers: the catalog is enough.
    Installer,
    /// The depot lane for this build: the DLC must be in the manifest.
    Depot { build_id: String },
}

/// A selection error: always a usage error (exit 2), and always lists what would
/// have worked (design 19 §2.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DlcSelectError {
    #[error("unknown DLC \"{name}\" for {game}; its DLC: {}", list(.valid))]
    Unknown {
        name: String,
        game: String,
        valid: Vec<DlcInfo>,
    },
    #[error("you don't own \"{title}\" ({id}); owned DLC for {game}: {}", list_owned(.valid))]
    Unowned {
        title: String,
        id: u64,
        game: String,
        valid: Vec<DlcInfo>,
    },
    #[error(
        "\"{title}\" ({id}) isn't in build {build_id} on the depot lane (an installer-only DLC, \
         or the build predates it); use the offline lane (`gogo download --dlc`) or update the \
         game. DLC in this build: {}",
        list_in_build(.valid)
    )]
    NotInBuild {
        title: String,
        id: u64,
        build_id: String,
        valid: Vec<DlcInfo>,
    },
    /// A rebuild that keeps the recorded DLC meets one the build lacks (design 21
    /// D8): never dropped silently.
    #[error(
        "{} {} in build {build}, and the game has {} installed; pass --dlc without {} \
         (or --no-dlcs) to build without {}, or pick another build",
        names(.missing),
        if .missing.len() == 1 { "isn't" } else { "aren't" },
        if .missing.len() == 1 { "it" } else { "them" },
        if .missing.len() == 1 { "it" } else { "them" },
        if .missing.len() == 1 { "it" } else { "them" },
    )]
    KeptNotInBuild {
        /// `(id, title)` of each recorded DLC the build lacks.
        missing: Vec<(u64, String)>,
        build: String,
    },
    #[error("DLC \"{name}\" is ambiguous for {game}; it matches: {}", list(.matches))]
    Ambiguous {
        name: String,
        game: String,
        matches: Vec<DlcInfo>,
    },
}

fn names(v: &[(u64, String)]) -> String {
    v.iter()
        .map(|(id, t)| format!("\"{t}\" ({id})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pure (design 21 D8): the recorded DLC `kept` that build `build` (a label) lacks
/// are refused by name. `recorded` titles them; `infos` says what the build has.
pub fn check_kept(
    kept: &[u64],
    recorded: &[(u64, String)],
    infos: &[DlcInfo],
    build: &str,
) -> Result<(), DlcSelectError> {
    let missing: Vec<(u64, String)> = kept
        .iter()
        .filter(|id| !infos.iter().any(|d| d.id == **id && d.in_build))
        .map(|id| {
            let title = recorded
                .iter()
                .find(|(r, _)| r == id)
                .map(|(_, t)| t.clone())
                .or_else(|| infos.iter().find(|d| d.id == *id).map(|d| d.title.clone()))
                .unwrap_or_else(|| id.to_string());
            (*id, title)
        })
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(DlcSelectError::KeptNotInBuild {
            missing,
            build: build.to_string(),
        })
    }
}

fn one(d: &DlcInfo) -> String {
    format!(
        "{} {}{}",
        d.id,
        d.title,
        if d.owned { " (owned)" } else { "" }
    )
}

fn list(v: &[DlcInfo]) -> String {
    if v.is_empty() {
        return "none".into();
    }
    v.iter().map(one).collect::<Vec<_>>().join(", ")
}

fn list_owned(v: &[DlcInfo]) -> String {
    list(&v.iter().filter(|d| d.owned).cloned().collect::<Vec<_>>())
}

fn list_in_build(v: &[DlcInfo]) -> String {
    list(
        &v.iter()
            .filter(|d| d.owned && d.in_build)
            .cloned()
            .collect::<Vec<_>>(),
    )
}

/// Lowercase and collapse whitespace.
fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The forms a title can be named by: whole, and with the base game's title
/// stripped as a prefix (`Cyberpunk 2077: Phantom Liberty` → `phantom liberty`).
fn title_forms(title: &str, game_title: &str) -> Vec<String> {
    let full = norm(title);
    let base = norm(game_title);
    let mut forms = vec![full.clone()];
    if !base.is_empty()
        && let Some(rest) = full.strip_prefix(&base)
    {
        let rest = rest.trim_start_matches([' ', ':', '-', '–', '—']).trim();
        if !rest.is_empty() {
            forms.push(rest.to_string());
        }
    }
    forms
}

/// Check one DLC id can be taken from `lane`: it exists, is owned and, on the depot
/// lane, is in the build.
pub fn check_id(
    id: u64,
    game_title: &str,
    infos: &[DlcInfo],
    lane: &Lane,
) -> Result<(), DlcSelectError> {
    let Some(d) = infos.iter().find(|d| d.id == id) else {
        return Err(DlcSelectError::Unknown {
            name: id.to_string(),
            game: game_title.into(),
            valid: infos.to_vec(),
        });
    };
    if !d.owned {
        return Err(DlcSelectError::Unowned {
            title: d.title.clone(),
            id,
            game: game_title.into(),
            valid: infos.to_vec(),
        });
    }
    if let Lane::Depot { build_id } = lane
        && !d.in_build
    {
        return Err(DlcSelectError::NotInBuild {
            title: d.title.clone(),
            id,
            build_id: build_id.clone(),
            valid: infos.to_vec(),
        });
    }
    Ok(())
}

/// Resolve `names` (a product id, or a title: case-insensitive, whitespace-collapsed,
/// exact before unique substring, the base game's title stripped as a prefix) to
/// DLC ids (design 19 §2.1). Never guesses: two matches are
/// [`DlcSelectError::Ambiguous`]. Order and first occurrence are kept.
pub fn resolve(
    names: &[String],
    game_title: &str,
    infos: &[DlcInfo],
    lane: &Lane,
) -> Result<Vec<u64>, DlcSelectError> {
    let mut ids: Vec<u64> = Vec::new();
    for name in names {
        let name = name.trim();
        let id = if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) {
            name.parse::<u64>().unwrap_or(u64::MAX)
        } else {
            match_title(name, game_title, infos)?
        };
        check_id(id, game_title, infos, lane)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn match_title(name: &str, game_title: &str, infos: &[DlcInfo]) -> Result<u64, DlcSelectError> {
    let want = norm(name);
    let forms: Vec<(&DlcInfo, Vec<String>)> = infos
        .iter()
        .map(|d| (d, title_forms(&d.title, game_title)))
        .collect();
    let pick = |hits: Vec<&DlcInfo>| -> Option<Result<u64, DlcSelectError>> {
        match hits.as_slice() {
            [] => None,
            [d] => Some(Ok(d.id)),
            many => Some(Err(DlcSelectError::Ambiguous {
                name: name.into(),
                game: game_title.into(),
                matches: many.iter().map(|d| (*d).clone()).collect(),
            })),
        }
    };
    let exact: Vec<&DlcInfo> = forms
        .iter()
        .filter(|(_, f)| f.contains(&want))
        .map(|(d, _)| *d)
        .collect();
    if let Some(r) = pick(exact) {
        return r;
    }
    let partial: Vec<&DlcInfo> = forms
        .iter()
        .filter(|(_, f)| !want.is_empty() && f.iter().any(|f| f.contains(&want)))
        .map(|(d, _)| *d)
        .collect();
    if let Some(r) = pick(partial) {
        return r;
    }
    Err(DlcSelectError::Unknown {
        name: name.into(),
        game: game_title.into(),
        valid: infos.to_vec(),
    })
}

/// The ids a [`DlcSet`] means for a build: `Ids` after checking each against the
/// build, `AllOwned` every owned DLC in the manifest, in `infos` order.
pub fn select(
    set: &DlcSet,
    game_title: &str,
    infos: &[DlcInfo],
    lane: &Lane,
) -> Result<Vec<u64>, DlcSelectError> {
    match set {
        DlcSet::None => Ok(Vec::new()),
        DlcSet::Ids(ids) => {
            for id in ids {
                check_id(*id, game_title, infos, lane)?;
            }
            Ok(ids.clone())
        }
        DlcSet::AllOwned => Ok(infos
            .iter()
            .filter(|d| d.owned && (d.in_build || *lane == Lane::Installer))
            .map(|d| d.id)
            .collect()),
    }
}

/// What a build's DLC flags ask for, names already resolved to ids (design 19 §1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// No flag.
    Default,
    /// `--all-dlcs`.
    All,
    /// `--no-dlcs`.
    NoDlc,
    /// `--dlc X…`: exactly these.
    Ids(Vec<u64>),
}

/// How a build gets its DLC set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Basis {
    /// A set that is already decided.
    Set(DlcSet),
    /// Ask the person: these owned DLC, all preselected.
    Pick(Vec<u64>),
}

/// [`decide`]'s answer: the set (or a picker) and what the plan must say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub basis: Basis,
    /// Recorded DLC a rebuild leaves out (the plan names them).
    pub dropped: Vec<u64>,
    /// Owned DLC of the build that a build with no flag didn't take.
    pub not_included: usize,
}

/// Which DLC a build gets: design 19 §1.2's table. `recorded` is the DLC the game's
/// `content.toml` lists (`Some` for a rebuild), `infos` the game's DLC, and
/// `interactive` a TTY without `--yes`. A game with no owned DLC in the build never
/// asks.
pub fn decide(
    want: &Want,
    recorded: Option<&[u64]>,
    infos: &[DlcInfo],
    interactive: bool,
) -> Decision {
    let takeable: Vec<u64> = infos
        .iter()
        .filter(|d| d.owned && d.in_build)
        .map(|d| d.id)
        .collect();
    let dropped_from = |kept: &[u64]| -> Vec<u64> {
        recorded
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|id| !kept.contains(id))
            .collect()
    };
    let set = |basis, dropped| Decision {
        basis,
        dropped,
        not_included: 0,
    };
    match want {
        Want::Ids(ids) => set(Basis::Set(DlcSet::Ids(ids.clone())), dropped_from(ids)),
        Want::All => set(Basis::Set(DlcSet::AllOwned), Vec::new()),
        Want::NoDlc => set(Basis::Set(DlcSet::None), dropped_from(&[])),
        Want::Default => match recorded {
            Some(r) => set(Basis::Set(DlcSet::Ids(r.to_vec())), Vec::new()),
            None if takeable.is_empty() => set(Basis::Set(DlcSet::None), Vec::new()),
            None if interactive => set(Basis::Pick(takeable), Vec::new()),
            None => Decision {
                basis: Basis::Set(DlcSet::None),
                dropped: Vec::new(),
                not_included: takeable.len(),
            },
        },
    }
}

/// The plan's lines about the DLC choice (after the `dlc:` line).
pub fn decision_notes(d: &Decision, infos: &[DlcInfo]) -> Vec<String> {
    let name = |id: &u64| {
        infos
            .iter()
            .find(|i| i.id == *id)
            .map_or_else(|| id.to_string(), |i| i.title.clone())
    };
    let mut out = Vec::new();
    if d.not_included > 0 {
        out.push(format!(
            "{} owned DLC not included (--all-dlcs)",
            d.not_included
        ));
    }
    if !d.dropped.is_empty() {
        out.push(format!(
            "dropped from the recorded set: {}",
            d.dropped.iter().map(name).collect::<Vec<_>>().join(", ")
        ));
    }
    out
}

/// The offline-installer lane takes no DLC yet (design 19 §5, #351).
pub const INSTALLER_LANE_NOTE: &str =
    "DLC isn't installed on the offline-installer lane yet; the DLC flags are ignored";

/// What the secure-link probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Licence {
    Ok,
    /// `403` with `error: invalid_licence`: the account doesn't own the product.
    NotOwned,
}

/// Pure: whether a secure-link response is GOG's "not owned" refusal. Reads only the
/// `error` code; `error_description` (it holds the account id) is never looked at.
pub fn interpret_secure_link(status: u16, body: &str) -> Licence {
    if status != 403 {
        return Licence::Ok;
    }
    let code = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str().map(str::to_owned)));
    match code.as_deref() {
        Some("invalid_licence") => Licence::NotOwned,
        _ => Licence::Ok,
    }
}

/// `https://content-system.gog.com`, the secure-link host.
pub const CONTENT_SYSTEM_BASE: &str = "https://content-system.gog.com";

/// Ask GOG for `product_id`'s secure link and report only whether it is refused for
/// a missing licence. Anything else (a transport error, another status) is `Ok`: the
/// downloader reports those itself. The response body is dropped without being
/// kept, so no link token or account id can reach output.
pub async fn probe_licence(
    http: &reqwest::Client,
    base: &str,
    access_token: &str,
    product_id: u64,
    generation: u32,
    timestamp: Option<u32>,
) -> Licence {
    let path = if generation == 2 {
        "/".to_string()
    } else {
        format!("/windows/{}", timestamp.unwrap_or(0))
    };
    let mut query = vec![("_version", "2".to_string()), ("path", path)];
    if generation == 2 {
        query.push(("generation", "2".into()));
    } else {
        query.push(("type", "depot".into()));
    }
    let url = format!("{base}/products/{product_id}/secure_link");
    let Ok(resp) = http
        .get(url)
        .query(&query)
        .bearer_auth(access_token)
        .send()
        .await
    else {
        return Licence::Ok;
    };
    let status = resp.status().as_u16();
    if status != 403 {
        return Licence::Ok;
    }
    let body = resp.text().await.unwrap_or_default();
    interpret_secure_link(status, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME: &str = "Cyberpunk 2077";

    fn product(id: u64, title: &str, installers: bool) -> Product {
        let downloads = if installers {
            r#","downloads":{"installers":[{"id":"en1installer0","files":[]}]}"#
        } else {
            ""
        };
        serde_json::from_str(&format!(r#"{{"id":{id},"title":"{title}"{downloads}}}"#)).unwrap()
    }

    fn md(id: u64, dl: u64, disk: u64) -> ManifestDlc {
        ManifestDlc {
            id,
            download_size: dl,
            disk_size: disk,
        }
    }

    /// PL owned, REDmod owned (zero bytes), Soundtrack not owned, Extra owned but
    /// installer-only.
    fn infos() -> Vec<DlcInfo> {
        let catalog = vec![
            product(1256837418, "Cyberpunk 2077: Phantom Liberty", false),
            product(1597316373, "REDmod", false),
            product(5, "Cyberpunk 2077: Soundtrack Edition", false),
            product(6, "Extra Pack", true),
            product(7, "Only Goodies", false),
        ];
        let manifest = [
            md(1256837418, 29, 30),
            md(1597316373, 0, 0),
            md(5, 1, 1),
            md(99, 2, 2),
        ];
        available(&catalog, &[1256837418, 1597316373, 6, 99], Some(&manifest))
    }

    fn lane() -> Lane {
        Lane::Depot {
            build_id: "b1".into(),
        }
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn available_joins_catalog_owned_and_manifest() {
        let v = infos();
        let ids: Vec<u64> = v.iter().map(|d| d.id).collect();
        // goodies-only 7 is not a row; manifest-only 99 is, titled by id
        assert_eq!(ids, [1256837418, 1597316373, 5, 6, 99]);
        let pl = &v[0];
        assert!(pl.owned && pl.in_build);
        assert_eq!((pl.download_size, pl.disk_size), (Some(29), Some(30)));
        // a DLC with zero depot bytes is valid, size 0
        assert_eq!(v[1].download_size, Some(0));
        assert!(v[1].in_build);
        // unowned: no size
        assert!(!v[2].owned && v[2].download_size.is_none());
        // installer-only: owned, not in the build
        assert!(v[3].owned && !v[3].in_build && v[3].download_size.is_none());
        assert_eq!(v[4].title, "99");
    }

    #[test]
    fn without_a_manifest_only_installer_dlc_are_rows() {
        let catalog = vec![product(1, "A", false), product(2, "B", true)];
        let v = available(&catalog, &[2], None);
        assert_eq!(v.len(), 1);
        assert!(v[0].owned && !v[0].in_build);
    }

    #[test]
    fn resolves_by_id() {
        assert_eq!(
            resolve(&names(&["1597316373"]), GAME, &infos(), &lane()).unwrap(),
            [1597316373]
        );
    }

    #[test]
    fn resolves_by_exact_title_and_stripped_base_title() {
        let v = infos();
        for n in [
            "Cyberpunk 2077: Phantom Liberty",
            "phantom   LIBERTY",
            "Phantom Liberty",
        ] {
            assert_eq!(
                resolve(&names(&[n]), GAME, &v, &lane()).unwrap(),
                [1256837418],
                "{n}"
            );
        }
        // unique substring
        assert_eq!(
            resolve(&names(&["redm"]), GAME, &v, &lane()).unwrap(),
            [1597316373]
        );
    }

    #[test]
    fn repeats_dedupe_and_keep_order() {
        let got = resolve(
            &names(&["REDmod", "phantom liberty", "1597316373"]),
            GAME,
            &infos(),
            &lane(),
        )
        .unwrap();
        assert_eq!(got, [1597316373, 1256837418]);
    }

    #[test]
    fn ambiguous_lists_the_matches() {
        let catalog = vec![
            product(1, "Game: Soundtrack", true),
            product(2, "Game: Soundtrack Deluxe", true),
        ];
        let v = available(&catalog, &[1, 2], None);
        let e = resolve(&names(&["sound"]), "Game", &v, &Lane::Installer).unwrap_err();
        let DlcSelectError::Ambiguous { matches, .. } = &e else {
            panic!("{e:?}");
        };
        assert_eq!(matches.len(), 2);
        let text = e.to_string();
        assert!(text.contains("1 Game: Soundtrack") && text.contains("2 Game: Soundtrack Deluxe"));
        // exact wins over a longer title that contains it
        assert_eq!(
            resolve(&names(&["Soundtrack"]), "Game", &v, &Lane::Installer).unwrap(),
            [1]
        );
    }

    #[test]
    fn unknown_lists_the_game_dlc() {
        for n in ["nope", "12345", ""] {
            let e = resolve(&names(&[n]), GAME, &infos(), &lane()).unwrap_err();
            assert!(matches!(e, DlcSelectError::Unknown { .. }), "{n}");
            let t = e.to_string();
            assert!(
                t.starts_with(&format!("unknown DLC \"{n}\" for {GAME}")),
                "{t}"
            );
            assert!(t.contains("1256837418 Cyberpunk 2077: Phantom Liberty (owned)"));
        }
    }

    #[test]
    fn unowned_is_an_error_listing_owned_dlc() {
        let e = resolve(&names(&["Soundtrack Edition"]), GAME, &infos(), &lane()).unwrap_err();
        assert!(matches!(e, DlcSelectError::Unowned { id: 5, .. }));
        let t = e.to_string();
        assert!(t.starts_with("you don't own \"Cyberpunk 2077: Soundtrack Edition\" (5)"));
        assert!(t.contains("owned DLC for Cyberpunk 2077: 1256837418"));
        // by id too
        assert!(matches!(
            resolve(&names(&["5"]), GAME, &infos(), &lane()),
            Err(DlcSelectError::Unowned { .. })
        ));
    }

    #[test]
    fn installer_only_dlc_is_not_in_a_depot_build() {
        let e = resolve(&names(&["Extra Pack"]), GAME, &infos(), &lane()).unwrap_err();
        assert!(matches!(e, DlcSelectError::NotInBuild { id: 6, .. }));
        assert!(
            e.to_string()
                .contains("isn't in build b1 on the depot lane")
        );
        // the installer lane takes it
        assert_eq!(
            resolve(&names(&["Extra Pack"]), GAME, &infos(), &Lane::Installer).unwrap(),
            [6]
        );
    }

    #[test]
    fn all_owned_skips_unowned_manifest_products() {
        let v = infos();
        // 5 is in the manifest but unowned; 6 owned but not in the build
        assert_eq!(
            select(&DlcSet::AllOwned, GAME, &v, &lane()).unwrap(),
            [1256837418, 1597316373, 99]
        );
        assert_eq!(
            select(&DlcSet::AllOwned, GAME, &v, &Lane::Installer).unwrap(),
            [1256837418, 1597316373, 6, 99]
        );
        assert!(select(&DlcSet::None, GAME, &v, &lane()).unwrap().is_empty());
        assert!(select(&DlcSet::Ids(vec![5]), GAME, &v, &lane()).is_err());
        assert_eq!(
            select(&DlcSet::Ids(vec![1597316373]), GAME, &v, &lane()).unwrap(),
            [1597316373]
        );
    }

    #[test]
    fn secure_link_403_invalid_licence_is_not_owned() {
        let body = r#"{"error":"invalid_licence","error_description":"Account 4242424242 does not own product 5"}"#;
        assert_eq!(interpret_secure_link(403, body), Licence::NotOwned);
        assert_eq!(interpret_secure_link(403, "{}"), Licence::Ok);
        assert_eq!(interpret_secure_link(403, "not json"), Licence::Ok);
        assert_eq!(interpret_secure_link(200, body), Licence::Ok);
        assert_eq!(interpret_secure_link(500, body), Licence::Ok);
    }

    #[tokio::test]
    async fn probe_maps_the_403_and_output_never_has_the_description() {
        let server = wiremock::MockServer::start().await;
        let body = r#"{"error":"invalid_licence","error_description":"Account 4242424242 does not own product 5"}"#;
        wiremock::Mock::given(wiremock::matchers::path("/products/5/secure_link"))
            .respond_with(wiremock::ResponseTemplate::new(403).set_body_string(body))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::path("/products/6/secure_link"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string(r#"{"product_id":6,"urls":[]}"#),
            )
            .mount(&server)
            .await;
        let http = crate::gog::http().unwrap();
        let got = probe_licence(&http, &server.uri(), "t", 5, 2, None).await;
        assert_eq!(got, Licence::NotOwned);
        assert_eq!(
            probe_licence(&http, &server.uri(), "t", 6, 2, None).await,
            Licence::Ok
        );

        // the mapped error, as the CLI prints it (Display) or logs it (Debug)
        let e = check_id(5, GAME, &infos(), &lane()).unwrap_err();
        for text in [e.to_string(), format!("{e:?}")] {
            assert!(!text.contains("4242424242"), "{text}");
            assert!(!text.contains("error_description"), "{text}");
        }
        assert!(e.to_string().starts_with("you don't own"));
    }

    #[test]
    fn default_set_follows_the_table() {
        let all = infos();
        let takeable: Vec<u64> = all
            .iter()
            .filter(|d| d.owned && d.in_build)
            .map(|d| d.id)
            .collect();
        assert!(!takeable.is_empty());
        // first build, interactive: a picker with every owned DLC
        let d = decide(&Want::Default, None, &all, true);
        assert_eq!(d.basis, Basis::Pick(takeable.clone()));
        // first build, --yes / no TTY: none, and the count
        let d = decide(&Want::Default, None, &all, false);
        assert_eq!(d.basis, Basis::Set(DlcSet::None));
        assert_eq!(d.not_included, takeable.len());
        assert_eq!(
            decision_notes(&d, &all),
            vec![format!(
                "{} owned DLC not included (--all-dlcs)",
                takeable.len()
            )]
        );
        // no owned DLC: no picker, no note
        let d = decide(&Want::Default, None, &[], true);
        assert_eq!(d.basis, Basis::Set(DlcSet::None));
        assert!(decision_notes(&d, &[]).is_empty());
        // flags on a first build
        assert_eq!(
            decide(&Want::All, None, &all, false).basis,
            Basis::Set(DlcSet::AllOwned)
        );
        assert_eq!(
            decide(&Want::NoDlc, None, &all, true).basis,
            Basis::Set(DlcSet::None)
        );
        assert_eq!(
            decide(&Want::Ids(vec![7]), None, &all, true).basis,
            Basis::Set(DlcSet::Ids(vec![7]))
        );
    }

    #[test]
    fn a_rebuild_keeps_narrows_or_drops_the_recorded_set() {
        let all = infos();
        let rec = [1256837418, 1597316373];
        // no flag: the recorded set, whatever the terminal
        for interactive in [true, false] {
            let d = decide(&Want::Default, Some(&rec), &all, interactive);
            assert_eq!(d.basis, Basis::Set(DlcSet::Ids(rec.to_vec())));
            assert!(d.dropped.is_empty());
        }
        // --dlc X: exactly X, the plan names what's dropped
        let d = decide(&Want::Ids(vec![1597316373]), Some(&rec), &all, false);
        assert_eq!(d.basis, Basis::Set(DlcSet::Ids(vec![1597316373])));
        assert_eq!(d.dropped, vec![1256837418]);
        assert_eq!(
            decision_notes(&d, &all),
            vec!["dropped from the recorded set: Cyberpunk 2077: Phantom Liberty".to_string()]
        );
        // --no-dlcs: all dropped; --all-dlcs: none
        let d = decide(&Want::NoDlc, Some(&rec), &all, false);
        assert_eq!(d.dropped, rec.to_vec());
        assert!(
            decide(&Want::All, Some(&rec), &all, false)
                .dropped
                .is_empty()
        );
    }

    #[test]
    fn a_recorded_dlc_the_build_lacks_is_refused_by_name() {
        // Phantom Liberty is in the build; Extra Pack (installer-only) is not.
        let recorded = vec![(6, "Extra Pack".to_string())];
        let e = check_kept(&[1256837418, 6], &recorded, &infos(), "2.3 (588)").unwrap_err();
        assert_eq!(
            e,
            DlcSelectError::KeptNotInBuild {
                missing: vec![(6, "Extra Pack".into())],
                build: "2.3 (588)".into(),
            }
        );
        let msg = e.to_string();
        assert!(
            msg.contains("\"Extra Pack\" (6) isn't in build 2.3 (588)")
                && msg.contains("pass --dlc without it"),
            "{msg}"
        );
        // A DLC the catalog doesn't know at all is named by its id.
        let e = check_kept(&[42], &[], &infos(), "b").unwrap_err();
        assert!(e.to_string().contains("\"42\" (42)"), "{e}");
        assert!(check_kept(&[1256837418], &[], &infos(), "b").is_ok());
        assert!(check_kept(&[], &[], &infos(), "b").is_ok());
    }
}

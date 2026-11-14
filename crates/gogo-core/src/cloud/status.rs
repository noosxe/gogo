//! `gogo saves status` (design 22 §5.1, §5.2 steps 1–3): what a sync would
//! do, file by file, and nothing more.
//!
//! What it writes: the remote-config cache, and the main session when the
//! Galaxy token was refreshed ([`CloudAuth`] persists it). Nothing in the
//! cloud, nothing in the game's dirs or prefix, not the sync record: a
//! client-id change (D11) is reported and planned as a first sync, and the
//! record is set aside by the next sync. Cloud objects fetched to compare a
//! first sync's both-sided files are checked and hashed in memory only.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use futures::StreamExt;
use serde::Serialize;

use super::auth::CloudAuth;
use super::classify::{self, Action, Cloud, ConflictReason, Local};
use super::error::CloudError;
use super::http::StorageClient;
use super::locations::{Game, Lane, Location, Resolved, Skipped, resolve};
use super::record::{self, FileRecord, Record};
use super::remote_config::{self, RemoteConfigError, Source};
use super::scan::{self, LocalFile};
use super::storage::{self, Entry};

/// Downloads at a time (design 22 §5.4).
pub const CONCURRENCY: usize = 4;

/// Where `status` reads from.
pub struct Inputs<'a> {
    pub game: &'a Game,
    pub auth: &'a CloudAuth<'a>,
    pub storage: &'a StorageClient,
    /// For the remote config (gogo's own client and UA).
    pub http: &'a reqwest::Client,
    pub config_base: &'a str,
    pub cache_dir: &'a Path,
    /// `<data_root>/cloud-saves` ([`record::default_dir`]).
    pub record_dir: &'a Path,
}

/// The first, account-free half of a status: the client id and the save
/// locations.
pub enum Located {
    /// Cloud storage is off for the game, or GOG has no config for it: the
    /// plan says so and nothing else is needed.
    Off(Plan),
    On {
        client_id: String,
        resolved: Resolved,
        source: Source,
    },
}

/// One side of a file, as shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInfo {
    pub size: u64,
    pub mtime: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloudInfo {
    /// Stored (gzip) size.
    pub bytes: u64,
    pub hash: String,
    pub last_modified: Option<String>,
    pub tombstone: bool,
}

/// What a sync would do with one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilePlan {
    /// The object name, `<location>/<path>`.
    pub name: String,
    #[serde(flatten)]
    pub action: Action,
    pub local: Option<LocalInfo>,
    pub cloud: Option<CloudInfo>,
    /// In the sync record.
    pub recorded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocationPlan {
    pub name: String,
    pub path: PathBuf,
    pub exists: bool,
    /// `__default` (D10).
    pub unverified: bool,
    /// Every file with something to say; [`Action::Nothing`] ones included.
    pub files: Vec<FilePlan>,
    /// Local files left out by the excludes.
    pub local_excluded: usize,
    /// Local links, special files, odd names: never synced.
    pub ignored: Vec<String>,
}

/// Why a listed object is outside every location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmappedReason {
    /// Its first part names no declared location.
    NoLocation,
    /// Its location was skipped (see `skipped`).
    SkippedLocation,
    /// Its path can't be mapped safely below its location.
    BadPath,
    /// The excludes leave it out.
    Excluded,
}

/// A listed object no sync touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unmapped {
    pub name: String,
    pub reason: UnmappedReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Container {
    pub objects: usize,
    pub tombstones: usize,
    pub bytes_used: Option<u64>,
    pub quota_bytes: Option<u64>,
}

/// The plan: what `gogo saves sync` would do now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub gogid: String,
    pub lane: Lane,
    pub windows_user: &'static str,
    /// Cloud storage is on for the game's Windows build.
    pub enabled: bool,
    /// GOG has no remote config for the game (a 404).
    pub no_remote_config: bool,
    pub config_source: Source,
    /// No (usable) sync record: every file is decided on its own (D8).
    pub first_sync: bool,
    /// The record is for another client id (D11): the next sync sets it
    /// aside and is a first sync.
    pub client_id_changed: bool,
    pub rolled_back: bool,
    pub pending_push: bool,
    pub last_sync: Option<String>,
    /// Files per action (`upload`, `download`, `conflict`, …).
    pub counts: BTreeMap<String, usize>,
    pub locations: Vec<LocationPlan>,
    pub skipped: Vec<Skipped>,
    pub unmapped: Vec<Unmapped>,
    pub container: Option<Container>,
}

impl Plan {
    fn off(game: &Game, source: Source, no_remote_config: bool) -> Plan {
        Plan {
            gogid: game.gogid.clone(),
            lane: game.layout.lane,
            windows_user: game.layout.lane.windows_user(),
            enabled: false,
            no_remote_config,
            config_source: source,
            first_sync: false,
            client_id_changed: false,
            rolled_back: false,
            pending_push: false,
            last_sync: None,
            counts: BTreeMap::new(),
            locations: vec![],
            skipped: vec![],
            unmapped: vec![],
            container: None,
        }
    }

    /// Files the next sync can't settle alone.
    pub fn conflicts(&self) -> usize {
        self.count("conflict")
    }

    pub fn corrupt(&self) -> usize {
        self.count("corrupt")
    }

    pub fn count(&self, action: &str) -> usize {
        self.counts.get(action).copied().unwrap_or(0)
    }

    /// Every file, location by location.
    pub fn files(&self) -> impl Iterator<Item = &FilePlan> {
        self.locations.iter().flat_map(|l| l.files.iter())
    }
}

/// The name an action is counted and shown under.
pub fn action_name(a: Action) -> &'static str {
    match a {
        Action::Nothing => "nothing",
        Action::Upload => "upload",
        Action::Download => "download",
        Action::DeleteLocal => "delete_local",
        Action::DeleteCloud => "delete_cloud",
        Action::Record => "record",
        Action::Forget => "forget",
        Action::Conflict(_) => "conflict",
        Action::Corrupt => "corrupt",
    }
}

/// One file while it is planned.
#[derive(Default)]
struct Item {
    entry: Option<Entry>,
    local: Option<LocalFile>,
    record: Option<FileRecord>,
    case_clash: bool,
}

/// Run steps 1–3 of design 22 §5.2 for `inputs.game`.
pub async fn status(inputs: &Inputs<'_>) -> Result<Plan, CloudError> {
    match locate(
        inputs.game,
        inputs.http,
        inputs.config_base,
        inputs.cache_dir,
    )
    .await?
    {
        Located::Off(plan) => Ok(plan),
        Located::On {
            client_id,
            resolved,
            source,
        } => {
            plan(
                inputs.game,
                &client_id,
                &resolved,
                source,
                inputs.auth,
                inputs.storage,
                inputs.record_dir,
            )
            .await
        }
    }
}

/// The client id (no account: [`Game::client_id`]) and the remote config's
/// save locations. A game without cloud saves needs nothing more.
pub async fn locate(
    game: &Game,
    http: &reqwest::Client,
    config_base: &str,
    cache_dir: &Path,
) -> Result<Located, CloudError> {
    let client_id = game.client_id()?;
    let (cfg, source) = match remote_config::get_in(cache_dir, http, config_base, &client_id).await
    {
        Ok(c) => c,
        Err(RemoteConfigError::Status { status: 404, .. }) => {
            return Ok(Located::Off(Plan::off(game, Source::Network, true)));
        }
        Err(e) => return Err(e.into()),
    };
    let resolved = resolve(&cfg, &client_id, &game.layout);
    if !resolved.enabled {
        return Ok(Located::Off(Plan::off(game, source, false)));
    }
    Ok(Located::On {
        client_id,
        resolved,
        source,
    })
}

/// The second half: credentials, the per-game token, the listing, the local
/// files and the record, classified.
pub async fn plan(
    game: &Game,
    client_id: &str,
    resolved: &Resolved,
    source: Source,
    auth: &CloudAuth<'_>,
    storage_client: &StorageClient,
    record_dir: &Path,
) -> Result<Plan, CloudError> {
    let gogid = game.id()?;
    let record = record::load(record_dir, gogid)?;
    let installed = crate::gog::content::load(&game.root)
        .ok()
        .flatten()
        .and_then(|c| c.build_id);
    let creds = auth.credentials(game, installed.as_deref()).await?;
    if creds.client_id != client_id {
        tracing::debug!(
            credentials = %creds.client_id,
            game = %client_id,
            "cloud saves: client id mismatch"
        );
        return Err(CloudError::ClientIdMismatch { gogid });
    }
    let token = auth.token(gogid, &creds).await?;
    let listing = storage::list(storage_client, &token, client_id).await?;

    let client_id_changed = record.as_ref().is_some_and(|r| r.client_id != client_id);
    let rolled_back = record.as_ref().is_some_and(|r| r.rolled_back.is_some());
    let pending_push = record.as_ref().is_some_and(|r| r.pending_push);
    let last_sync = record
        .as_ref()
        .and_then(|r| r.last_sync.as_ref())
        .map(|d| d.to_string());
    // D11: a record for another client is no record.
    let usable: Option<&Record> = record.as_ref().filter(|_| !client_id_changed);

    let mut plan = Plan {
        gogid: game.gogid.clone(),
        lane: game.layout.lane,
        windows_user: game.layout.lane.windows_user(),
        enabled: true,
        no_remote_config: false,
        config_source: source,
        first_sync: usable.is_none(),
        client_id_changed,
        rolled_back,
        pending_push,
        last_sync,
        counts: BTreeMap::new(),
        locations: vec![],
        skipped: resolved.skipped.clone(),
        unmapped: vec![],
        container: Some(Container {
            objects: listing.entries.len(),
            tombstones: listing.entries.iter().filter(|e| e.is_tombstone()).count(),
            bytes_used: listing.bytes_used,
            quota_bytes: listing.quota_bytes,
        }),
    };

    let by_location = group(resolved, &listing.entries, &mut plan.unmapped);
    let mut pending: Vec<(usize, String, Entry)> = vec![];
    let mut staged: Vec<(LocationPlan, BTreeMap<String, Item>)> = vec![];
    for loc in &resolved.locations {
        let entries = by_location
            .get(loc.name.as_str())
            .cloned()
            .unwrap_or_default();
        let (lp, items) = gather(loc, entries, usable, &mut plan.unmapped)?;
        staged.push((lp, items));
    }

    // Local hashes where the table needs them, and the objects a first sync
    // must compare by content.
    let mut local_md5: HashMap<(usize, String), String> = HashMap::new();
    for (i, (_, items)) in staged.iter().enumerate() {
        for (name, item) in items {
            let (Some(local), cloud) = (&item.local, cloud_of(item)) else {
                continue;
            };
            let l = to_local(local, None);
            if classify::needs_local_md5(&l, &cloud, item.record.as_ref()) {
                local_md5.insert((i, name.clone()), scan::md5_file(&local.path)?);
            }
            if item.record.is_none()
                && let (Cloud::Present { .. }, Some(e)) = (&cloud, &item.entry)
            {
                pending.push((i, name.clone(), e.clone()));
            }
        }
    }
    let fetched = fetch_plain(storage_client, &token, client_id, pending).await?;

    for (i, (mut lp, items)) in staged.into_iter().enumerate() {
        for (name, item) in items {
            let key = (i, name.clone());
            let cloud = cloud_of(&item);
            let local = item
                .local
                .as_ref()
                .map(|l| to_local(l, local_md5.get(&key).cloned()));
            let action = match fetched.get(&key) {
                Some(None) => Some(Action::Corrupt),
                _ if item.case_clash => Some(Action::Conflict(ConflictReason::CaseClash)),
                other => {
                    let same_content = other
                        .and_then(|p| p.as_ref())
                        .map(|plain| local.as_ref().and_then(|l| l.md5.as_ref()) == Some(plain));
                    classify::classify(
                        classify::local_side(local.as_ref(), item.record.as_ref()),
                        classify::cloud_side(&cloud, item.record.as_ref()),
                        same_content,
                        rolled_back && usable.is_some(),
                    )
                }
            };
            let Some(action) = action else { continue };
            *plan
                .counts
                .entry(action_name(action).to_string())
                .or_default() += 1;
            lp.files.push(FilePlan {
                name,
                action,
                local: item.local.as_ref().map(|l| LocalInfo {
                    size: l.size,
                    mtime: l.mtime.to_string(),
                }),
                cloud: item.entry.as_ref().map(|e| CloudInfo {
                    bytes: e.bytes,
                    hash: e.hash.clone(),
                    last_modified: e.last_modified.clone(),
                    tombstone: e.is_tombstone(),
                }),
                recorded: item.record.is_some(),
            });
        }
        plan.locations.push(lp);
    }
    plan.unmapped.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plan)
}

fn cloud_of(item: &Item) -> Cloud {
    match &item.entry {
        None => Cloud::Absent,
        Some(e) if e.is_tombstone() => Cloud::Tombstone,
        Some(e) => Cloud::Present {
            hash: e.hash.clone(),
        },
    }
}

fn to_local(f: &LocalFile, md5: Option<String>) -> Local {
    Local {
        size: f.size,
        mtime: f.mtime,
        md5,
    }
}

/// The listing split by location name; objects outside every location go to
/// `unmapped` (tombstones there are history and are left out).
fn group<'e>(
    resolved: &Resolved,
    entries: &'e [Entry],
    unmapped: &mut Vec<Unmapped>,
) -> HashMap<&'e str, Vec<Entry>> {
    let mut out: HashMap<&str, Vec<Entry>> = HashMap::new();
    for e in entries {
        let loc = e.split().map(|(l, _)| l);
        match loc {
            Some(l) if resolved.locations.iter().any(|x| x.name == l) => {
                out.entry(l).or_default().push(e.clone());
            }
            _ if e.is_tombstone() => {}
            Some(l) if resolved.skipped.iter().any(|s| s.name == l) => unmapped.push(Unmapped {
                name: e.name.clone(),
                reason: UnmappedReason::SkippedLocation,
            }),
            _ => unmapped.push(Unmapped {
                name: e.name.clone(),
                reason: UnmappedReason::NoLocation,
            }),
        }
    }
    out
}

/// One location's files: its listed objects, its local files and its record
/// entries, matched by local path (Windows paths ignore case).
fn gather(
    loc: &Location,
    entries: Vec<Entry>,
    record: Option<&Record>,
    unmapped: &mut Vec<Unmapped>,
) -> Result<(LocationPlan, BTreeMap<String, Item>), CloudError> {
    let s = scan::scan(loc)?;
    let prefix = format!("{}/", loc.name);
    let mut items: BTreeMap<String, Item> = BTreeMap::new();
    for e in entries {
        let name = e.name.clone();
        items.entry(name).or_default().entry = Some(e);
    }
    if let Some(r) = record {
        for (name, f) in r.files.range(prefix.clone()..) {
            if !name.starts_with(&prefix) {
                break;
            }
            items.entry(name.clone()).or_default().record = Some(f.clone());
        }
    }

    let mut local_by_path: HashMap<PathBuf, LocalFile> =
        s.files.into_iter().map(|f| (f.path.clone(), f)).collect();
    let mut claimed: HashMap<PathBuf, String> = HashMap::new();
    let mut clashes = vec![];
    let mut drop = vec![];
    for (name, item) in items.iter_mut() {
        let rel = &name[prefix.len()..];
        if loc.excluded(rel) {
            if item.entry.as_ref().is_some_and(|e| !e.is_tombstone()) {
                unmapped.push(Unmapped {
                    name: name.clone(),
                    reason: UnmappedReason::Excluded,
                });
            }
            drop.push(name.clone());
            continue;
        }
        let path = match loc.file(rel) {
            Ok(p) => p,
            Err(_) => {
                if item.entry.as_ref().is_some_and(|e| !e.is_tombstone()) || item.record.is_some() {
                    unmapped.push(Unmapped {
                        name: name.clone(),
                        reason: UnmappedReason::BadPath,
                    });
                }
                drop.push(name.clone());
                continue;
            }
        };
        if let Some(other) = claimed.get(&path) {
            clashes.push(other.clone());
            item.case_clash = true;
        } else {
            claimed.insert(path.clone(), name.clone());
        }
        if let Some(f) = local_by_path.remove(&path) {
            item.local = Some(f);
        }
    }
    for name in drop {
        items.remove(&name);
    }
    for name in clashes {
        if let Some(i) = items.get_mut(&name) {
            i.case_clash = true;
        }
    }
    // Local files no listed or recorded name points at: new here.
    for (_, f) in local_by_path {
        let name = format!("{prefix}{}", f.rel);
        let item = items.entry(name).or_default();
        if item.local.is_none() {
            item.local = Some(f);
        }
    }
    let lp = LocationPlan {
        name: loc.name.clone(),
        path: loc.path.clone(),
        exists: loc.exists,
        unverified: loc.unverified,
        files: vec![],
        local_excluded: s.excluded,
        ignored: s.ignored,
    };
    Ok((lp, items))
}

/// The plain MD5 of each object, fetched [`CONCURRENCY`] at a time into
/// memory. `None` for one that failed its MD5 check or isn't gzip; any other
/// failure stops the run.
async fn fetch_plain(
    client: &StorageClient,
    token: &super::auth::CloudToken,
    client_id: &str,
    pending: Vec<(usize, String, Entry)>,
) -> Result<HashMap<(usize, String), Option<String>>, CloudError> {
    let results: Vec<_> = futures::stream::iter(pending)
        .map(|(i, name, e)| async move {
            let r = storage::plain_md5(client, token, client_id, &e).await;
            ((i, name), r)
        })
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;
    let mut out = HashMap::new();
    for (key, r) in results {
        match r {
            Ok((md5, _)) => {
                out.insert(key, Some(md5));
            }
            Err(CloudError::Corrupt { name, reason }) => {
                tracing::warn!("cloud object {name:?}: {reason}");
                out.insert(key, None);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;

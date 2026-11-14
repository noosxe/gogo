//! `gogo saves …` (design 22 §5): GOG cloud saves of a built game. The
//! read-only `locations` and `status` subcommands, and `pull`, which writes
//! local save files only; push and sync come later.

use gogo::cloud::build_files::{self, Lookup};
use gogo::cloud::classify::{Action, ConflictReason};
use gogo::cloud::error::CloudError;
use gogo::cloud::locations::{Game, Lane, Resolved, resolve};
use gogo::cloud::remote_config::{self, Source};
use gogo::cloud::status::{self as cloud_status, Located, Plan, UnmappedReason};

use crate::{AppError, cli};

#[derive(clap::Subcommand, Debug)]
pub enum SavesCmd {
    /// Where a built game's cloud saves live on this machine: GOG's save
    /// locations for the game (from its remote config, cached for a day)
    /// resolved onto the game's Wine prefix and files, with whether each
    /// dir exists yet. Reads only; nothing is written to the game, its
    /// prefix or the cloud, and no account is needed.
    Locations {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// What a sync of a built game's cloud saves would do now, file by
    /// file: downloads, uploads, deletions and conflicts, from GOG's cloud
    /// listing, the local save dirs and the record of the last sync. Reads
    /// only: nothing is written to the cloud, the game, its prefix or the
    /// sync record. Exits 8 when files need a decision, 5 when a cloud
    /// object fails its checksum.
    Status {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Bring the cloud's changes here: downloads and local deletions, as
    /// `status` plans them. Never writes to the cloud; uploads are left for
    /// `push`. Every local file it overwrites or deletes is copied to
    /// `cloud-saves/backups/<gogid>/` first, and on a game with snapshots
    /// a `cloud-pull` snapshot is taken before. Refuses while the game runs.
    /// Exits 8 when files that need a decision are left.
    Pull(PullArgs),
}

#[derive(clap::Args, Debug)]
pub struct PullArgs {
    /// GOG product id.
    #[arg(value_name = "GOGID")]
    pub gogid: String,
    /// Settle every conflict the cloud's way (`local` keeps the local file:
    /// that is `gogo saves push`'s side).
    #[arg(long, value_enum, value_name = "SIDE")]
    pub prefer: Option<Prefer>,
    /// Show what would be done; change nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Don't ask before overwriting or deleting local files.
    #[arg(long)]
    pub yes: bool,
    /// Machine-readable output (no prompts).
    #[arg(long)]
    pub json: bool,
    /// Skip the `cloud-pull` snapshot for this run (the backups are still
    /// made).
    #[arg(long)]
    pub no_snapshots: bool,
}

/// A pull settles conflicts only the cloud's way.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefer {
    Cloud,
}

pub async fn run(cmd: SavesCmd) -> Result<i32, AppError> {
    match cmd {
        SavesCmd::Locations { gogid, json } => locations(&gogid, json).await,
        SavesCmd::Status { gogid, json } => status(&gogid, json).await,
        SavesCmd::Pull(a) => pull(a).await,
    }
}

/// What a command prints and exits with; kept apart from the printing so
/// tests see all three.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    /// The one error → exit-code mapping of every `saves` command is
    /// [`CloudError::exit_code`].
    fn failed(e: &CloudError) -> Outcome {
        Outcome {
            code: e.exit_code(),
            stdout: String::new(),
            stderr: format!("gogo: {e}\n"),
        }
    }
}

async fn locations(gogid: &str, json: bool) -> Result<i32, AppError> {
    let lane = Lane::host();
    let game = match Game::find(gogid, lane) {
        Ok(g) => g,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let client_id = match game.client_id() {
        Ok(c) => c,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let http = gogo::gog::http().map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    let cache = gogo::state::cache::cache_dir();
    let out = report(
        &Request {
            gogid,
            client_id: &client_id,
            layout: &game.layout,
            game: Some(&game),
            json,
        },
        &cache,
        remote_config::BASE,
        &http,
    )
    .await;
    Ok(emit(out))
}

async fn status(gogid: &str, json: bool) -> Result<i32, AppError> {
    let game = match Game::find(gogid, Lane::host()) {
        Ok(g) => g,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let http = gogo::gog::http().map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    let cache = gogo::state::cache::cache_dir();
    let located = cloud_status::locate(&game, &http, remote_config::BASE, &cache).await;
    let (client_id, resolved, source) = match located {
        Err(e) => return Ok(emit(Outcome::failed(&e))),
        Ok(Located::Off(plan)) => return Ok(emit(status_outcome(Ok(plan), json))),
        Ok(Located::On {
            client_id,
            resolved,
            source,
        }) => (client_id, resolved, source),
    };
    let mut resolved = resolved;
    // Locations in the game's files need the build's file list (design 22
    // §3.4): offline when gogo has it, else the build's depot manifests.
    if resolved.locations.iter().any(|l| l.in_game_files()) {
        let list = build_list(build_files::lookup(&game), fetch_build_files).await;
        build_files::apply(&mut resolved, &game, list);
    }
    let cfg = crate::load_config_from_env()?;
    let store = gogo::auth::store::open(cfg.auth.token_store);
    let session = match gogo::auth::Session::restore(store.clone()).await {
        Ok(s) => s,
        Err(e) => {
            return Ok(emit(Outcome::failed(&gogo::cloud::auth::auth_error(e))));
        }
    };
    let auth = gogo::cloud::auth::CloudAuth::new(&session, &*store);
    let storage = match gogo::cloud::http::StorageClient::new() {
        Ok(s) => s,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let plan = cloud_status::plan(
        &game,
        &client_id,
        &resolved,
        source,
        &auth,
        &storage,
        &gogo::cloud::record::default_dir(),
    )
    .await;
    Ok(emit(status_outcome(plan, json)))
}

async fn pull(a: PullArgs) -> Result<i32, AppError> {
    use gogo::cloud::pull::{self as cloud_pull, LiveHost, PullOptions};
    let game = match Game::find(&a.gogid, Lane::host()) {
        Ok(g) => g,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let http = gogo::gog::http().map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    let cache = gogo::state::cache::cache_dir();
    let located = cloud_status::locate(&game, &http, remote_config::BASE, &cache).await;
    let (client_id, mut resolved, source) = match located {
        Err(e) => return Ok(emit(Outcome::failed(&e))),
        Ok(Located::Off(plan)) => return Ok(emit(status_outcome(Ok(plan), a.json))),
        Ok(Located::On {
            client_id,
            resolved,
            source,
        }) => (client_id, resolved, source),
    };
    if resolved.locations.iter().any(|l| l.in_game_files()) {
        let list = build_list(build_files::lookup(&game), fetch_build_files).await;
        build_files::apply(&mut resolved, &game, list);
    }
    let cfg = crate::load_config_from_env()?;
    let store = gogo::auth::store::open(cfg.auth.token_store);
    let session = match gogo::auth::Session::restore(store.clone()).await {
        Ok(s) => s,
        Err(e) => return Ok(emit(Outcome::failed(&gogo::cloud::auth::auth_error(e)))),
    };
    let auth = gogo::cloud::auth::CloudAuth::new(&session, &*store);
    let storage = match gogo::cloud::http::StorageClient::new() {
        Ok(s) => s,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let json = a.json;
    let mut progress = |ev: &gogo::prelude::ProgressEvent| {
        if json {
            return;
        }
        use gogo::prelude::ProgressEvent as E;
        match ev {
            E::SnapshotPruned { id, label, created } => eprintln!(
                "gogo: {}",
                gogo::linuxapp::snapshots::pruned_line(*id, label, created)
            ),
            E::Warn { message } => eprintln!("gogo: note: {message}"),
            _ => {}
        }
    };
    let mut host = LiveHost::new(&game, crate::auto_snapshots(a.no_snapshots)?, &mut progress);
    let mut decide = CliDecide {
        prefer_cloud: a.prefer == Some(Prefer::Cloud),
        yes: a.yes,
        interactive: !a.dry_run && crate::dlc_cmd::interactive(a.json),
    };
    let report = cloud_pull::pull(
        &cloud_pull::Inputs {
            game: &game,
            client_id: &client_id,
            resolved: &resolved,
            source,
            auth: &auth,
            storage: &storage,
            record_dir: &gogo::cloud::record::default_dir(),
        },
        &PullOptions {
            dry_run: a.dry_run,
            keep_backups: cfg.cloud_saves.keep_backups,
            now: chrono::Utc::now(),
            stop_before_rename: None,
        },
        &mut host,
        &mut decide,
    )
    .await;
    Ok(emit(pull_outcome(report, a.json)))
}

/// `--prefer`, `--yes` and the prompts.
pub struct CliDecide {
    pub prefer_cloud: bool,
    pub yes: bool,
    /// A TTY on stdin and stderr, no `--json`, not a dry run.
    pub interactive: bool,
}

impl gogo::cloud::pull::Decide for CliDecide {
    fn conflict(
        &mut self,
        file: &cloud_status::FilePlan,
        choices: &[gogo::cloud::pull::Choice],
    ) -> gogo::cloud::pull::Choice {
        use gogo::cloud::pull::Choice;
        if self.prefer_cloud {
            return Choice::Cloud;
        }
        if !self.interactive {
            return Choice::Skip;
        }
        let why = match file.action {
            Action::Conflict(r) => conflict_text(r),
            _ => "needs a decision",
        };
        let side = |s: Option<String>| s.unwrap_or_else(|| "none".into());
        let here = side(
            file.local
                .as_ref()
                .map(|l| format!("{} bytes, {}", l.size, clean(&l.mtime))),
        );
        let there = side(file.cloud.as_ref().map(|c| {
            if c.tombstone {
                "deleted".to_string()
            } else {
                format!(
                    "{} bytes stored, uploaded {}",
                    c.bytes,
                    clean(c.last_modified.as_deref().unwrap_or("?"))
                )
            }
        }));
        let items: Vec<&str> = choices
            .iter()
            .map(|c| match c {
                Choice::Cloud => "cloud: take the cloud's (the local file is backed up)",
                Choice::Local => "local: keep this one (push it later)",
                Choice::Both => "both: keep this one renamed, download the cloud's",
                Choice::Skip => "skip: decide later",
            })
            .collect();
        let prompt = format!(
            "{} ({why}); here: {here}; cloud: {there}",
            clean(&file.name)
        );
        let pick = crate::depot_progress::prompting(|| {
            dialoguer::Select::new()
                .with_prompt(prompt)
                .items(&items)
                .default(items.len() - 1)
                .interact()
        });
        pick.ok()
            .and_then(|i| choices.get(i).copied())
            .unwrap_or(Choice::Skip)
    }

    fn confirm(&mut self, c: &gogo::cloud::pull::Consent) -> gogo::cloud::pull::Confirm {
        use gogo::cloud::pull::Confirm;
        if self.yes {
            return Confirm::Yes;
        }
        if !self.interactive {
            return Confirm::CannotAsk;
        }
        let prompt = format!(
            "{}, backed up to {} first. Go ahead?",
            consent_text(c),
            clean(&c.backups.display().to_string())
        );
        let go = crate::depot_progress::prompting(|| {
            dialoguer::Confirm::new()
                .with_prompt(prompt)
                .default(false)
                .interact()
        })
        .unwrap_or(false);
        if go { Confirm::Yes } else { Confirm::No }
    }
}

fn consent_text(c: &gogo::cloud::pull::Consent) -> String {
    let mut parts = vec![];
    if c.overwrites > 0 {
        parts.push(format!(
            "{} download(s) overwrite local files",
            c.overwrites
        ));
    }
    if c.deletes > 0 {
        parts.push(format!("{} local file(s) deleted", c.deletes));
    }
    parts.join(", ")
}

/// A pull's output and exit code: its error's code when it stopped (6 a
/// failed write, 7 refused); 2 when consent was needed and couldn't be
/// asked, or was refused; then 5 a corrupt object, 1 an unreadable file, 8
/// files left for a decision, 0.
pub fn pull_outcome(
    report: Result<gogo::cloud::pull::PullReport, CloudError>,
    json: bool,
) -> Outcome {
    let r = match report {
        Ok(r) => r,
        Err(e) => return Outcome::failed(&e),
    };
    let mut stderr = String::new();
    let code = if let Some(e) = &r.error {
        stderr.push_str(&format!("gogo: {e}\n"));
        e.exit_code()
    } else if r.needs_consent {
        stderr.push_str(
            "gogo: pulling these saves overwrites or deletes local files and needs consent: \
             pass --yes\n",
        );
        cli::exit::USAGE
    } else if r.declined {
        stderr.push_str("gogo: aborted; nothing was changed\n");
        cli::exit::USAGE
    } else if r.corrupt() > 0 {
        cli::exit::CORRUPT
    } else if r.unreadable() > 0 {
        cli::exit::FAILED
    } else if r.left() > 0 {
        cli::exit::CONFLICTS
    } else {
        cli::exit::OK
    };
    let stdout = if json {
        format!(
            "{}\n",
            serde_json::to_string(&r).expect("a pull report serializes")
        )
    } else {
        render_pull(&r)
    };
    Outcome {
        code,
        stdout,
        stderr,
    }
}

/// The plain-text report of a pull.
pub fn render_pull(r: &gogo::cloud::pull::PullReport) -> String {
    use gogo::cloud::pull::{LeftReason, Op, State};
    let p = &r.plan;
    let mut out = String::new();
    if !p.enabled {
        return render_status(p);
    }
    let dry = r.dry_run;
    if dry {
        out.push_str("dry run: nothing is changed\n");
    }
    if p.config_source == Source::StaleCache {
        out.push_str("note: GOG's remote config couldn't be fetched; using an older cached copy\n");
    }
    if p.first_sync {
        out.push_str(
            "first sync: no record of an earlier one, so files on both sides are compared by content\n",
        );
    }
    for w in &r.warnings {
        out.push_str(&format!("note: {}\n", clean(w)));
    }
    for l in &p.locations {
        if l.missing {
            out.push_str(&format!(
                "warning: {} ({}) is gone but the last sync recorded files in it (a moved \
                 prefix, a drive not mounted?): nothing is downloaded there\n",
                clean(&l.name),
                clean(&l.path.display().to_string())
            ));
        }
    }
    let op_text = |o: Op| match o {
        Op::Download => "download",
        Op::Overwrite => "overwrite",
        Op::KeepBoth => "keep both",
        Op::DeleteLocal => "delete here",
        Op::Record => "same",
        Op::Forget => "forget",
    };
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    for f in &r.files {
        let (tag, note) = match (&f.state, f.op) {
            (State::Planned, Some(o)) => (op_text(o), String::new()),
            (State::Done, Some(o)) => (op_text(o), String::new()),
            (State::Done, None) => continue,
            (State::Left(why), _) => (
                "left",
                match (why, f.planned) {
                    (LeftReason::Undecided, Action::Conflict(c)) => {
                        format!(" ({}: needs a decision)", conflict_text(c))
                    }
                    (LeftReason::Undecided, _) => " (needs a decision)".into(),
                    (LeftReason::CaseClash, _) => {
                        " (cloud names differing only by case: settle it by hand)".into()
                    }
                    (LeftReason::LocationMissing, _) => " (the save dir is gone)".into(),
                    (LeftReason::NoPrefix, _) => {
                        " (the game's prefix isn't there yet: run the game once)".into()
                    }
                    (LeftReason::ChangedSincePlan, _) => {
                        " (changed since the plan was made: run the pull again)".into()
                    }
                },
            ),
            (State::ForPush, _) => ("for push", String::new()),
            (State::KeptLocal, _) => ("kept here", " (push it later)".into()),
            (State::Corrupt, _) => ("corrupt", " (failed its checksum; not downloaded)".into()),
            (State::Unreadable, _) => ("unreadable", String::new()),
            (State::NotDone, Some(o)) => ("not done", format!(" (would {})", op_text(o))),
            (_, None) => continue,
        };
        *counts.entry(tag).or_default() += 1;
        let extra = match &f.kept_as {
            Some(k) => format!(" (yours kept as {})", clean(k)),
            None => String::new(),
        };
        out.push_str(&format!("  {tag:<12}  {}{note}{extra}\n", clean(&f.name)));
    }
    if counts.is_empty() {
        out.push_str("nothing to pull\n");
    } else {
        let parts: Vec<String> = counts.iter().map(|(k, n)| format!("{n} {k}")).collect();
        out.push_str(&format!(
            "{}: {}\n",
            if dry { "would" } else { "pull" },
            parts.join(", ")
        ));
    }
    if let Some(s) = &r.snapshot {
        out.push_str(&format!("{}\n", clean(s)));
    }
    for p in &r.pruned {
        out.push_str(&format!(
            "pruned an old backup: {}\n",
            clean(&p.display().to_string())
        ));
    }
    if counts.contains_key("for push") || counts.contains_key("kept here") {
        out.push_str("local changes are left for `gogo saves push`\n");
    }
    if counts.contains_key("left") {
        out.push_str(
            "files that need a decision were left: re-run on a terminal, or with --prefer cloud\n",
        );
    }
    if let Some(d) = &r.backup_dir {
        out.push_str(&format!("backups: {}\n", clean(&d.display().to_string())));
    }
    out
}

/// The build-file list `lookup` leads to: as found, fetched with `fetch`
/// when only the build's depot manifests have it, or why there is none. A
/// fetch that fails or gives an empty list is no list.
pub(crate) async fn build_list(
    lookup: Lookup,
    fetch: impl AsyncFnOnce(
        gogo::linuxapp::rollback::FetchWant,
    ) -> Option<gogo::snapshot::saves::BuildFiles>,
) -> Result<gogo::snapshot::saves::BuildFiles, String> {
    match lookup {
        Lookup::Known(l) => Ok(l),
        Lookup::Unknown(why) => Err(why),
        Lookup::Fetch(want) => fetch(want)
            .await
            .filter(|l| !l.is_empty())
            .ok_or_else(|| build_files::NOT_FETCHED.to_string()),
    }
}

/// The build's file list from its depot manifests (the snapshot command's
/// fetcher, Linux only): `None` when it can't be had.
#[cfg(target_os = "linux")]
async fn fetch_build_files(
    want: gogo::linuxapp::rollback::FetchWant,
) -> Option<gogo::snapshot::saves::BuildFiles> {
    crate::snapshot_cmd::fetch_build_files(want).await
}

/// The Mac lane has no depot-lane game to fetch for: no list, and the
/// location is skipped.
#[cfg(not(target_os = "linux"))]
async fn fetch_build_files(
    _want: gogo::linuxapp::rollback::FetchWant,
) -> Option<gogo::snapshot::saves::BuildFiles> {
    None
}

/// A status run's output and exit code: 0 a plan with nothing to decide, 8
/// conflicts (D6: a dry run exits as the run would), 1 a local file that
/// couldn't be read, 5 a corrupt object (first to win).
pub fn status_outcome(plan: Result<Plan, CloudError>, json: bool) -> Outcome {
    let plan = match plan {
        Ok(p) => p,
        Err(e) => return Outcome::failed(&e),
    };
    let code = if plan.corrupt() > 0 {
        cli::exit::CORRUPT
    } else if plan.unreadable() > 0 {
        cli::exit::FAILED
    } else if plan.conflicts() > 0 {
        cli::exit::CONFLICTS
    } else {
        cli::exit::OK
    };
    let stdout = if json {
        format!(
            "{}\n",
            serde_json::to_string(&plan).expect("a plan serializes")
        )
    } else {
        render_status(&plan)
    };
    Outcome {
        code,
        stdout,
        stderr: String::new(),
    }
}

fn conflict_text(r: ConflictReason) -> &'static str {
    match r {
        ConflictReason::BothChanged => "changed here and in the cloud",
        ConflictReason::LocalChangedCloudDeleted => "changed here, deleted in the cloud",
        ConflictReason::LocalDeletedCloudChanged => "deleted here, changed in the cloud",
        ConflictReason::BothNewDiffer => "on both sides with different content",
        ConflictReason::RolledBack => "a rollback is pending",
        ConflictReason::CaseClash => "cloud names differing only by case",
        ConflictReason::LocationMissing => "the save dir is gone; not taken for a deletion",
        ConflictReason::LocationEmptied => "the save dir is empty; not taken for a deletion",
    }
}

/// How each action reads in the text output, in the order shown.
const ACTIONS: &[(&str, &str)] = &[
    ("download", "download"),
    ("upload", "upload"),
    ("delete_local", "delete here"),
    ("delete_cloud", "delete in cloud"),
    ("conflict", "conflict"),
    ("corrupt", "corrupt"),
    ("unreadable", "unreadable"),
    ("record", "same"),
    ("forget", "forget"),
    ("nothing", "unchanged"),
];

fn label(a: Action) -> &'static str {
    let name = cloud_status::action_name(a);
    ACTIONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, l)| *l)
        .unwrap_or(name)
}

/// `s` with control characters escaped: names and times come from the
/// cloud or the disk and must not move the terminal's cursor.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// The plain-text plan.
pub fn render_status(p: &Plan) -> String {
    let mut out = String::new();
    if !p.enabled {
        let why = if p.no_remote_config {
            "GOG has no remote config for this game"
        } else {
            "GOG has them off for its Windows build"
        };
        out.push_str(&format!("{}: no cloud saves ({why})\n", p.gogid));
        return out;
    }
    if p.config_source == Source::StaleCache {
        out.push_str("note: GOG's remote config couldn't be fetched; using an older cached copy\n");
    }
    if p.client_id_changed {
        out.push_str(
            "note: the sync record is for another edition's client; the next sync sets it \
             aside and starts over\n",
        );
    }
    if p.first_sync {
        out.push_str(
            "first sync: no record of an earlier one, so files on both sides are compared by content\n",
        );
    } else if let Some(t) = &p.last_sync {
        out.push_str(&format!("last sync: {}\n", clean(t)));
    }
    if p.rolled_back {
        out.push_str(
            "note: a snapshot rollback is pending: every difference needs a decision \
             (--prefer local|cloud)\n",
        );
    }
    if p.pending_push {
        out.push_str("note: a push from an earlier run is still pending\n");
    }
    for l in &p.locations {
        let tag = if l.unverified {
            " (unverified default location)"
        } else {
            ""
        };
        out.push_str(&format!(
            "{}  {}{tag}\n",
            clean(&l.name),
            clean(&l.path.display().to_string())
        ));
        if let Some(b) = &l.build_files {
            out.push_str(&format!(
                "  left out as the game's own files: {} (list: {})\n",
                l.local_build_files,
                clean(&b.source)
            ));
        }
        if l.missing {
            out.push_str(
                "  warning: this save dir is gone but the last sync recorded files in it \
                 (a moved prefix, a drive not mounted?); none is taken for deleted\n",
            );
        } else if l.emptied {
            out.push_str(
                "  warning: this save dir is empty but the last sync recorded files in it; \
                 none is taken for deleted without a decision\n",
            );
        }
        let prefix = format!("{}/", l.name);
        for f in l.files.iter().filter(|f| f.action != Action::Nothing) {
            let rel = clean(f.name.strip_prefix(&prefix).unwrap_or(&f.name));
            let local = f
                .local
                .as_ref()
                .map(|x| format!("{}, {}", gogo::fmt::human_size(x.size), clean(&x.mtime)))
                .unwrap_or_else(|| "-".into());
            let cloud = match &f.cloud {
                Some(c) if c.tombstone => "deleted".to_string(),
                Some(c) => format!(
                    "{} stored, {}",
                    gogo::fmt::human_size(c.bytes),
                    clean(c.last_modified.as_deref().unwrap_or("?"))
                ),
                None => "-".into(),
            };
            let why = match f.action {
                Action::Conflict(r) => format!(" ({})", conflict_text(r)),
                _ => String::new(),
            };
            out.push_str(&format!(
                "  {:<15} {rel}{why}\n  {:<15}   here: {local}; cloud: {cloud}\n",
                label(f.action),
                ""
            ));
        }
        for i in &l.ignored {
            out.push_str(&format!(
                "  ignored: {} (a link or special file)\n",
                clean(i)
            ));
        }
    }
    let counts: Vec<String> = ACTIONS
        .iter()
        .filter_map(|(n, l)| {
            let c = p.count(n);
            (c > 0).then(|| format!("{c} {l}"))
        })
        .collect();
    if counts.is_empty() {
        out.push_str("nothing to sync: no saves on either side\n");
    } else {
        out.push_str(&format!("plan: {}\n", counts.join(", ")));
    }
    let unmapped = |r| p.unmapped.iter().filter(|u| u.reason == r).count();
    for (r, what) in [
        (UnmappedReason::NoLocation, "belong to no declared location"),
        (
            UnmappedReason::SkippedLocation,
            "belong to a skipped location",
        ),
        (
            UnmappedReason::BadPath,
            "have a path that can't be mapped safely",
        ),
        (
            UnmappedReason::Excluded,
            "are excluded (logs, caches, partial files)",
        ),
        (
            UnmappedReason::BuildFile,
            "have the path of one of the game's own files",
        ),
    ] {
        let n = unmapped(r);
        if n > 0 {
            out.push_str(&format!("note: {n} cloud objects {what}; never synced\n"));
        }
    }
    for s in &p.skipped {
        out.push_str(&format!(
            "warning: location {:?} skipped: {}\n",
            s.name,
            clean(&s.message)
        ));
    }
    if p.container.as_ref().is_some_and(|c| c.missing) {
        out.push_str("cloud: nothing stored for this game yet\n");
    } else if let Some(c) = &p.container {
        out.push_str(&format!(
            "cloud: {} objects ({} deletion markers){}\n",
            c.objects,
            c.tombstones,
            match (c.bytes_used, c.quota_bytes) {
                (Some(u), Some(q)) => format!(
                    ", {} of {} used",
                    gogo::fmt::human_size(u),
                    gogo::fmt::human_size(q)
                ),
                (Some(u), None) => format!(", {} used", gogo::fmt::human_size(u)),
                _ => String::new(),
            }
        ));
    }
    if p.conflicts() > 0 {
        out.push_str("conflicts need a decision: a sync will ask, or take --prefer local|cloud\n");
    }
    out
}

fn emit(o: Outcome) -> i32 {
    print!("{}", o.stdout);
    eprint!("{}", o.stderr);
    o.code
}

pub struct Request<'a> {
    pub gogid: &'a str,
    pub client_id: &'a str,
    pub layout: &'a gogo::cloud::locations::Layout,
    /// The built game, for the build-file list of locations in its files
    /// (none in tests that only resolve).
    pub game: Option<&'a Game>,
    pub json: bool,
}

/// Fetch (or read from the cache) the remote config and resolve it. A 404
/// means GOG has no config for the client: no cloud saves, exit 0. A real
/// network failure with no cache to fall back to is exit 4.
pub async fn report(
    req: &Request<'_>,
    cache: &std::path::Path,
    base: &str,
    http: &reqwest::Client,
) -> Outcome {
    let lane = req.layout.lane;
    let (resolved, source, no_config) =
        match remote_config::get_in(cache, http, base, req.client_id).await {
            Ok((cfg, source)) => {
                let mut r = resolve(&cfg, req.client_id, req.layout);
                if let Some(g) = req.game {
                    build_note(&mut r, g);
                }
                (r, source, false)
            }
            Err(remote_config::RemoteConfigError::Status { status: 404, .. }) => {
                (Resolved::off(), Source::Network, true)
            }
            Err(e) => return Outcome::failed(&e.into()),
        };
    let stdout = if req.json {
        let mut report = serde_json::to_value(&resolved).unwrap();
        report["gogid"] = serde_json::json!(req.gogid);
        report["lane"] = serde_json::json!(lane);
        report["windows_user"] = serde_json::json!(lane.windows_user());
        report["config_source"] = serde_json::json!(source);
        report["no_remote_config"] = serde_json::json!(no_config);
        format!("{report}\n")
    } else if no_config {
        format!(
            "{}: no cloud saves (GOG has no remote config for this game)\n",
            req.gogid
        )
    } else {
        render(req.gogid, source, &resolved)
    };
    Outcome {
        code: cli::exit::OK,
        stdout,
        stderr: String::new(),
    }
}

/// The build-file list of the locations in the game's files, offline
/// (design 22 §3.4): where it comes from, or the location skipped. A list
/// only the network has is named, and left to `saves status`.
fn build_note(r: &mut Resolved, game: &Game) {
    if !r.locations.iter().any(|l| l.in_game_files()) {
        return;
    }
    match build_files::lookup(game) {
        Lookup::Known(l) => build_files::apply(r, game, Ok(l)),
        Lookup::Unknown(why) => build_files::apply(r, game, Err(why)),
        Lookup::Fetch(_) => {
            for l in r.locations.iter_mut().filter(|l| l.in_game_files()) {
                l.build_files = Some(gogo::cloud::locations::BuildNote {
                    source: "the build's depot manifests, fetched by `saves status`".into(),
                    files: None,
                });
            }
        }
    }
}

/// The plain-text report.
pub fn render(gogid: &str, source: Source, r: &Resolved) -> String {
    let mut out = String::new();
    if !r.enabled {
        out.push_str(&format!(
            "{gogid}: no cloud saves (GOG has them off for its Windows build)\n"
        ));
        return out;
    }
    if source == Source::StaleCache {
        out.push_str("note: GOG's remote config couldn't be fetched; using an older cached copy\n");
    }
    if r.locations.is_empty() {
        out.push_str(&format!("{gogid}: no usable save locations\n"));
    }
    for l in &r.locations {
        let state = if l.exists { "exists" } else { "not there yet" };
        let tag = if l.unverified {
            " (unverified default location)"
        } else {
            ""
        };
        out.push_str(&format!(
            "{}  <?{}?>  {}  [{state}]{tag}\n",
            l.name,
            l.variable.name(),
            l.path.display()
        ));
        if let Some(b) = &l.build_files {
            let n = b.files.map(|n| format!(", {n} listed")).unwrap_or_default();
            out.push_str(&format!(
                "  in the game's files: the game's own files are left out (list: {}{n})\n",
                b.source
            ));
        }
    }
    for s in &r.skipped {
        out.push_str(&format!(
            "warning: location {:?} skipped: {}\n",
            s.name, s.message
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use gogo::cloud::locations::{Layout, resolve_storage};
    use gogo::cloud::remote_config::{CloudStorage, RawLocation};

    #[test]
    fn locations_parses() {
        let c = cli::Cli::try_parse_from(["gogo", "saves", "locations", "123", "--json"]).unwrap();
        let cli::Command::Saves {
            cmd: SavesCmd::Locations { gogid, json },
        } = c.command
        else {
            panic!("expected saves locations")
        };
        assert_eq!(gogid, "123");
        assert!(json);
    }

    fn resolved(enabled: bool, locs: &[(&str, &str)]) -> Resolved {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(dir.join("prefix")).unwrap();
        std::fs::create_dir_all(dir.join("game")).unwrap();
        let layout = Layout {
            lane: Lane::Linux,
            prefix: dir.join("prefix"),
            game_files: dir.join("game"),
        };
        let s = CloudStorage {
            enabled,
            locations: locs
                .iter()
                .map(|(n, l)| RawLocation {
                    name: n.to_string(),
                    location: l.to_string(),
                })
                .collect(),
        };
        resolve_storage(Some(&s), "1", &layout)
    }

    #[test]
    fn renders_locations_skips_and_the_off_case() {
        let r = resolved(true, &[("saves", "<?DOCUMENTS?>/G"), ("x", "<?NOPE?>/y")]);
        let t = render("5", Source::Network, &r);
        assert!(t.contains("saves  <?DOCUMENTS?>  "));
        assert!(t.contains("[not there yet]"));
        assert!(t.contains("warning: location \"x\" skipped: unknown variable <?NOPE?>"));
        let d = render("5", Source::StaleCache, &resolved(true, &[]));
        assert!(d.contains("older cached copy"));
        assert!(d.contains("__default") && d.contains("unverified"));
        let off = render("5", Source::Cache, &resolved(false, &[]));
        assert!(off.contains("no cloud saves"));
    }

    const CONFIG: &str = r#"{"content":{"Windows":{"cloudStorage":{"enabled":true,
        "locations":[{"name":"saves","location":"<?DOCUMENTS?>/G"}]}}}}"#;
    const CONFIG_OFF: &str =
        r#"{"content":{"Windows":{"cloudStorage":{"enabled":false,"locations":[]}}}}"#;

    struct Fixture {
        _dir: tempfile::TempDir,
        cache: std::path::PathBuf,
        layout: gogo::cloud::locations::Layout,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("prefix")).unwrap();
        std::fs::create_dir_all(base.join("game")).unwrap();
        Fixture {
            cache: base.join("cache"),
            layout: Layout {
                lane: Lane::Linux,
                prefix: base.join("prefix"),
                game_files: base.join("game"),
            },
            _dir: dir,
        }
    }

    async fn run_report(f: &Fixture, base: &str, json: bool) -> Outcome {
        report(
            &Request {
                gogid: "5",
                client_id: "77",
                layout: &f.layout,
                game: None,
                json,
            },
            &f.cache,
            base,
            &reqwest::Client::new(),
        )
        .await
    }

    async fn serving(status: u16, body: &str) -> wiremock::MockServer {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn locations_json_lists_resolved_and_skipped() {
        let f = fixture();
        let server = serving(200, CONFIG).await;
        let o = run_report(&f, &server.uri(), true).await;
        assert_eq!(o.code, 0);
        assert!(o.stderr.is_empty());
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["enabled"], true);
        assert_eq!(v["gogid"], "5");
        assert_eq!(v["lane"], "linux");
        assert_eq!(v["windows_user"], "steamuser");
        assert_eq!(v["config_source"], "network");
        assert_eq!(v["locations"][0]["name"], "saves");
        assert_eq!(v["locations"][0]["exists"], false);
        assert!(v.get("user_id").is_none() && v.get("token").is_none());
    }

    #[tokio::test]
    async fn disabled_storage_is_exit_0_with_enabled_false() {
        let f = fixture();
        let server = serving(200, CONFIG_OFF).await;
        let o = run_report(&f, &server.uri(), true).await;
        assert_eq!(o.code, 0);
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["enabled"], false);
        assert_eq!(v["locations"].as_array().unwrap().len(), 0);
        let o = run_report(&f, &server.uri(), false).await;
        assert_eq!(o.code, 0);
        assert!(o.stdout.contains("no cloud saves"));
    }

    #[tokio::test]
    async fn a_404_means_no_cloud_saves_not_a_failure() {
        let f = fixture();
        let server = serving(404, "").await;
        let o = run_report(&f, &server.uri(), false).await;
        assert_eq!(o.code, 0);
        assert!(o.stdout.contains("no remote config"));
        let o = run_report(&f, &server.uri(), true).await;
        assert_eq!(o.code, 0);
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["enabled"], false);
        assert_eq!(v["no_remote_config"], true);
    }

    #[tokio::test]
    async fn network_failure_is_exit_4_and_server_errors_too() {
        let f = fixture();
        let o = run_report(&f, "http://127.0.0.1:1", false).await;
        assert_eq!(o.code, cli::exit::NETWORK);
        assert!(o.stdout.is_empty() && o.stderr.starts_with("gogo: "));
        let server = serving(500, "").await;
        assert_eq!(
            run_report(&f, &server.uri(), false).await.code,
            cli::exit::NETWORK
        );
    }

    #[tokio::test]
    async fn a_foreign_200_is_a_failure_not_no_cloud_saves() {
        let f = fixture();
        let server = serving(200, "{}").await;
        let o = run_report(&f, &server.uri(), false).await;
        assert_eq!(o.code, cli::exit::FAILED);
    }

    #[tokio::test]
    async fn an_old_cache_stands_in_with_a_note_when_the_network_is_down() {
        let f = fixture();
        let server = serving(200, CONFIG).await;
        assert_eq!(run_report(&f, &server.uri(), false).await.code, 0);
        let cached = f.cache.join("remote-config/77.json");
        let file = std::fs::File::options().write(true).open(&cached).unwrap();
        file.set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 24 * 3600),
        )
        .unwrap();
        drop(file);
        let o = run_report(&f, "http://127.0.0.1:1", false).await;
        assert_eq!(o.code, 0);
        assert!(o.stdout.contains("older cached copy"));
        assert!(o.stdout.contains("saves  <?DOCUMENTS?>"));
        let o = run_report(&f, "http://127.0.0.1:1", true).await;
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["config_source"], "stale_cache");
    }

    const INSTALL_CONFIG: &str = r#"{"content":{"Windows":{"cloudStorage":{"enabled":true,
        "locations":[{"name":"game","location":"<?INSTALL?>/SAVE"}]}}}}"#;

    /// An installer-lane game in `f`'s prefix, `drive_c/GOG/G`.
    fn installer_game(f: &Fixture, log: bool) -> Game {
        let root = f.layout.prefix.parent().unwrap().to_path_buf();
        let gf = f.layout.prefix.join("drive_c/GOG/G");
        std::fs::create_dir_all(gf.join("SAVE")).unwrap();
        std::fs::write(
            root.join("content.toml"),
            "schema = 1\nlane = \"installer\"\nbuild_id = \"b1\"\n",
        )
        .unwrap();
        if log {
            std::fs::create_dir_all(root.join("install")).unwrap();
            std::fs::write(
                root.join("install/inno.log"),
                "Dest filename: C:\\GOG\\G\\Game.exe\r\n",
            )
            .unwrap();
        }
        Game {
            gogid: "5".into(),
            layout: Layout {
                lane: Lane::Linux,
                prefix: f.layout.prefix.clone(),
                game_files: gf,
            },
            root,
        }
    }

    #[tokio::test]
    async fn locations_name_the_build_file_list_or_skip_the_location() {
        let f = fixture();
        let server = serving(200, INSTALL_CONFIG).await;
        let uri = server.uri();
        let run = async |game: &Game, json: bool| {
            let req = Request {
                gogid: "5",
                client_id: "77",
                layout: &game.layout,
                game: Some(game),
                json,
            };
            report(&req, &f.cache, &uri, &reqwest::Client::new()).await
        };
        let g = installer_game(&f, true);
        let o = run(&g, false).await;
        assert!(
            o.stdout.contains(
                "in the game's files: the game's own files are left out \
                 (list: the installer's log, 1 listed)"
            ),
            "{}",
            o.stdout
        );
        let v: serde_json::Value = serde_json::from_str(&run(&g, true).await.stdout).unwrap();
        assert_eq!(
            v["locations"][0]["build_files"]["source"],
            "the installer's log"
        );

        std::fs::remove_file(g.root.join("install/inno.log")).unwrap();
        let o = run(&g, false).await;
        assert_eq!(o.code, 0);
        assert!(
            o.stdout
                .contains("warning: location \"game\" skipped: it is in the game's files"),
            "{}",
            o.stdout
        );
        let v: serde_json::Value = serde_json::from_str(&run(&g, true).await.stdout).unwrap();
        assert_eq!(v["skipped"][0]["reason"]["kind"], "build_files_unknown");
    }

    #[tokio::test]
    async fn the_build_list_is_found_fetched_or_refused() {
        use gogo::linuxapp::rollback::FetchWant;
        use gogo::snapshot::saves::{BuildFiles, Expect};
        let want = FetchWant {
            product_id: 5,
            build_id: "b1".into(),
            language: "en-US".into(),
            products: vec!["5".into()],
        };
        let mut one = BuildFiles::new("the build's depot manifests");
        one.insert("Game.exe", Expect::Size(1));
        let never = async |_: FetchWant| -> Option<BuildFiles> { panic!("no fetch") };

        let known = build_list(Lookup::Known(one.clone()), never).await;
        assert_eq!(known.unwrap(), one);
        let unknown = build_list(Lookup::Unknown("why".into()), never).await;
        assert_eq!(unknown.unwrap_err(), "why");

        let asked = std::cell::Cell::new(None);
        let fetched = build_list(Lookup::Fetch(want.clone()), async |w: FetchWant| {
            asked.set(Some(w.build_id));
            Some(one.clone())
        })
        .await;
        assert_eq!(fetched.unwrap(), one);
        assert_eq!(asked.take().as_deref(), Some("b1"));

        let failed = build_list(Lookup::Fetch(want.clone()), async |_| None).await;
        assert_eq!(failed.unwrap_err(), build_files::NOT_FETCHED);
        let empty = build_list(Lookup::Fetch(want), async |_| {
            Some(BuildFiles::new("the build's depot manifests"))
        })
        .await;
        assert_eq!(empty.unwrap_err(), build_files::NOT_FETCHED);
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(clean("a\u{1b}[2Jb\n"), "a\\u{1b}[2Jb\\n");
        assert_eq!(clean("Slot 1/sav.dat"), "Slot 1/sav.dat");
    }

    #[test]
    fn status_parses() {
        let c = cli::Cli::try_parse_from(["gogo", "saves", "status", "123"]).unwrap();
        let cli::Command::Saves {
            cmd: SavesCmd::Status { gogid, json },
        } = c.command
        else {
            panic!("expected saves status")
        };
        assert_eq!(gogid, "123");
        assert!(!json);
    }

    /// `saves status` against a mock remote config and storage, with a fake
    /// exchange: exit codes and both outputs.
    #[tokio::test]
    async fn status_against_mock_storage() {
        use gogo::cloud::auth::CloudAuth;
        use gogo::cloud::http::StorageClient;
        use gogo::cloud::locations::Game;
        use gogo::cloud::storage::{gzip, md5_hex};
        use gogo::cloud::testing::{FixedExchange, MainSession, NoManifests, keep_manifest};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const CLIENT: &str = "11111111111111111";
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root = base.join("games/5");
        let saves = root.join("prefix/drive_c/users/steamuser/Documents/G");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::create_dir_all(root.join("game")).unwrap();
        keep_manifest(&root, CLIENT);
        std::fs::write(saves.join("mine.sav"), b"local").unwrap();
        std::fs::write(saves.join("both.sav"), b"local").unwrap();
        let game = Game {
            gogid: "5".into(),
            layout: Layout {
                lane: Lane::Linux,
                prefix: root.join("prefix"),
                game_files: root.join("game"),
            },
            root: root.clone(),
        };

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/components/galaxy_client/clients/{CLIENT}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(CONFIG))
            .mount(&server)
            .await;
        let objects = [
            ("saves/theirs.sav", gzip(b"cloud")),
            ("saves/both.sav", gzip(b"cloud")),
        ];
        let listing: Vec<serde_json::Value> = objects
            .iter()
            .map(|(n, b)| serde_json::json!({"name": n, "bytes": b.len(), "hash": md5_hex(b)}))
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/v1/7/{CLIENT}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(listing))
            .mount(&server)
            .await;
        for (n, b) in &objects {
            Mock::given(method("GET"))
                .and(path(format!("/v1/7/{CLIENT}/{n}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b.clone()))
                .mount(&server)
                .await;
        }

        let session = MainSession::new();
        let exchange = FixedExchange::default();
        let auth = CloudAuth::with_parts(&session, || Ok(()), NoManifests, &exchange);
        let storage =
            StorageClient::with_base(format!("{}/v1/", server.uri()).parse().unwrap()).unwrap();
        let run = || async {
            cloud_status::status(&cloud_status::Inputs {
                game: &game,
                auth: &auth,
                storage: &storage,
                http: &reqwest::Client::new(),
                config_base: &server.uri(),
                cache_dir: &base.join("cache"),
                record_dir: &base.join("records"),
            })
            .await
        };

        let text = status_outcome(run().await, false);
        assert_eq!(text.code, cli::exit::CONFLICTS, "{}", text.stdout);
        assert!(text.stdout.contains("first sync"), "{}", text.stdout);
        assert!(
            text.stdout.contains("download        theirs.sav"),
            "{}",
            text.stdout
        );
        assert!(
            text.stdout.contains("upload          mine.sav"),
            "{}",
            text.stdout
        );
        assert!(
            text.stdout
                .contains("conflict        both.sav (on both sides with different content)"),
            "{}",
            text.stdout
        );
        assert!(
            text.stdout
                .contains("plan: 1 download, 1 upload, 1 conflict"),
            "{}",
            text.stdout
        );

        let json = status_outcome(run().await, true);
        assert_eq!(json.code, cli::exit::CONFLICTS);
        let v: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
        assert_eq!(v["first_sync"], true);
        assert_eq!(v["counts"]["download"], 1);
        assert_eq!(v["locations"][0]["files"][0]["action"], "conflict");
        assert_eq!(v["locations"][0]["files"][0]["reason"], "both_new_differ");
        for secret in [CLIENT, "made-up-game-token", gogo::cloud::testing::SECRET] {
            assert!(!json.stdout.contains(secret), "{secret}");
        }
        assert!(!base.join("records").exists(), "status writes no record");

        // The game's own files in a location in the game's files.
        let mut plan = run().await.unwrap();
        plan.locations[0].local_build_files = 3;
        plan.locations[0].build_files = Some(gogo::cloud::locations::BuildNote {
            source: "the depot cache".into(),
            files: Some(9),
        });
        plan.unmapped.push(cloud_status::Unmapped {
            name: "saves/Game.exe".into(),
            reason: UnmappedReason::BuildFile,
        });
        let t = render_status(&plan);
        assert!(
            t.contains("  left out as the game's own files: 3 (list: the depot cache)"),
            "{t}"
        );
        assert!(
            t.contains("note: 1 cloud objects have the path of one of the game's own files"),
            "{t}"
        );

        // Exit precedence: a corrupt object (5) over an unreadable file (1)
        // over conflicts (8).
        let mut plan = run().await.unwrap();
        plan.counts.insert("unreadable".into(), 1);
        assert_eq!(
            status_outcome(Ok(plan.clone()), false).code,
            cli::exit::FAILED
        );
        plan.counts.insert("corrupt".into(), 1);
        assert_eq!(status_outcome(Ok(plan), false).code, cli::exit::CORRUPT);

        // A broken or newer record stops the run, naming the file.
        std::fs::create_dir_all(base.join("records")).unwrap();
        for body in ["schema = ", "schema = 2\nclient_id = \"1\"\n"] {
            std::fs::write(base.join("records/5.toml"), body).unwrap();
            let o = status_outcome(run().await, false);
            assert_eq!(o.code, cli::exit::FAILED, "{body}");
            assert!(o.stderr.contains("5.toml"), "{}", o.stderr);
            assert!(o.stdout.is_empty());
        }

        // An error maps through the one table.
        let e = status_outcome(Err(CloudError::NoLicense { gogid: 5 }), false);
        assert_eq!(e.code, cli::exit::AUTH);
        assert!(e.stderr.starts_with("gogo: "));
    }

    #[test]
    fn pull_parses_and_prefers_only_the_cloud() {
        let c = cli::Cli::try_parse_from([
            "gogo",
            "saves",
            "pull",
            "123",
            "--prefer",
            "cloud",
            "--dry-run",
            "--yes",
            "--json",
            "--no-snapshots",
        ])
        .unwrap();
        let cli::Command::Saves {
            cmd: SavesCmd::Pull(a),
        } = c.command
        else {
            panic!("expected saves pull")
        };
        assert_eq!(a.gogid, "123");
        assert_eq!(a.prefer, Some(Prefer::Cloud));
        assert!(a.dry_run && a.yes && a.json && a.no_snapshots);
        for side in ["local", "newer"] {
            assert!(
                cli::Cli::try_parse_from(["gogo", "saves", "pull", "1", "--prefer", side]).is_err(),
                "{side}"
            );
        }
    }

    /// `saves pull` headless against mock storage: an add-only plan runs, an
    /// overwrite needs `--yes` (exit 2, as other commands), a conflict left
    /// is exit 8, `--prefer cloud` settles it; nothing but GETs reach the
    /// storage and the output holds no secret.
    #[tokio::test]
    async fn pull_against_mock_storage() {
        use gogo::cloud::auth::CloudAuth;
        use gogo::cloud::http::StorageClient;
        use gogo::cloud::pull::{self as cloud_pull, Host, PullOptions};
        use gogo::cloud::storage::{gzip, md5_hex};
        use gogo::cloud::testing::{FixedExchange, MainSession, NoManifests, keep_manifest};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        struct NoHost;
        impl Host for NoHost {
            fn lock(&mut self) -> Result<(), CloudError> {
                Ok(())
            }
            fn snapshot(&mut self) -> Result<Option<String>, CloudError> {
                Ok(None)
            }
        }

        const CLIENT: &str = "11111111111111111";
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root = base.join("games/5");
        let saves = root.join("prefix/drive_c/users/steamuser/Documents/G");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::create_dir_all(root.join("game")).unwrap();
        keep_manifest(&root, CLIENT);
        std::fs::write(saves.join("both.sav"), b"local").unwrap();
        let game = Game {
            gogid: "5".into(),
            layout: Layout {
                lane: Lane::Linux,
                prefix: root.join("prefix"),
                game_files: root.join("game"),
            },
            root: root.clone(),
        };
        let server = MockServer::start().await;
        let objects = [
            ("saves/theirs.sav", gzip(b"cloud")),
            ("saves/both.sav", gzip(b"cloud")),
        ];
        let listing: Vec<serde_json::Value> = objects
            .iter()
            .map(|(n, b)| serde_json::json!({"name": n, "bytes": b.len(), "hash": md5_hex(b)}))
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/v1/7/{CLIENT}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(listing))
            .mount(&server)
            .await;
        for (n, b) in &objects {
            Mock::given(method("GET"))
                .and(path(format!("/v1/7/{CLIENT}/{n}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b.clone()))
                .mount(&server)
                .await;
        }
        let cfg: gogo::cloud::remote_config::RemoteConfig = serde_json::from_str(CONFIG).unwrap();
        let resolved = resolve(&cfg, CLIENT, &game.layout);

        let session = MainSession::new();
        let exchange = FixedExchange::default();
        let auth = CloudAuth::with_parts(&session, || Ok(()), NoManifests, &exchange);
        let storage =
            StorageClient::with_base(format!("{}/v1/", server.uri()).parse().unwrap()).unwrap();
        let records = base.join("records");
        let run = |prefer_cloud: bool, yes: bool, dry_run: bool| {
            let (auth, storage, game, resolved, records) =
                (&auth, &storage, &game, &resolved, &records);
            async move {
                cloud_pull::pull(
                    &cloud_pull::Inputs {
                        game,
                        client_id: CLIENT,
                        resolved,
                        source: Source::Network,
                        auth,
                        storage,
                        record_dir: records,
                    },
                    &PullOptions {
                        dry_run,
                        keep_backups: 20,
                        now: chrono::Utc::now(),
                        stop_before_rename: None,
                    },
                    &mut NoHost,
                    &mut CliDecide {
                        prefer_cloud,
                        yes,
                        interactive: false,
                    },
                )
                .await
            }
        };

        // No TTY, no --prefer: the add is done, the conflict left (8).
        let o = pull_outcome(run(false, false, false).await, false);
        assert_eq!(o.code, cli::exit::CONFLICTS, "{}{}", o.stdout, o.stderr);
        assert!(
            o.stdout.contains("  download      saves/theirs.sav"),
            "{}",
            o.stdout
        );
        assert!(
            o.stdout.contains(
                "  left          saves/both.sav (on both sides with different content: needs a decision)"
            ),
            "{}",
            o.stdout
        );
        assert_eq!(std::fs::read(saves.join("theirs.sav")).unwrap(), b"cloud");
        assert_eq!(std::fs::read(saves.join("both.sav")).unwrap(), b"local");

        // A dry run with --prefer cloud plans the overwrite and changes nothing.
        let o = pull_outcome(run(true, false, true).await, false);
        assert_eq!(o.code, cli::exit::OK, "{}", o.stdout);
        assert!(o.stdout.starts_with("dry run"), "{}", o.stdout);
        assert!(
            o.stdout.contains("  overwrite     saves/both.sav"),
            "{}",
            o.stdout
        );
        assert!(o.stdout.contains("would: 1 overwrite"), "{}", o.stdout);

        // --prefer cloud without --yes: consent can't be asked (exit 2).
        let o = pull_outcome(run(true, false, false).await, false);
        assert_eq!(o.code, cli::exit::USAGE);
        assert!(o.stderr.contains("pass --yes"), "{}", o.stderr);
        assert!(
            o.stdout
                .contains("  not done      saves/both.sav (would overwrite)"),
            "{}",
            o.stdout
        );
        assert_eq!(std::fs::read(saves.join("both.sav")).unwrap(), b"local");

        // With --yes: done, backed up.
        let o = pull_outcome(run(true, true, false).await, true);
        assert_eq!(o.code, cli::exit::OK, "{}", o.stderr);
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["files"][0]["name"], "saves/both.sav");
        assert_eq!(v["files"][0]["op"], "overwrite");
        assert_eq!(v["files"][0]["state"], "done");
        let backup = std::path::PathBuf::from(v["backup_dir"].as_str().unwrap());
        assert_eq!(
            std::fs::read(backup.join("local/saves/both.sav")).unwrap(),
            b"local"
        );
        assert_eq!(std::fs::read(saves.join("both.sav")).unwrap(), b"cloud");
        for secret in [CLIENT, "made-up-game-token", gogo::cloud::testing::SECRET] {
            assert!(!o.stdout.contains(secret), "{secret}");
        }
        for r in server.received_requests().await.unwrap() {
            assert_eq!(r.method.as_str(), "GET");
        }

        // Errors map through the one table.
        let e = pull_outcome(Err(CloudError::Refused("x".into())), false);
        assert_eq!(e.code, 7);
    }
}

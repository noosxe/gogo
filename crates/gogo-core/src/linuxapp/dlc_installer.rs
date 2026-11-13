//! DLC on the offline-installer lane (design 19 §5, #351): each DLC's own Inno
//! Setup installer, run in the game's Wine prefix with `/DIR=` the base game's dir.
//!
//! - A first build runs them in the staging prefix after the base installer, one
//!   at a time; a failed one fails the build before the prefix moves
//!   ([`run_one`], from `build::execute`).
//! - `dlc add` runs them in the real prefix under the game's lock ([`add`]): the
//!   `auto` snapshot first, then `dlc-journal.json` with `lane = "installer"` (the
//!   launcher refuses and doctor fails while it is there), the installers, the
//!   commit (`content.toml`), the journal deleted. Inno has no rollback gogo can
//!   drive: a stopped add is finished by running the add again, which runs every
//!   DLC the journal names again (Inno overwrites what it wrote).
//! - A DLC is installed when its installer exits 0 and its `goggame-<id>.info` is
//!   in the base game's dir. An info elsewhere in the prefix means the installer
//!   ignored `/DIR=` (design 19 §11 Q5): an error naming where it went.
//!
//! The network side (which DLC, their installers' download, the version check's
//! catalog half) is the caller's.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engine::events::{ProgressEvent, step, working};
use crate::gog::content::{self, Content, DlcRow};
use crate::gog::lay::{self, AddingDlc, Journal};
use crate::macapp::error::MacAppError;
use crate::macapp::{info, installer};

use super::dlc::{AddError, AddOutcome, Game};
use super::game;
use super::setup::Umu;

/// The journal's `lane` for an add on this lane.
pub const LANE: &str = "installer";

/// One DLC's offline installer, downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallerDlc {
    pub id: u64,
    pub title: String,
    /// The setup `.exe` (its `.bin` parts beside it).
    pub setup: PathBuf,
    /// The catalog's version label of the installer.
    pub version: Option<String>,
}

/// A first build's DLC on this lane (design 19 §5): chosen before the plan, their
/// installers downloaded after the consent and before the build runs.
#[derive(Debug, Clone, Default)]
pub struct BuildDlc {
    /// The game the setup is for, when gogo could tell: the game the base
    /// installer installs must be it before any DLC installer runs.
    pub gogid: Option<String>,
    /// The base installer's version label (GOG's), when known.
    pub version: Option<String>,
    /// The base installer's language, which its DLC's follow.
    pub language: Option<String>,
    /// The chosen DLC, titled, for the plan.
    pub refs: Vec<content::DlcRef>,
    /// Their installers, once downloaded.
    pub installers: Vec<InstallerDlc>,
}

/// What a DLC's installer left in the game's dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub id: u64,
    /// The `buildId` of its `goggame-<id>.info`, when it has one.
    pub build_id: Option<String>,
}

/// The Inno flags of the installer lane (as on the Mac, design 10 §2.4): silent,
/// into `win_target`, its log written through `z:`.
pub fn inno_args(setup: &Path, win_target: &str, log: &Path) -> Vec<String> {
    vec![
        setup.display().to_string(),
        "/VERYSILENT".into(),
        "/SUPPRESSMSGBOXES".into(),
        "/NORESTART".into(),
        "/SP-".into(),
        format!("/DIR={win_target}"),
        format!("/LOG={}", installer::z_path(log)),
    ]
}

/// The Inno log of DLC `id`'s installer, beside the base's in the install logs.
pub fn inno_log_name(id: u64) -> String {
    format!("dlc-{id}-inno.log")
}

/// `goggame-<id>.info` anywhere under `drive_c`, but Windows' own dir: where a
/// DLC installer that ignored `/DIR=` put its files. Bounded: five levels.
pub fn find_info(drive_c: &Path, id: u64) -> Option<PathBuf> {
    let name = format!("goggame-{id}.info");
    let mut level = vec![drive_c.to_path_buf()];
    for _ in 0..5 {
        let mut next = Vec::new();
        for dir in level {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = rd.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let Ok(ft) = e.file_type() else { continue };
                let file = e.file_name();
                if ft.is_file() && file.to_string_lossy().eq_ignore_ascii_case(&name) {
                    return Some(e.path());
                }
                if ft.is_dir() && !(dir == drive_c && file.eq_ignore_ascii_case("windows")) {
                    next.push(e.path());
                }
            }
        }
        level = next;
    }
    None
}

/// The DLC's info in `install_dir` (any case), else `None`.
fn info_in(install_dir: &Path, id: u64) -> Option<PathBuf> {
    let name = format!("goggame-{id}.info");
    std::fs::read_dir(install_dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(&name))
        .map(|e| e.path())
}

/// Run DLC `d`'s installer through `umu` into `win_target` (`C:\GOG\<name>`,
/// `install_dir` on the host), its Inno log in `logs`; check it landed there.
pub fn run_one(
    umu: &Umu,
    d: &InstallerDlc,
    win_target: &str,
    install_dir: &Path,
    logs: &Path,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Installed, MacAppError> {
    std::fs::create_dir_all(logs)?;
    let log_name = inno_log_name(d.id);
    let log = logs.join(&log_name);
    let status = working(emit, step::DLC_INSTALLER, || {
        umu.run(
            "a DLC's installer",
            umu.command(inno_args(&d.setup, win_target, &log)),
        )
    })?;
    if !status.success() {
        return Err(MacAppError::DlcInstallerFailed {
            id: d.id,
            title: d.title.clone(),
            exit: status.code().unwrap_or(-1),
            log: log_name,
            logs: logs.to_path_buf(),
            tail: installer::log_tail(&log, 8),
        });
    }
    let Some(found) = info_in(install_dir, d.id) else {
        return Err(MacAppError::DlcNotInGameDir {
            id: d.id,
            title: d.title.clone(),
            dir: install_dir.to_path_buf(),
            found: find_info(&umu.prefix.join("drive_c"), d.id),
        });
    };
    let build_id = std::fs::read_to_string(&found)
        .ok()
        .and_then(|t| serde_json::from_str::<info::GogGameInfo>(&t).ok())
        .and_then(|i| i.build_id_text());
    emit(&ProgressEvent::DlcInstalled { id: d.id });
    Ok(Installed { id: d.id, build_id })
}

// — the version check (design 19 §5, decision 5) —

/// How the base game's installer compares with the one GOG serves now, whose
/// DLC installers GOG serves beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCheck {
    /// The same label: the DLC installers are for this base.
    Same,
    /// The base is another version than GOG's current installer.
    Differs { installed: String, current: String },
    /// Either side has no label: nothing to compare.
    Unknown,
}

/// Compare the base game's installer version (`content.toml`'s `version` on this
/// lane) with GOG's current base installer's. Only the same product's labels are
/// compared: a DLC's own label can differ from its base's (`1.0.10` beside
/// `1.0.10b`) and says nothing.
pub fn version_check(installed: Option<&str>, current: Option<&str>) -> VersionCheck {
    match (
        installed.map(str::trim).filter(|s| !s.is_empty()),
        current.map(str::trim).filter(|s| !s.is_empty()),
    ) {
        (Some(i), Some(c)) if i == c => VersionCheck::Same,
        (Some(i), Some(c)) => VersionCheck::Differs {
            installed: i.to_string(),
            current: c.to_string(),
        },
        _ => VersionCheck::Unknown,
    }
}

/// The refusal for [`VersionCheck::Differs`], naming the way out.
pub fn version_refusal(gogid: &str, installed: &str, current: &str) -> String {
    format!(
        "the game was installed from version {installed} of its installer, and GOG now \
         serves {current}, with DLC installers for {current} only; download the new \
         installer (`gogo download {gogid}`), then rebuild with the DLC \
         (`gogo linux-app {gogid} --dlc <id|title>`)"
    )
}

/// The plan's line for [`VersionCheck::Unknown`].
pub fn version_unknown_note(current: Option<&str>) -> String {
    match current {
        Some(c) => format!(
            "the base game's installer version isn't known, so it can't be checked against \
             GOG's {c}, the version its DLC installers are for"
        ),
        None => "GOG lists no version for the game's installer: the DLC's can't be checked \
                 against it"
            .into(),
    }
}

/// The version of a GOG setup known from its file name alone: `current` (GOG's
/// label for its installer now) when the name has it as a whole `_`-separated
/// part, else `None`. A name's own version isn't GOG's label: the installer GOG
/// labels `1.0.10b` is `setup_<slug>_1.0.10_(44657).exe`, so only a match counts.
pub fn setup_version(file_name: &str, current: Option<&str>) -> Option<String> {
    let c = current.filter(|c| !c.is_empty())?;
    let stem = file_name
        .strip_suffix(".exe")
        .or_else(|| file_name.strip_suffix(".EXE"))
        .unwrap_or(file_name)
        .to_ascii_lowercase();
    let parts: Vec<&str> = stem.split('_').collect();
    let lower = c.to_ascii_lowercase();
    let want: Vec<&str> = lower.split('_').collect();
    parts
        .windows(want.len())
        .any(|w| w == want.as_slice())
        .then(|| c.to_string())
}

/// The owned game a setup file's name names (`setup_<slug>_…`): the longest slug
/// of `games` the name starts with, followed by `_` or the end.
pub fn game_for_setup(file_name: &str, games: &[(u64, String)]) -> Option<u64> {
    let lower = file_name.to_ascii_lowercase();
    let rest = lower.strip_prefix("setup_")?;
    let rest = rest.strip_suffix(".exe").unwrap_or(rest);
    games
        .iter()
        .filter(|(_, slug)| {
            let s = slug.to_ascii_lowercase();
            !s.is_empty()
                && rest
                    .strip_prefix(&s)
                    .is_some_and(|r| r.is_empty() || r.starts_with('_'))
        })
        .max_by_key(|(_, slug)| slug.len())
        .map(|(id, _)| *id)
}

// — content.toml (design 19 §6.1) —

/// Record what an installer run installed: each DLC's row gets its installer's
/// version label and its info's build, and keeps its `added` date.
pub fn record(content: &mut Content, dlcs: &[InstallerDlc], installed: &[Installed]) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    for d in dlcs {
        let build_id = installed
            .iter()
            .find(|i| i.id == d.id)
            .and_then(|i| i.build_id.clone());
        match content.dlc.iter_mut().find(|r| r.id == d.id) {
            Some(r) => {
                r.version = d.version.clone().or(r.version.take());
                r.build_id = build_id.or(r.build_id.take());
            }
            None => content.dlc.push(DlcRow {
                id: d.id,
                title: d.title.clone(),
                build_id,
                version: d.version.clone(),
                added: Some(today.clone()),
            }),
        }
    }
}

// — dlc add (design 19 §5 "Adding later") —

/// What an add runs.
#[derive(Debug, Clone, Default)]
pub struct AddPlan {
    /// Every DLC to install, in order: an interrupted add's first.
    pub dlcs: Vec<InstallerDlc>,
    /// The language the game records, when it records none yet.
    pub language: Option<String>,
}

/// How to run an add.
#[derive(Debug, Clone)]
pub struct AddOptions {
    pub snapshots: super::snapshots::AutoOptions,
    /// Run setup after the commit. Tests turn it off.
    pub run_setup: bool,
    /// Crash injection: stop after this DLC's installer, before the commit.
    #[doc(hidden)]
    pub stop_after: Option<u64>,
}

impl Default for AddOptions {
    fn default() -> Self {
        Self {
            snapshots: super::snapshots::AutoOptions::default(),
            run_setup: true,
            stop_after: None,
        }
    }
}

/// The DLC an interrupted add of `root`'s game still has to install: its
/// journal's, when it is this lane's.
pub fn pending(root: &Path) -> Vec<AddingDlc> {
    lay::read_journal(root)
        .ok()
        .flatten()
        .filter(|j| j.lane == LANE)
        .map(|j| j.adding)
        .unwrap_or_default()
}

/// The base game's dir in the prefix: `C:\GOG\<name>` of `gogo-launch.toml`'s exe.
pub fn install_dir(game: &Game) -> Result<(String, PathBuf), MacAppError> {
    let (baked, _) = game::read_launch_toml(&game.dirs)?;
    let name = baked.tree_name().ok_or_else(|| {
        MacAppError::Apps(format!(
            "{}'s exe ({}) isn't under C:\\GOG\\<name>: gogo can't tell where its DLC go",
            game.gogid, baked.exe
        ))
    })?;
    Ok((
        format!("C:\\GOG\\{name}"),
        game.dirs.prefix.join("drive_c/GOG").join(name),
    ))
}

fn journal(game: &Game, adding: &[AddingDlc], source: &Path) -> Journal {
    Journal {
        gogid: game.gogid.clone(),
        lane: LANE.into(),
        build_id: game.content.build_id.clone().unwrap_or_default(),
        language: game.content.language.clone().unwrap_or_default(),
        adding: adding.to_vec(),
        source: source.to_path_buf(),
        source_is_cache: false,
        moves: false,
        files: Vec::new(),
        replaced: Vec::new(),
        dirs: Vec::new(),
        kind: None,
        from: None,
        to: None,
        removed: Vec::new(),
        removing: Vec::new(),
        md5s: Default::default(),
        prune: Vec::new(),
    }
}

/// What gives an add its umu: [`super::setup::game_runner`] in production.
pub type Runner<'a> =
    dyn FnMut(&mut dyn FnMut(&ProgressEvent)) -> Result<super::setup::GameRunner, MacAppError> + 'a;

/// Run `plan`'s DLC installers in `game`'s prefix (design 19 §5). `runner` gives
/// the umu to run them with (the game's Proton build and id: [`super::setup::game_runner`]
/// in production). `stop` says the user asked to stop.
pub fn add(
    game: &mut Game,
    plan: &AddPlan,
    runner: &mut Runner,
    opts: &AddOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
    stop: &dyn Fn() -> bool,
) -> Result<AddOutcome, AddError> {
    let root = game.dirs.root.clone();
    for message in
        super::rollback::settle_pending(&game.gogid, &super::snapshots::Backends::real())?
    {
        emit(&ProgressEvent::Warn { message });
    }
    let lock = crate::macapp::relocate::lock_game(&game.gogid)?;
    game::refuse_running(&game.gogid, &root, &game.dirs.prefix)?;
    // The dir as it is now, under the lock.
    *game = game.reload()?;
    let prior = lay::read_journal(&root)?;
    if let Some(j) = &prior
        && j.lane != LANE
    {
        return Err(AddError::Game(MacAppError::Apps(format!(
            "{} has a depot-lane journal ({}) on an offline-installer game; it can't be \
             settled here",
            game.gogid,
            lay::journal_path(&root).display()
        ))));
    }
    let resumed = prior.is_some();
    let have = game.installed();
    let mut adding: Vec<AddingDlc> = prior.map(|j| j.adding).unwrap_or_default();
    for d in &plan.dlcs {
        if !adding.iter().any(|a| a.id == d.id) && !have.contains(&d.id) {
            adding.push(AddingDlc {
                id: d.id,
                title: d.title.clone(),
            });
        }
    }
    let mut runs: Vec<InstallerDlc> = Vec::new();
    for a in &adding {
        let Some(d) = plan.dlcs.iter().find(|d| d.id == a.id) else {
            return Err(AddError::Game(MacAppError::Apps(format!(
                "an earlier add of DLC \"{}\" ({}) stopped, and its installer isn't in this \
                 add's plan",
                a.title, a.id
            ))));
        };
        runs.push(d.clone());
    }
    let mut warnings = Vec::new();
    if adding.is_empty() {
        return Ok(outcome(game, &[], None, None, warnings));
    }
    let (win_target, install_dir) = install_dir(game)?;
    // The `auto` snapshot (design 20 §7.1, §9.4): after the refusals, before the
    // journal. A rerun after a stop took it on its first try, and no snapshot
    // operation runs while a journal is pending (§5.1).
    let mut taken = None;
    if !resumed {
        let titles: Vec<&str> = adding.iter().map(|a| a.title.as_str()).collect();
        taken = super::snapshots::protect(
            &game.gogid,
            &game.dirs,
            Some(&lock),
            crate::snapshot::Trigger::DlcAdd,
            crate::snapshot::labels::adding(&titles),
            &opts.snapshots,
            emit,
        )?;
    }
    let source = runs[0]
        .setup
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    lay::write_json_durably(&lay::journal_path(&root), &journal(game, &adding, &source))?;
    if resumed {
        emit(&ProgressEvent::Warn {
            message: format!(
                "an earlier DLC add stopped while its installers ran: running {} again",
                adding
                    .iter()
                    .map(|a| a.title.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }

    let r = runner(emit)?;
    let logs = root.join(crate::macapp::INSTALL_LOGS);
    let umu = Umu {
        run: &r.umu_run,
        proton: &r.build.dir,
        prefix: &game.dirs.prefix,
        umu_id: &r.umu_id,
        cwd: &root,
        log: logs.join(installer::WINE_LOG),
    };
    umu.check()?;
    let mut installed = Vec::new();
    for d in &runs {
        if stop() {
            return Err(AddError::Interrupted(
                "DLC installers",
                rerun_note(&game.gogid),
            ));
        }
        match run_one(&umu, d, &win_target, &install_dir, &logs, emit) {
            Ok(i) => installed.push(i),
            Err(_) if stop() => {
                return Err(AddError::Interrupted(
                    "DLC installers",
                    rerun_note(&game.gogid),
                ));
            }
            Err(e) => return Err(e.into()),
        }
        if opts.stop_after == Some(d.id) {
            return Err(AddError::Interrupted(
                "DLC installers",
                rerun_note(&game.gogid),
            ));
        }
    }

    // The commit: `content.toml`, then the journal goes.
    let mut c = game.content.clone();
    if c.language.is_none() {
        c.language = plan.language.clone();
    }
    record(&mut c, &runs, &installed);
    content::write(&root, &c)?;
    game.content = c;
    game.derived = false;
    lay::finish(&root)?;

    if opts.run_setup
        && let Err(e) = super::setup::setup(
            &game.gogid,
            &super::setup::SetupOptions {
                refresh: false,
                prompt: None,
                runtime_snapshot: None,
            },
            emit,
        )
    {
        warnings.push(format!(
            "the DLC is installed, but setup failed: {e}; run `gogo linux-app setup {}`",
            game.gogid
        ));
    }
    Ok(outcome(game, &adding, taken, Some(resumed), warnings))
}

fn rerun_note(gogid: &str) -> String {
    format!(
        "the game won't start until `gogo linux-app dlc {gogid} add` runs the DLC's \
         installers again"
    )
}

fn outcome(
    game: &Game,
    adding: &[AddingDlc],
    taken: Option<super::snapshots::Taken>,
    resumed: Option<bool>,
    warnings: Vec<String>,
) -> AddOutcome {
    AddOutcome {
        gogid: game.gogid.clone(),
        build_id: game.content.build_id.clone(),
        language: game.content.language.clone(),
        dlcs: game.content.refs(),
        added: adding
            .iter()
            .map(|a| content::DlcRef {
                id: a.id,
                title: a.title.clone(),
            })
            .collect(),
        recovered: resumed.filter(|r| *r).map(|_| lay::Direction::Forward),
        lay: None,
        snapshot: taken.as_ref().map(super::snapshots::Taken::snapshot),
        pruned: taken
            .as_ref()
            .map(super::snapshots::Taken::pruned)
            .unwrap_or_default(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inno_args_name_the_dir_and_log() {
        let a = inno_args(
            Path::new("/dl/setup_x_dlc.exe"),
            r"C:\GOG\x",
            Path::new("/g/install-logs/dlc-1-inno.log"),
        );
        assert_eq!(a[0], "/dl/setup_x_dlc.exe");
        assert!(a.contains(&r"/DIR=C:\GOG\x".to_string()));
        assert!(a.contains(&"/VERYSILENT".to_string()));
        assert!(a.iter().any(|x| x.starts_with("/LOG=Z:")), "{a:?}");
    }

    #[test]
    fn versions_compare_one_product_only() {
        assert_eq!(
            version_check(Some("1.0.10b"), Some("1.0.10b")),
            VersionCheck::Same
        );
        assert_eq!(
            version_check(Some("2.31a"), Some("2.5_RedModFix")),
            VersionCheck::Differs {
                installed: "2.31a".into(),
                current: "2.5_RedModFix".into()
            }
        );
        assert_eq!(version_check(None, Some("1.0")), VersionCheck::Unknown);
        assert_eq!(version_check(Some(" "), Some("1.0")), VersionCheck::Unknown);
        assert_eq!(version_check(Some("1.0"), None), VersionCheck::Unknown);
    }

    #[test]
    fn a_setup_name_only_confirms_the_current_label() {
        let n = "setup_journey_to_the_savage_planet_1.0.10b_(64bit)_(57689).exe";
        assert_eq!(
            setup_version(n, Some("1.0.10b")).as_deref(),
            Some("1.0.10b")
        );
        // GOG labels this file 1.0.10b: a name's own version proves nothing.
        let real = "setup_journey_to_the_savage_planet_1.0.10_(44657).exe";
        assert_eq!(setup_version(real, Some("1.0.10b")), None);
        assert_eq!(
            setup_version(
                "setup_cyberpunk_2077_2.5_RedModFix_(1).exe",
                Some("2.5_RedModFix")
            )
            .as_deref(),
            Some("2.5_RedModFix")
        );
        assert_eq!(setup_version("setup_x_1.0.exe", None), None);
    }

    #[test]
    fn a_setup_names_its_game_by_the_longest_slug() {
        let games = vec![
            (1, "journey".to_string()),
            (2, "journey_to_the_savage_planet".to_string()),
            (3, "planet".to_string()),
        ];
        assert_eq!(
            game_for_setup("setup_journey_to_the_savage_planet_1.0.10b_(1).exe", &games),
            Some(2)
        );
        assert_eq!(game_for_setup("setup_journey_2.0.exe", &games), Some(1));
        assert_eq!(game_for_setup("setup_journeyman_1.exe", &games), None);
        assert_eq!(game_for_setup("journey_1.exe", &games), None);
    }

    #[test]
    fn an_info_outside_the_dir_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let c = tmp.path().join("drive_c");
        std::fs::create_dir_all(c.join("windows/system32")).unwrap();
        std::fs::write(c.join("windows/system32/goggame-7.info"), "{}").unwrap();
        assert_eq!(find_info(&c, 7), None, "Windows' own dir isn't searched");
        let other = c.join("GOG Games/The DLC");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("goggame-7.info"), "{}").unwrap();
        assert_eq!(find_info(&c, 7), Some(other.join("goggame-7.info")));
        assert_eq!(find_info(&c, 8), None);
    }

    #[test]
    fn record_keeps_rows_and_adds_versions() {
        let mut c = Content {
            schema: content::SCHEMA,
            lane: content::Lane::Installer,
            build_id: Some("b".into()),
            version: Some("1.0".into()),
            branch: None,
            language: Some("en".into()),
            dlc: vec![DlcRow {
                id: 1,
                title: "One".into(),
                build_id: None,
                version: None,
                added: Some("2026-01-01".into()),
            }],
        };
        let dlc = |id: u64| InstallerDlc {
            id,
            title: format!("D{id}"),
            setup: PathBuf::from("/x.exe"),
            version: Some("1.0".into()),
        };
        record(
            &mut c,
            &[dlc(1), dlc(2)],
            &[Installed {
                id: 2,
                build_id: Some("9".into()),
            }],
        );
        assert_eq!(c.dlc.len(), 2);
        assert_eq!(c.dlc[0].added.as_deref(), Some("2026-01-01"));
        assert_eq!(c.dlc[0].version.as_deref(), Some("1.0"));
        assert_eq!(c.dlc[1].build_id.as_deref(), Some("9"));
        assert_eq!(c.dlc[1].title, "D2");
    }
}

//! What a Linux build or set-up would do, worked out **without writing
//! anything**, for a frontend that asks every question and shows its plan before
//! the user confirms (design 25 §4).
//!
//! [`super::build::plan`] and [`super::setup::setup`] ask as they go and store each
//! answer at once. A preview asks the same questions in the same order, keeps the
//! answers in [`Answers`], and stores nothing: [`Answers::store`] and
//! [`Answers::replay`] hand them to the pipeline after the confirm.
//!
//! It reads the config, the game's files and the runtime store, runs
//! `python3 --version`, and asks GitHub for the size of anything it would
//! download. It creates no file or dir.

use std::path::{Path, PathBuf};

use super::build;
use super::comet;
use super::galaxy_switch;
use super::game::{self, LinuxState, refuse_private_tmp};
use super::proton::{self, ProtonRef};
use super::setup;
use super::umu::{self, Host};
use crate::config::{Config, GalaxyDefault, ProtonDir, UmuMode};
use crate::macapp::catalog::{self, CatalogEntry};
use crate::macapp::compat::{self, GalaxyMode, Layer, Os};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;
use crate::macapp::prompt::{PromptAnswer, PromptKind, PromptSpec};
use crate::macapp::renderer::RendererChoice;
use crate::macapp::{lutris, pe, store, verbs};

/// What a preview reads from the machine and the network.
pub struct Env<'a> {
    /// The config file that holds the asked-once answers.
    pub config: PathBuf,
    pub host: Host<'a>,
    /// GitHub's API, for download sizes.
    pub api: String,
    pub http: reqwest::Client,
}

impl Env<'static> {
    pub fn real() -> Result<Self, MacAppError> {
        Ok(Env {
            config: Config::path(),
            host: Host::real(),
            api: catalog::GITHUB_API.to_string(),
            http: catalog::http_client()?,
        })
    }
}

/// Which question a preview is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// `[linux] umu`.
    Umu,
    /// `[linux] proton_dir`; what each of the question's items means.
    ProtonDir(Vec<ProtonDir>),
    /// Whether Comet runs beside the game (`galaxy_default = "ask"`).
    Galaxy,
    /// Whether to install Comet, which that answer needs.
    InstallComet,
}

/// A question, in the words the CLI asks it.
#[derive(Debug, Clone)]
pub struct Question {
    pub ask: Ask,
    pub spec: PromptSpec,
}

/// A preview's result.
#[derive(Debug, Clone)]
pub enum Preview<T> {
    /// Answer this with [`Answers::answer`] and preview again.
    Ask(Question),
    Ready(T),
}

/// The answers collected so far. Kept in memory until the user confirms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answers {
    pub umu: Option<UmuMode>,
    pub proton_dir: Option<ProtonDir>,
    pub galaxy: Option<GalaxyMode>,
    pub install_comet: Option<bool>,
}

impl Answers {
    /// Take the answer to `question`. `false`: it isn't an answer to it.
    pub fn answer(&mut self, question: &Question, answer: &PromptAnswer) -> bool {
        match (&question.ask, answer) {
            (Ask::Umu, PromptAnswer::Index(i @ (0 | 1))) => {
                self.umu = Some(if *i == 0 {
                    UmuMode::Zipapp
                } else {
                    UmuMode::System
                });
            }
            (Ask::ProtonDir(dirs), PromptAnswer::Index(i)) => {
                let Some(dir) = dirs.get(*i) else {
                    return false;
                };
                self.proton_dir = Some(*dir);
            }
            (Ask::Galaxy, PromptAnswer::Index(i @ (0 | 1))) => {
                self.galaxy = Some(if *i == 0 {
                    GalaxyMode::Comet
                } else {
                    GalaxyMode::Off
                });
            }
            (Ask::InstallComet, PromptAnswer::Confirmed(yes)) => self.install_comet = Some(*yes),
            _ => return false,
        }
        true
    }

    /// Store the `[linux] umu` and `proton_dir` answers, so the pipeline doesn't
    /// ask them. Only after the user confirmed.
    pub fn store(&self, config: &Path) -> Result<(), MacAppError> {
        if let Some(mode) = self.umu {
            umu::store_at(config, mode)?;
        }
        if let Some(dir) = self.proton_dir {
            super::store_key(config, "proton_dir", dir.as_str())?;
        }
        Ok(())
    }

    /// What a pipeline's `PromptFn` answers: the Galaxy question, which setup
    /// asks (and stores) itself once the game is there. `None` for anything
    /// else, which cancels it.
    pub fn replay(&self, spec: &PromptSpec) -> Option<PromptAnswer> {
        let galaxy = galaxy_switch::ask_spec("", true, false);
        if spec.kind != PromptKind::Select || spec.items != galaxy.items {
            return None;
        }
        self.galaxy
            .map(|m| PromptAnswer::Index(usize::from(m != GalaxyMode::Comet)))
    }
}

/// What a [`Download`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Umu,
    Proton,
    Comet,
}

/// Something the pipeline would download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub item: Item,
    pub version: String,
    pub bytes: u64,
    /// A Steam's `compatibilitytools.d`; `None` for gogo's store.
    pub into: Option<PathBuf>,
}

/// The runtime side of a preview, the same for a build and a set-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub umu: UmuMode,
    /// `ge-proton/<version>` or `path:<dir>`.
    pub runtime: String,
    pub downloads: Vec<Download>,
    /// Comet will run beside the game (if it ships the SDK, when that isn't
    /// known yet).
    pub galaxy: bool,
    /// The user said yes to installing Comet.
    pub install_comet: bool,
}

/// What `linux-app setup <gogid>` would do.
#[derive(Debug, Clone)]
pub struct SetupPreview {
    pub title: String,
    pub dirs: GameDirs,
    pub prefix_exists: bool,
    /// The GE-Proton changes: from, to.
    pub runtime_change: Option<(String, String)>,
    pub tools: Tools,
}

/// What a build is previewed from.
#[derive(Debug, Clone, Copy)]
pub struct BuildRequest<'a> {
    pub gogid: &'a str,
    /// The game files, when they're here already (the depot cache).
    pub tree: Option<&'a Path>,
    /// `--runtime`.
    pub runtime: Option<&'a str>,
    /// `--renderer`.
    pub renderer: RendererChoice,
    /// `library.dir`, unprobed.
    pub library: Option<&'a Path>,
}

/// What a build would do.
#[derive(Debug, Clone)]
pub struct BuildPreview {
    pub dirs: GameDirs,
    /// The game is there: its files are replaced, the prefix and saves stay.
    pub rebuild: bool,
    pub prefix_exists: bool,
    /// Bytes under `tree`.
    pub tree_bytes: Option<u64>,
    /// The renderer, when the files are here to scan.
    pub renderer: Option<String>,
    /// The winetricks verbs the files call for.
    pub verbs: Vec<String>,
    pub tools: Tools,
}

/// The preview of a built game's set-up. The questions come in setup's order:
/// Galaxy, GE-Proton's dir, umu.
pub fn setup(
    gogid: &str,
    answers: &Answers,
    env: &Env<'_>,
) -> Result<Preview<SetupPreview>, MacAppError> {
    let dirs = GameDirs::for_game(gogid)?;
    refuse_private_tmp("gogo's data dir", &crate::state::data_root())?;
    let prior = LinuxState::load(&dirs.state_toml)?;
    let (baked, _) = game::read_launch_toml(&dirs)?;
    game::refuse_running(gogid, &dirs.root, &dirs.prefix)?;
    let l4 = compat::load_user_for(gogid, Os::Linux)?;

    let files = setup::game_files(&dirs, baked.tree_name().as_deref());
    let sdk = files
        .is_dir()
        .then(|| galaxy_switch::detect_sdk(&files))
        .flatten();
    let galaxy = match galaxy(gogid, &l4.entry, sdk, false, answers, env)? {
        Preview::Ask(q) => return Ok(Preview::Ask(q)),
        Preview::Ready(g) => g,
    };

    let runtime = l4.entry.runtime.clone().unwrap_or(baked.runtime.clone());
    let r = ProtonRef::parse(&runtime).map_err(MacAppError::Proton)?;
    let mut downloads = Vec::new();
    let reference = match proton(&r, answers, env, &mut downloads)? {
        Preview::Ask(q) => return Ok(Preview::Ask(q)),
        Preview::Ready(reference) => reference,
    };
    let umu = match umu_step(answers, env, &mut downloads)? {
        Preview::Ask(q) => return Ok(Preview::Ask(q)),
        Preview::Ready(mode) => mode,
    };
    let (galaxy, install_comet) = match comet_step(galaxy, answers, env, &mut downloads)? {
        Preview::Ask(q) => return Ok(Preview::Ask(q)),
        Preview::Ready(v) => v,
    };

    let runtime_change = prior
        .as_ref()
        .filter(|p| !p.runtime.is_empty() && p.runtime != reference)
        .map(|p| (p.runtime.clone(), reference.clone()));
    Ok(Preview::Ready(SetupPreview {
        title: baked.title.clone(),
        prefix_exists: setup::is_prefix(&dirs.prefix),
        runtime_change,
        tools: Tools {
            umu,
            runtime: reference,
            downloads,
            galaxy,
            install_comet,
        },
        dirs,
    }))
}

/// The preview of a build. The questions come in the build's order: umu,
/// GE-Proton's dir, then setup's Galaxy.
pub fn build(
    req: &BuildRequest<'_>,
    answers: &Answers,
    env: &Env<'_>,
) -> Result<Preview<BuildPreview>, MacAppError> {
    let gogid = req.gogid;
    compat::validate_gogid(gogid)?;
    refuse_private_tmp("gogo's data dir", &crate::state::data_root())?;
    let dirs = GameDirs::for_build_in(gogid, req.library)?;
    refuse_private_tmp("the game's dir", &dirs.root)?;
    let rebuild = game::launch_toml_path(&dirs).is_file() || dirs.game.exists();
    if rebuild {
        game::refuse_running(gogid, &dirs.root, &dirs.prefix)?;
    }

    let mut downloads = Vec::new();
    let umu = match umu_step(answers, env, &mut downloads)? {
        Preview::Ask(q) => return Ok(Preview::Ask(q)),
        Preview::Ready(mode) => mode,
    };
    // As `build::plan` resolves it: the flag, else L3, else the default.
    let compat = build::compat_for(gogid, &mut |_| {});
    let l3 = compat
        .runtime
        .as_ref()
        .filter(|r| r.layer == Layer::Db)
        .map(|r| r.value.clone());
    let r = match req.runtime.or(l3.as_deref()) {
        Some(s) => ProtonRef::parse(s).map_err(MacAppError::Proton)?,
        None => proton::default_ref(super::stored_key(&env.config, "default_runtime").as_deref())
            .map_err(MacAppError::Proton)?,
    };
    let mut reference = match proton(&r, answers, env, &mut downloads)? {
        Preview::Ask(q) => return Ok(Preview::Ask(q)),
        Preview::Ready(reference) => reference,
    };
    let l4 = compat::load_user_for(gogid, Os::Linux)?;
    // Setup runs the game's own L4 runtime, unless `--runtime` replaces it.
    if req.runtime.is_none()
        && let Some(own) = &l4.entry.runtime
    {
        let own = ProtonRef::parse(own).map_err(MacAppError::Proton)?;
        if own != r {
            reference = match proton(&own, answers, env, &mut downloads)? {
                Preview::Ask(q) => return Ok(Preview::Ask(q)),
                Preview::Ready(reference) => reference,
            };
        }
    }

    let sdk = req.tree.and_then(galaxy_switch::detect_sdk);
    let galaxy = match galaxy(gogid, &l4.entry, sdk, req.tree.is_none(), answers, env)? {
        Preview::Ask(q) => return Ok(Preview::Ask(q)),
        Preview::Ready(g) => g,
    };
    let (galaxy, install_comet) = match comet_step(galaxy, answers, env, &mut downloads)? {
        Preview::Ask(q) => return Ok(Preview::Ask(q)),
        Preview::Ready(v) => v,
    };

    let scan = req.tree.map(pe::scan);
    let verbs = scan
        .as_ref()
        .and_then(|s| s.as_ref().ok())
        .map(|s| verbs::names(&verbs::for_scan(s)))
        .unwrap_or_default();
    let renderer = scan.as_ref().map(|s| {
        build::select_renderer(Some(gogid), s, req.renderer, &mut |_| {})
            .renderer
            .value
    });
    Ok(Preview::Ready(BuildPreview {
        rebuild,
        prefix_exists: setup::is_prefix(&dirs.prefix),
        tree_bytes: req.tree.map(|t| build::kept_bytes(t, None)),
        renderer,
        verbs,
        tools: Tools {
            umu,
            runtime: reference,
            downloads,
            galaxy,
            install_comet,
        },
        dirs,
    }))
}

fn entry(id: &str) -> Result<&'static CatalogEntry, MacAppError> {
    catalog::find(id).ok_or_else(|| MacAppError::Proton(format!("{id} is not in gogo's catalog")))
}

fn probe(env: &Env<'_>, entry: &CatalogEntry, version: Option<&str>) -> Result<u64, MacAppError> {
    Ok(lutris::block_on_detached(catalog::probe(entry, version, &env.http, &env.api))?.size)
}

/// How umu would run, checked as the pipeline checks it, and its download.
fn umu_step(
    answers: &Answers,
    env: &Env<'_>,
    downloads: &mut Vec<Download>,
) -> Result<Preview<UmuMode>, MacAppError> {
    let entry = entry(catalog::UMU)?;
    let version = setup::pinned_version(catalog::UMU);
    let installed = store::installed(catalog::UMU, version).is_some();
    let Some(mode) = answers.umu.or_else(|| umu::stored_at(&env.config)) else {
        let size = probe(env, entry, None)?;
        let blocker = umu::resolve_python(&env.host).err().map(|e| match e {
            MacAppError::Umu(why) => why,
            e => e.to_string(),
        });
        let found = umu::resolve_system(&env.host).is_ok();
        return Ok(Preview::Ask(Question {
            ask: Ask::Umu,
            spec: umu::prompt_spec(entry, size, &env.config, blocker.as_deref(), found),
        }));
    };
    match mode {
        UmuMode::System => {
            umu::resolve_system(&env.host)?;
        }
        UmuMode::Zipapp => {
            // The interpreter first: nothing is downloaded that couldn't run.
            umu::resolve_python(&env.host)?;
            if !installed {
                downloads.push(Download {
                    item: Item::Umu,
                    version: version.unwrap_or_default().to_string(),
                    bytes: probe(env, entry, None)?,
                    into: None,
                });
            }
        }
    }
    Ok(Preview::Ready(mode))
}

/// The Proton build `r` names, and its download when it's missing or no longer
/// matches its marker.
fn proton(
    r: &ProtonRef,
    answers: &Answers,
    env: &Env<'_>,
    downloads: &mut Vec<Download>,
) -> Result<Preview<String>, MacAppError> {
    if let Some(b) = setup::find_proton(r)?
        && setup::changed_managed(r, &b.dir).is_none()
    {
        return Ok(Preview::Ready(b.reference));
    }
    let ProtonRef::Catalog { id, version } = r else {
        unreachable!("find_proton returns path: builds or fails")
    };
    let entry = entry(id)?;
    let version = version
        .as_deref()
        .or(setup::pinned_version(id))
        .ok_or_else(|| MacAppError::Proton(format!("{id} has no pinned version")))?;
    let bytes = probe(env, entry, Some(version))?;
    let roots = proton::steam_roots(&env.host);
    let choice = answers
        .proton_dir
        .or_else(|| proton::stored_at(&env.config));
    let choice = match choice {
        Some(c) => c,
        None if roots.is_empty() => ProtonDir::Store,
        None => {
            let store = store::runtimes_dir().join(entry.id);
            let mut dirs = vec![ProtonDir::Store];
            dirs.extend(roots.iter().map(|r| r.kind.proton_dir()));
            return Ok(Preview::Ask(Question {
                ask: Ask::ProtonDir(dirs),
                spec: proton::prompt_spec(entry, Some(bytes), &roots, &store, &env.config),
            }));
        }
    };
    downloads.push(Download {
        item: Item::Proton,
        version: version.to_string(),
        bytes,
        into: proton::root_for(choice, &roots).map(|r| r.tools_dir()),
    });
    Ok(Preview::Ready(format!("{id}/{version}")))
}

/// Whether Comet runs beside the game, and whether this run's answer turned it
/// on.
#[derive(Debug, Clone, Copy)]
struct Galaxy {
    on: bool,
    asked_on: bool,
}

/// `unknown`: the files aren't here yet, so `galaxy_default = "ask"` asks in
/// case they turn out to ship the SDK.
fn galaxy(
    gogid: &str,
    l4: &compat::CompatEntry,
    sdk: Option<bool>,
    unknown: bool,
    answers: &Answers,
    env: &Env<'_>,
) -> Result<Preview<Galaxy>, MacAppError> {
    let (default, _) = galaxy_switch::default_at(&env.config);
    let (own, recommended) = galaxy_switch::split(&galaxy_switch::effective(gogid, l4));
    let decision = galaxy_switch::decide(own, recommended, default, sdk);
    let might = unknown && own.is_none() && default == GalaxyDefault::Ask;
    if !decision.ask && !might {
        return Ok(Preview::Ready(Galaxy {
            on: decision.on,
            asked_on: false,
        }));
    }
    let Some(answer) = answers.galaxy else {
        let spec = if decision.ask {
            galaxy_switch::ask_spec(gogid, sdk == Some(true), recommended)
        } else {
            galaxy_switch::unknown_spec(gogid)
        };
        return Ok(Preview::Ask(Question {
            ask: Ask::Galaxy,
            spec,
        }));
    };
    let on = answer == GalaxyMode::Comet;
    Ok(Preview::Ready(Galaxy { on, asked_on: on }))
}

/// After "on": offer to install Comet when it's missing, as the CLI does.
/// Returns whether Comet will run and whether to install it.
fn comet_step(
    galaxy: Galaxy,
    answers: &Answers,
    env: &Env<'_>,
    downloads: &mut Vec<Download>,
) -> Result<Preview<(bool, bool)>, MacAppError> {
    if !galaxy.asked_on {
        return Ok(Preview::Ready((galaxy.on, false)));
    }
    let mode = comet::mode_at(&env.config)?;
    if !galaxy_switch::offer_applies(&env.host, &mode) {
        return Ok(Preview::Ready((galaxy.on, false)));
    }
    let entry = entry(catalog::COMET)?;
    let bytes = probe(env, entry, None)?;
    match answers.install_comet {
        None => Ok(Preview::Ask(Question {
            ask: Ask::InstallComet,
            spec: galaxy_switch::offer_spec(&comet::plan_line(entry, bytes)),
        })),
        Some(false) => Ok(Preview::Ready((galaxy.on, false))),
        Some(true) => {
            downloads.push(Download {
                item: Item::Comet,
                version: setup::pinned_version(catalog::COMET)
                    .unwrap_or_default()
                    .to_string(),
                bytes,
                into: None,
            });
            Ok(Preview::Ready((galaxy.on, true)))
        }
    }
}

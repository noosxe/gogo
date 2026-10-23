//! `gogo mac-app doctor <bundle|gogid>` (#79, docs/19 §6.6): can this game
//! work on this Mac? A list of findings (ok / warn / fail, one line each with
//! a fix hint) from read-only checks of the bundle, the runtime store, the
//! state dir, the host and the last launch. It never launches the game; the
//! only thing it changes is, with [`DoctorOptions::stop_leftovers`], Wine
//! processes a dead launcher left running in this game's prefix (#103).

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::config::Consent;

use super::compat;
use super::error::MacAppError;
use super::launch::{self, GameDirs, GameState, LaunchToml};
use super::renderer::{self, Renderer, RendererRef};
use super::{catalog, d3dmetal, installer, store};

/// How bad a finding is. Only [`Level::Fail`] fails the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

/// One check's answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// The check's id: `bundle`, `codesign`, `compat`, `runtime`, `renderer`,
    /// `d3dmetal`, `stamp`, `links`, `rosetta`, `processes`, `log`.
    pub check: &'static str,
    pub level: Level,
    pub message: String,
    /// What to run or do about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Extra lines (the last launch's output tail).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
}

impl Finding {
    fn new(check: &'static str, level: Level, message: impl Into<String>) -> Finding {
        Finding {
            check,
            level,
            message: message.into(),
            hint: None,
            detail: vec![],
        }
    }

    fn ok(check: &'static str, message: impl Into<String>) -> Finding {
        Finding::new(check, Level::Ok, message)
    }

    fn warn(check: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Finding {
        Finding::new(check, Level::Warn, message).hint(hint)
    }

    fn fail(check: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Finding {
        Finding::new(check, Level::Fail, message).hint(hint)
    }

    fn hint(mut self, hint: impl Into<String>) -> Finding {
        self.hint = Some(hint.into());
        self
    }
}

/// The host facts the checks read; [`Host::detect`] for the real Mac, a
/// stub per field in tests.
#[derive(Debug, Clone)]
pub struct Host {
    /// `codesign`; `None`: not on this host, the check is skipped.
    pub codesign: Option<PathBuf>,
    /// The file that proves Rosetta 2 is installed; `None`: not needed here
    /// (an Intel Mac, or not macOS).
    pub rosetta: Option<PathBuf>,
    /// Where the launcher writes run logs: `<logs>/<gogid>/` (#77).
    pub logs: PathBuf,
}

impl Host {
    pub fn detect() -> Host {
        let codesign = Path::new("/usr/bin/codesign");
        let needs_rosetta = cfg!(all(target_os = "macos", target_arch = "aarch64"));
        Host {
            codesign: codesign.is_file().then(|| codesign.to_path_buf()),
            rosetta: needs_rosetta
                .then(|| PathBuf::from("/Library/Apple/usr/share/rosetta/rosetta")),
            logs: dirs::home_dir()
                .unwrap_or_default()
                .join("Library/Logs/gogo"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub host: Host,
    /// Stop Wine processes left running in the prefix with no live launcher
    /// (the launcher's own teardown: `wineserver -k`, then SIGKILL).
    pub stop_leftovers: bool,
}

/// What [`doctor`] found.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub gogid: String,
    pub bundle: Option<PathBuf>,
    pub findings: Vec<Finding>,
}

impl Report {
    /// No finding failed (warnings don't count).
    pub fn ok(&self) -> bool {
        self.findings.iter().all(|f| f.level != Level::Fail)
    }
}

/// Run every check for `target`, a gogid or a v2 bundle path. Errors only
/// when the target names no game at all; everything else is a finding.
pub fn doctor(target: &str, opts: &DoctorOptions) -> Result<Report, MacAppError> {
    let given_bundle = (!is_gogid(target)).then(|| launch::absolute(Path::new(target)));
    let gogid = super::setup::target_gogid(target)?;
    let dirs = GameDirs::for_game(&gogid)?;
    let state = GameState::load(&dirs.state_toml);
    let mut f = Vec::new();

    // The bundle and what it baked: the one given, the one setup recorded,
    // or the portable bundle (#84) whose launcher set the prefix up.
    let bundle = given_bundle
        .or_else(|| {
            state
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .map(|s| s.bundle.clone())
        })
        .or_else(|| super::portable::marker(&gogid).map(|m| m.bundle));
    let toml_raw = bundle
        .as_ref()
        .map(|b| b.join("Contents/Resources/gogo-launch.toml"))
        .map(|p| {
            std::fs::read(&p)
                .map(|raw| (p.clone(), raw))
                .map_err(|e| (p, e))
        });
    let baked = match &toml_raw {
        None => {
            f.push(Finding::fail(
                "bundle",
                format!("no state for {gogid} and no bundle given"),
                format!("pass the .app path, or build it: gogo mac-app {gogid}"),
            ));
            None
        }
        Some(Err((p, e))) => {
            f.push(Finding::fail(
                "bundle",
                format!("cannot read {}: {e}", p.display()),
                format!("rebuild the bundle: gogo mac-app {gogid} --yes"),
            ));
            None
        }
        Some(Ok((p, raw))) => match LaunchToml::parse(&String::from_utf8_lossy(raw), p) {
            Ok(t) => {
                f.push(Finding::ok(
                    "bundle",
                    format!("{} ({})", bundle.as_ref().expect("read").display(), t.title),
                ));
                Some((t, raw.clone()))
            }
            Err(e) => {
                f.push(Finding::fail(
                    "bundle",
                    e.to_string(),
                    format!("rebuild the bundle: gogo mac-app {gogid} --yes"),
                ));
                None
            }
        },
    };
    if let (Some(b), Some(_)) = (&bundle, &baked) {
        f.push(check_codesign(&opts.host, b, &gogid));
    }
    if let (Some(b), Some((t, raw))) = (&bundle, &baked)
        && let Some(p) = &t.portable
    {
        portable_checks(b, t, p, raw, &dirs, &gogid, opts, &mut f);
        return Ok(Report {
            gogid,
            bundle,
            findings: f,
        });
    }
    f.push(check_compat(
        &gogid,
        baked.as_ref().map(|(t, _)| t.built_from()),
    ));

    // The effective config, as setup resolves it (bundle, then L4).
    let l4_raw = std::fs::read(compat::user_path(&gogid)).ok();
    let l4 = compat::load_user(&gogid);
    if let Err(e) = &l4 {
        f.push(Finding::fail(
            "compat",
            e.to_string(),
            format!("fix or remove {}", compat::user_path(&gogid).display()),
        ));
    }
    let l4_entry = l4.as_ref().map(|p| p.entry.clone()).unwrap_or_default();
    let eff = baked.as_ref().map(|(t, _)| {
        // An unknown `variant` (#85) fails setup; check the rest as if unset.
        t.effective(&compat::db_entry(&gogid).entry, &l4_entry)
            .unwrap_or_else(|reason| {
                f.push(Finding::fail(
                    "variant",
                    reason,
                    format!(
                        "fix `variant` in {} (`gogo mac-app compat {gogid}` lists them)",
                        compat::user_path(&gogid).display()
                    ),
                ));
                t.with_override(&l4_entry)
            })
    });
    let state = match state {
        Ok(s) => s,
        Err(e) => {
            f.push(Finding::fail(
                "stamp",
                e.to_string(),
                format!("gogo mac-app setup {gogid}"),
            ));
            None
        }
    };

    if let Some(eff) = &eff {
        let rt = check_runtime(&eff.runtime, &gogid, &mut f);
        check_renderer(&eff.renderer, rt.as_ref(), eff, &dirs, &gogid, &mut f);
        check_d3dmetal(&eff.renderer, state.as_ref(), eff, &dirs, &mut f);
    }
    if let Some((_, raw)) = &baked {
        f.push(check_stamp(
            &dirs,
            state.as_ref(),
            raw,
            l4_raw.as_deref(),
            &gogid,
        ));
    }
    f.push(check_links(&dirs, &gogid));
    f.push(check_rosetta(
        &opts.host,
        eff.as_ref().map(|e| e.runtime.as_str()),
    ));
    f.push(check_processes(
        &dirs,
        state.as_ref(),
        opts.stop_leftovers,
        &gogid,
    ));
    f.push(check_log(&opts.host.logs.join(&gogid)));

    Ok(Report {
        gogid,
        bundle,
        findings: f,
    })
}

/// A portable bundle (#84) carries its runtime and renderer and sets its
/// own prefix up at launch, so the store and setup checks don't apply: the
/// bundled Wine probes, the renderer's files are in the bundle, and the
/// prefix (if there is one yet) was set up from this build of it.
#[allow(clippy::too_many_arguments)]
fn portable_checks(
    bundle: &Path,
    t: &LaunchToml,
    p: &launch::Portable,
    raw: &[u8],
    dirs: &GameDirs,
    gogid: &str,
    opts: &DoctorOptions,
    f: &mut Vec<Finding>,
) {
    let res = bundle.join("Contents/Resources");
    let rebuild = format!("rebuild it: gogo mac-app {gogid} --portable --yes");
    let mut compat = check_compat(gogid, Some(t.built_from()));
    compat.message.push_str(
        " (portable: the compat DB and your override were baked in at build; a change to \
         either needs a rebuild)",
    );
    f.push(compat);

    let wine = res.join(&p.wine);
    if wine.is_file() {
        let mut cmd = Command::new(&wine);
        cmd.arg("--version");
        if let Some(fw) = &p.frameworks {
            cmd.env("DYLD_FALLBACK_LIBRARY_PATH", res.join(fw));
        }
        match cmd.output() {
            Ok(out) if out.status.success() => {
                let v = String::from_utf8_lossy(&out.stdout);
                f.push(Finding::ok(
                    "runtime",
                    format!(
                        "bundled {} ({})",
                        t.runtime,
                        v.lines().next().unwrap_or("").trim()
                    ),
                ));
            }
            Ok(out) => f.push(Finding::fail(
                "runtime",
                format!(
                    "the bundled Wine doesn't run (`wine --version` exited {}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                rebuild.clone(),
            )),
            Err(e) => f.push(Finding::fail(
                "runtime",
                format!("cannot run the bundled Wine {}: {e}", wine.display()),
                rebuild.clone(),
            )),
        }
    } else {
        f.push(Finding::fail(
            "runtime",
            format!("the bundled Wine {} is missing", wine.display()),
            "copy the bundle again, or rebuild it",
        ));
    }

    match RendererRef::parse(&t.renderer).map(|r| r.renderer) {
        Ok(Renderer::Wined3d) => f.push(Finding::ok("renderer", "wined3d (built into Wine)")),
        Ok(Renderer::CncDdraw) if res.join(&p.game).join("ddraw.dll").is_file() => f.push(
            Finding::ok("renderer", "cnc-ddraw: ddraw.dll in the bundled game dir"),
        ),
        Ok(Renderer::CncDdraw) => f.push(Finding::fail(
            "renderer",
            "cnc-ddraw: no ddraw.dll in the bundled game dir",
            rebuild.clone(),
        )),
        Ok(Renderer::Dxmt) => {
            let overlay = Path::new(&p.wine)
                .parent()
                .and_then(Path::parent)
                .map(|e| res.join(e).join("lib/wine/x86_64-unix/winemetal.so"));
            match overlay {
                Some(o) if o.is_file() => f.push(Finding::ok(
                    "renderer",
                    "dxmt: the bundled engine has x86_64-unix/winemetal.so",
                )),
                _ => f.push(Finding::fail(
                    "renderer",
                    "dxmt: the bundled engine lacks lib/wine/x86_64-unix/winemetal.so",
                    rebuild.clone(),
                )),
            }
        }
        Ok(Renderer::D3dMetal) => f.push(Finding::fail(
            "renderer",
            "d3dmetal in a portable bundle: gogo never puts D3DMetal into one",
            format!("{rebuild} --renderer dxmt"),
        )),
        Err(e) => f.push(Finding::fail("renderer", e, rebuild.clone())),
    }

    let launch_it = "launch the game: its launcher sets the prefix up".to_string();
    f.push(if !dirs.prefix.is_dir() {
        Finding::ok(
            "stamp",
            "no prefix yet: the first launch creates it with the bundled Wine",
        )
    } else {
        match super::portable::marker(gogid) {
            Some(m) if m.stamp == launch::stamp(raw, None) => {
                Finding::ok("stamp", "prefix set up from this bundle")
            }
            Some(m) => Finding::warn(
                "stamp",
                format!(
                    "prefix set up from another build of this game ({}): the next launch \
                     applies this one's verbs and registry values",
                    m.bundle.display()
                ),
                launch_it,
            ),
            None => Finding::warn(
                "stamp",
                "the prefix exists, but this bundle never set it up (a gogo install of the \
                 same game, or a first launch that didn't finish): the next launch applies \
                 its verbs and registry values",
                launch_it,
            ),
        }
    });
    f.push(check_links(dirs, gogid));
    f.push(check_rosetta(&opts.host, None));
    f.push(check_processes(dirs, None, opts.stop_leftovers, gogid));
    f.push(check_log(&opts.host.logs.join(gogid)));
}

fn is_gogid(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn rebuild_hint(gogid: &str) -> String {
    format!("rebuild the bundle: gogo mac-app {gogid} --yes")
}

// — the checks —

fn check_codesign(host: &Host, bundle: &Path, gogid: &str) -> Finding {
    let Some(codesign) = &host.codesign else {
        return Finding::ok("codesign", "skipped: no codesign on this host");
    };
    match Command::new(codesign)
        .args(["--verify", "--deep", "--strict"])
        .arg(bundle)
        .output()
    {
        Ok(out) if out.status.success() => Finding::ok("codesign", "signature valid"),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
            Finding::fail(
                "codesign",
                format!("codesign --verify failed: {}", last.trim()),
                rebuild_hint(gogid),
            )
        }
        Err(e) => Finding::warn(
            "codesign",
            format!("could not run {}: {e}", codesign.display()),
            "check the Xcode command line tools",
        ),
    }
}

/// The compat status for the lane the bundle was built from (#56); `None`
/// (no readable bundle) = whatever lane the entry was seen on.
fn check_compat(gogid: &str, lane: Option<compat::Lane>) -> Finding {
    let (eff, _) = compat::db_and_user(gogid);
    let line = eff.status_line_for(lane);
    match eff.status_for(lane) {
        compat::Status::Broken => Finding::warn(
            "compat",
            line,
            format!("see `gogo mac-app compat {gogid}`; it may not run at all"),
        ),
        _ => Finding::ok("compat", line),
    }
}

/// The effective runtime is resolvable and probes; a `path:` one (a host
/// Wine, #125) works but warns.
fn check_runtime(r: &str, gogid: &str, f: &mut Vec<Finding>) -> Option<super::runtime::Runtime> {
    match launch::resolve_ref(r) {
        Ok(rt) if r.starts_with("path:") => {
            f.push(Finding::warn(
                "runtime",
                format!(
                    "{r} is a host Wine outside the store ({}): not pinned, no renderer \
                     overlays, can change under the game",
                    rt.version
                ),
                format!("gogo mac-app --retarget {gogid} --runtime winecx"),
            ));
            Some(rt)
        }
        Ok(rt) => {
            f.push(Finding::ok("runtime", format!("{r} ({})", rt.version)));
            Some(rt)
        }
        Err(e) => {
            let hint = match r.strip_prefix("path:") {
                Some(_) => format!(
                    "reinstall that Wine, or gogo mac-app --retarget {gogid} --runtime winecx"
                ),
                None => {
                    let (id, ver) = r.split_once('/').map_or((r, None), |(i, v)| (i, Some(v)));
                    format!(
                        "gogo runtimes install {id}{}",
                        ver.map(|v| format!(" --version {v}")).unwrap_or_default()
                    )
                }
            };
            f.push(Finding::fail("runtime", format!("{r}: {e}"), hint));
            None
        }
    }
}

/// The files the chosen renderer needs: a derived runtime with the overlay
/// (DXMT, D3DMetal), or `ddraw.dll` in the game dir (cnc-ddraw).
fn check_renderer(
    r: &str,
    rt: Option<&super::runtime::Runtime>,
    eff: &LaunchToml,
    dirs: &GameDirs,
    gogid: &str,
    f: &mut Vec<Finding>,
) {
    let setup = format!("gogo mac-app setup {gogid}");
    let rref = match RendererRef::parse(r) {
        Ok(rref) => rref,
        Err(e) => {
            f.push(Finding::fail(
                "renderer",
                e,
                format!("fix `renderer` in {}", compat::user_path(gogid).display()),
            ));
            return;
        }
    };
    match rref.renderer {
        Renderer::Wined3d => f.push(Finding::ok("renderer", "wined3d (built into Wine)")),
        Renderer::CncDdraw => {
            let dll = eff
                .tree_name()
                .map(|n| dirs.prefix.join("drive_c/GOG").join(n).join("ddraw.dll"));
            match dll {
                Some(d) if d.is_file() => f.push(Finding::ok(
                    "renderer",
                    "cnc-ddraw: ddraw.dll in the game dir",
                )),
                _ => f.push(Finding::fail(
                    "renderer",
                    "cnc-ddraw: no ddraw.dll in the game dir",
                    setup,
                )),
            }
        }
        Renderer::D3dMetal if !renderer::d3dmetal_available() => f.push(Finding::ok(
            "renderer",
            "d3dmetal without consent: setup runs wined3d instead (see d3dmetal)",
        )),
        Renderer::Dxmt | Renderer::D3dMetal => {
            let name = rref.renderer.name();
            let Some(rt) = rt else {
                f.push(Finding::ok(
                    "renderer",
                    format!("{name}: not checked (no runtime)"),
                ));
                return;
            };
            if !rt.is_crossover() {
                f.push(Finding::fail(
                    "renderer",
                    format!(
                        "{name} needs a CrossOver-source engine (winecx), not {}",
                        rt.version
                    ),
                    format!("gogo mac-app --retarget {gogid} --runtime winecx"),
                ));
                return;
            }
            let id = rref.renderer.id();
            let ver = rref.version();
            let Some((payload, _)) = store::installed(id, ver.as_deref()) else {
                f.push(Finding::fail(
                    "renderer",
                    format!(
                        "{id}/{} is not in the renderer store",
                        ver.unwrap_or_default()
                    ),
                    setup,
                ));
                return;
            };
            let base = launch::runtime_ref(rt);
            let Some((base_id, base_ver)) =
                base.split_once('/').filter(|_| !base.starts_with("path:"))
            else {
                f.push(Finding::fail(
                    "renderer",
                    format!("{name}: runtime {base} is outside the store"),
                    format!("gogo mac-app --retarget {gogid} --runtime winecx"),
                ));
                return;
            };
            let dv = store::derived_version(base_ver, &payload);
            let derived = format!("{base_id}/{dv}");
            let Some((m, root)) = store::installed(base_id, Some(&dv)) else {
                f.push(Finding::fail(
                    "renderer",
                    format!("derived runtime {derived} is not in the store"),
                    setup,
                ));
                return;
            };
            // The payload's `<arch>-unix/<file>` (`x86_64-unix/winemetal.so`),
            // overlaid into the derived engine's `lib/wine/`.
            let rel: PathBuf = Path::new(&payload.entry_rel)
                .components()
                .rev()
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let engine = root
                .join(&m.entry_rel)
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .unwrap_or_else(|| root.join("engine"));
            let file = engine.join("lib/wine").join(&rel);
            if file.is_file() {
                f.push(Finding::ok(
                    "renderer",
                    format!("{name}: {derived} has {}", rel.display()),
                ));
            } else {
                f.push(Finding::fail(
                    "renderer",
                    format!("{name}: {derived} lacks lib/wine/{}", rel.display()),
                    format!("gogo runtimes prune {derived}, then {setup}"),
                ));
            }
        }
    }
}

/// D3DMetal consent (#71), only when D3DMetal is the renderer or the one
/// "Try next renderer" would switch to.
fn check_d3dmetal(
    r: &str,
    state: Option<&GameState>,
    eff: &LaunchToml,
    dirs: &GameDirs,
    f: &mut Vec<Finding>,
) {
    let current = RendererRef::parse(r).is_ok_and(|r| r.renderer == Renderer::D3dMetal);
    let next = || {
        let game = eff
            .tree_name()
            .map(|n| dirs.prefix.join("drive_c/GOG").join(n))
            .filter(|g| g.is_dir())?;
        let exe = launch::windows_to_unix(&dirs.prefix, &eff.exe).filter(|e| e.is_file());
        let pick = renderer::auto(&game, exe.as_deref());
        let applied = state.map_or(r, |s| s.renderer.as_str());
        (renderer::next_after(&pick, applied, true) == Some(Renderer::D3dMetal)).then_some(())
    };
    if !current && next().is_none() {
        return;
    }
    let role = if current {
        "the renderer"
    } else {
        "the next renderer"
    };
    let hint = format!(
        "to allow it, set {} = \"yes\" in {}",
        d3dmetal::KEY,
        d3dmetal::config_path().display()
    );
    f.push(match d3dmetal::stored() {
        Some(Consent::Yes) => {
            Finding::ok("d3dmetal", format!("D3DMetal is {role}; download allowed"))
        }
        Some(Consent::No) => Finding::warn(
            "d3dmetal",
            format!("D3DMetal is {role}, but you declined its download: wined3d runs instead"),
            hint,
        ),
        None => Finding::warn(
            "d3dmetal",
            format!(
                "D3DMetal is {role}, but you were never asked to allow it: wined3d runs instead"
            ),
            hint,
        ),
    });
}

/// Prefix present, and setup's stamp matches the bundle TOML + L4 file.
fn check_stamp(
    dirs: &GameDirs,
    state: Option<&GameState>,
    toml_raw: &[u8],
    l4_raw: Option<&[u8]>,
    gogid: &str,
) -> Finding {
    let setup = format!("gogo mac-app setup {gogid}");
    if !dirs.prefix.is_dir() {
        return Finding::fail(
            "stamp",
            format!("no prefix at {}", dirs.prefix.display()),
            setup,
        );
    }
    let Some(state) = state.filter(|_| dirs.launch_conf.is_file()) else {
        return Finding::fail(
            "stamp",
            "setup never completed (no state.toml or launch.conf)",
            setup,
        );
    };
    let want = launch::stamp(toml_raw, l4_raw);
    if state.stamp == want {
        Finding::ok(
            "stamp",
            format!(
                "config current (runtime {}, renderer {})",
                state.runtime, state.renderer
            ),
        )
    } else {
        Finding::warn(
            "stamp",
            "config stale: the bundle or your override changed since the last setup (the next launch runs it)",
            setup,
        )
    }
}

/// No symlink in the prefix leads out of the state dir (design 16
/// decision 4): not into `/Users/*`, not to `/` (`z:`).
fn check_links(dirs: &GameDirs, gogid: &str) -> Finding {
    if !dirs.prefix.is_dir() {
        return Finding::ok("links", "no prefix yet");
    }
    let root = crate::state::state_dir();
    let mut out = Vec::new();
    walk_links(&dirs.prefix, &root, &dirs.prefix, &mut out);
    if out.is_empty() {
        return Finding::ok("links", "no links out of the state dir");
    }
    let shown: Vec<String> = out
        .iter()
        .take(3)
        .map(|(p, t)| {
            format!(
                "{} -> {}",
                p.strip_prefix(&dirs.prefix).unwrap_or(p).display(),
                t.display()
            )
        })
        .collect();
    let more = out.len().saturating_sub(shown.len());
    Finding::fail(
        "links",
        format!(
            "{} link(s) out of the state dir: {}{}",
            out.len(),
            shown.join(", "),
            if more > 0 {
                format!(", +{more} more")
            } else {
                String::new()
            }
        ),
        format!("gogo mac-app setup {gogid} (turns home links into dirs, drops host drives)"),
    )
}

fn walk_links(dir: &Path, root: &Path, prefix: &Path, out: &mut Vec<(PathBuf, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let Ok(ty) = e.file_type() else { continue };
        if ty.is_symlink() {
            let Ok(target) = std::fs::read_link(&p) else {
                continue;
            };
            let resolved = normalize(&p.parent().unwrap_or(prefix).join(&target));
            if !resolved.starts_with(root) {
                out.push((p, target));
            }
        } else if ty.is_dir() {
            walk_links(&p, root, prefix, out);
        }
    }
}

/// Lexical `..`/`.` removal (the link target may not exist).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// Rosetta for the game's runtime (#82, [`catalog::runs_on`]); a portable
/// bundle's (`None`) is x86_64 like every runtime today.
fn check_rosetta(host: &Host, runtime: Option<&str>) -> Finding {
    let host = match &host.rosetta {
        None => return Finding::ok("rosetta", "not needed on this host"),
        Some(p) => catalog::HostArch::AppleSilicon {
            rosetta: p.exists(),
        },
    };
    let arch = runtime.map_or(catalog::Arch::X86_64Rosetta, catalog::runtime_arch);
    let which = runtime.unwrap_or("the bundled runtime");
    match (catalog::runs_on(arch, host), host) {
        (Err(e), _) => Finding::fail(
            "rosetta",
            format!("{which}: {e}"),
            "softwareupdate --install-rosetta --agree-to-license",
        ),
        (Ok(()), catalog::HostArch::AppleSilicon { rosetta: true }) => {
            Finding::ok("rosetta", "installed")
        }
        (Ok(()), _) => Finding::ok("rosetta", format!("not needed by {which}")),
    }
}

// — leftover Wine processes (#103) —

/// One process that belongs to the prefix.
#[derive(Debug, Clone, PartialEq)]
pub struct PrefixProc {
    pub pid: u32,
    pub name: String,
}

fn check_processes(dirs: &GameDirs, state: Option<&GameState>, stop: bool, gogid: &str) -> Finding {
    if !dirs.prefix.is_dir() {
        return Finding::ok("processes", "no prefix yet");
    }
    if let Some(pid) = live_launcher(&dirs.root) {
        return Finding::ok("processes", format!("the game is running (launcher {pid})"));
    }
    let found = prefix_procs(&dirs.prefix);
    if found.is_empty() {
        return Finding::ok("processes", "no Wine processes left in the prefix");
    }
    let list = |procs: &[PrefixProc]| {
        procs
            .iter()
            .map(|p| format!("{} {}", p.pid, p.name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !stop {
        return Finding::warn(
            "processes",
            format!(
                "{} Wine process(es) still running in the prefix with no launcher: {}",
                found.len(),
                list(&found)
            ),
            format!("gogo mac-app doctor {gogid} --stop-leftovers"),
        );
    }
    teardown(dirs, state);
    let left = prefix_procs(&dirs.prefix);
    if left.is_empty() {
        Finding::ok(
            "processes",
            format!(
                "stopped {} leftover Wine process(es): {}",
                found.len(),
                list(&found)
            ),
        )
    } else {
        Finding::fail(
            "processes",
            format!("still running after teardown: {}", list(&left)),
            "kill -KILL <pid> by hand, or log out".to_string(),
        )
    }
}

/// The launcher's teardown (#42, #103), prefix-scoped: `wineserver -k`
/// with the runtime setup last applied, then SIGKILL whatever is left.
fn teardown(dirs: &GameDirs, state: Option<&GameState>) {
    if let Some(rt) = state.and_then(|s| launch::resolve_ref(&s.runtime).ok()) {
        let _ = installer::stop_wineserver(&rt, &dirs.prefix);
    }
    for _ in 0..2 {
        let left = prefix_procs(&dirs.prefix);
        if left.is_empty() {
            return;
        }
        let _ = Command::new("kill")
            .arg("-KILL")
            .args(left.iter().map(|p| p.pid.to_string()))
            .status();
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// The pid of a running launcher of this game (`.launchers/<pid>` holding
/// its start time, as the launcher writes it), if any.
fn live_launcher(game_root: &Path) -> Option<u32> {
    std::fs::read_dir(game_root.join(".launchers"))
        .ok()?
        .flatten()
        .find_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            let start = std::fs::read_to_string(e.path()).ok()?;
            let out = Command::new("ps")
                .args(["-o", "stat=,lstart=", "-p", &pid.to_string()])
                .output()
                .ok()?;
            let line = String::from_utf8_lossy(&out.stdout);
            let (stat, lstart) = line.trim().split_once(' ')?;
            (!stat.starts_with('Z') && lstart.trim() == start.trim()).then_some(pid)
        })
}

/// Processes of this user that belong to `prefix`: its wineserver (cwd in
/// `/tmp/.wine-<uid>/server-<dev>-<ino>`, as the launcher finds it) and any
/// Wine process (name has `wine` or ends in `.exe`) whose cwd is inside the
/// prefix. Never this process.
pub fn prefix_procs(prefix: &Path) -> Vec<PrefixProc> {
    let Ok(canon) = prefix.canonicalize() else {
        return vec![];
    };
    let server = server_dir_suffix(&canon);
    let me = std::process::id();
    let mut out: Vec<PrefixProc> = cwds()
        .into_iter()
        .filter(|(pid, name, cwd)| {
            let lname = name.to_ascii_lowercase();
            let wine = lname.contains("wine") || lname.ends_with(".exe");
            *pid != me
                && (server
                    .as_ref()
                    .is_some_and(|s| cwd.to_string_lossy().ends_with(s.as_str()))
                    || (wine && (cwd.starts_with(&canon) || cwd.starts_with(prefix))))
        })
        .map(|(pid, name, _)| PrefixProc { pid, name })
        .collect();
    out.sort_by_key(|p| p.pid);
    out.dedup();
    out
}

/// `/.wine-<uid>/server-<dev>-<ino>`: the wineserver's cwd for `prefix`.
fn server_dir_suffix(prefix: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    let m = std::fs::metadata(prefix).ok()?;
    let uid = Command::new("id").arg("-u").output().ok()?;
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
    Some(format!("/.wine-{uid}/server-{:x}-{:x}", m.dev(), m.ino()))
}

/// `(pid, name, cwd)` of this user's processes.
#[cfg(target_os = "macos")]
fn cwds() -> Vec<(u32, String, PathBuf)> {
    let Ok(uid) = Command::new("id").arg("-u").output() else {
        return vec![];
    };
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
    let Ok(out) = Command::new("/usr/sbin/lsof")
        .args(["-a", "-d", "cwd", "-u", &uid, "-Fpcn"])
        .output()
    else {
        return vec![];
    };
    let mut procs = Vec::new();
    let (mut pid, mut name) = (0u32, String::new());
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        match line.split_at_checked(1) {
            Some(("p", v)) => pid = v.parse().unwrap_or(0),
            Some(("c", v)) => name = v.to_string(),
            Some(("n", v)) if pid != 0 => procs.push((pid, name.clone(), PathBuf::from(v))),
            _ => {}
        }
    }
    procs
}

#[cfg(not(target_os = "macos"))]
fn cwds() -> Vec<(u32, String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return vec![];
    };
    entries
        .flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            let cwd = std::fs::read_link(e.path().join("cwd")).ok()?;
            let name = std::fs::read_to_string(e.path().join("comm")).ok()?;
            Some((pid, name.trim().to_string(), cwd))
        })
        .collect()
}

// — the last launch (#77) —

fn check_log(dir: &Path) -> Finding {
    let latest = dir.join("latest.log");
    let run = if latest.is_file() {
        Some(latest)
    } else {
        let mut runs: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.ends_with(".log") && n.as_bytes().first().is_some_and(u8::is_ascii_digit)
                })
            })
            .collect();
        runs.sort();
        runs.pop()
    };
    let Some(run) = run else {
        return Finding::ok("log", "no launches yet");
    };
    let Ok(raw) = std::fs::read_to_string(&run) else {
        return Finding::ok("log", format!("{} unreadable", run.display()));
    };
    let field = |k: &str| {
        raw.lines()
            .rev()
            .find_map(|l| l.strip_prefix(&format!("# {k}: ")))
            .map(str::to_string)
    };
    // Wine's output: between the header's and the footer's `# ---`.
    let output: Vec<&str> = raw
        .lines()
        .scan(0, |n, l| {
            if l == "# ---" {
                *n += 1;
                return Some(None);
            }
            Some((*n == 1).then_some(l))
        })
        .flatten()
        .collect();
    let tail: Vec<String> = output[output.len().saturating_sub(5)..]
        .iter()
        .map(|l| l.chars().take(160).collect())
        .collect();
    let started = field("started").unwrap_or_default();
    let shown = std::fs::canonicalize(&run).unwrap_or(run);
    let mut finding = match field("exit") {
        Some(exit) if exit == "0" => Finding::ok("log", format!("last run ({started}) exited 0")),
        Some(exit) => Finding::warn(
            "log",
            format!(
                "last run ({started}) exited {exit} after {}",
                field("duration").unwrap_or_else(|| "?".into())
            ),
            format!("read {}", shown.display()),
        ),
        None => Finding::warn(
            "log",
            format!("last run ({started}) has no exit line: still running, or the launcher died"),
            format!("read {}", shown.display()),
        ),
    };
    if finding.level != Level::Ok {
        finding.detail = tail;
    }
    finding
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_is_lexical() {
        assert_eq!(
            normalize(Path::new("/s/games/1/prefix/drive_c/GOG/../../../game")),
            Path::new("/s/games/1/game")
        );
        assert_eq!(
            normalize(Path::new("/s/p/dosdevices/../drive_c")),
            Path::new("/s/p/drive_c")
        );
    }

    #[test]
    fn log_footer_and_tail() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(check_log(tmp.path()).message, "no launches yet");
        std::fs::write(
            tmp.path().join("20261023-120000-42.log"),
            "# gogo run log: T (1)\n# started: 2026-10-23 12:00:00 +0200\n# ---\n\
             a\nerr:d3d:x\n# ---\n# exit: 3\n# duration: 2s\n",
        )
        .unwrap();
        let f = check_log(tmp.path());
        assert_eq!(f.level, Level::Warn);
        assert_eq!(
            f.message,
            "last run (2026-10-23 12:00:00 +0200) exited 3 after 2s"
        );
        assert_eq!(f.detail, ["a", "err:d3d:x"]);
        std::os::unix::fs::symlink("20261023-120000-42.log", tmp.path().join("latest.log"))
            .unwrap();
        assert_eq!(check_log(tmp.path()).level, Level::Warn);
        std::fs::write(
            tmp.path().join("20261023-130000-43.log"),
            "# started: x\n# ---\n# ---\n# exit: 0\n",
        )
        .unwrap();
        std::fs::remove_file(tmp.path().join("latest.log")).unwrap();
        assert_eq!(check_log(tmp.path()).level, Level::Ok);
    }
}

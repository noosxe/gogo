//! Lutris script interpretation + bounded execution (design 11 §3).
//!
//! The script is JSON (serde_json only). [`plan`] classifies every step into
//! a [`Step`] (applied) or a [`Skipped`] with a reason; [`execute`] runs
//! applied steps through typed executors only — runtime argv, user-installed
//! winetricks argv, std::fs, and the file downloader. There is no escape
//! hatch to a host shell (`execute` steps are skipped by default, design 11
//! §7). All touched paths are canonicalised and must resolve inside the
//! bundle.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::error::MacAppError;
use super::installer::run_with_timeout;
use std::process::Command;
use std::time::Duration;

/// One classified installer step.
#[derive(Debug, Clone)]
pub enum Step {
    /// Harvest args from the script's wineexec-of-the-GOG-setup — gogo already
    /// installed the game, so the setup is not re-run (captures /NOGUI lore).
    HarvestInstallerArgs {
        args: String,
    },
    /// Run an exe through the resolved runtime (patch installers etc.).
    Wineexec {
        exe: PathBuf,
        args: String,
    },
    /// `task: winetricks` — bridged to a user-installed winetricks binary.
    Winetricks {
        verbs: Vec<String>,
    },
    /// `files:` download (id, url).
    Download {
        id: String,
        url: String,
    },
    /// `extract: {file: id-or-path, dst}`.
    Extract {
        file: PathBuf,
        dst: PathBuf,
    },
    /// `merge`/`copy`: src file/dir → dst (src may be a downloaded file id).
    Merge {
        src: PathBuf,
        dst: PathBuf,
    },
    Move {
        src: PathBuf,
        dst: PathBuf,
    },
    WriteFile {
        file: PathBuf,
        data: String,
    },
    /// `write_config`: INI `[section] key=value` merge.
    WriteConfig {
        file: PathBuf,
        section: String,
        values: BTreeMap<String, String>,
    },
    Chmodx {
        file: PathBuf,
    },
    Mkdir {
        dirs: Vec<PathBuf>,
    },
    Registry {
        key: String,
        values: BTreeMap<String, String>,
    },
    Launch {
        exe: Option<String>,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
}

/// A step that will not run, with the reason (rendered to the user).
#[derive(Debug, Clone)]
pub struct Skipped {
    pub index: usize,
    pub verb: String,
    pub reason: String,
}

/// The planned recipe: applied steps (in order) + skipped steps.
#[derive(Debug, Default)]
pub struct RecipePlan {
    pub steps: Vec<(usize, Step)>,
    pub skipped: Vec<Skipped>,
    pub installer_args: Vec<String>,
    pub launch_exe: Option<String>,
    pub launch_args: Vec<String>,
    pub launch_env: BTreeMap<String, String>,
}

impl RecipePlan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
            && self.skipped.is_empty()
            && self.installer_args.is_empty()
            && self.launch_exe.is_none()
            && self.launch_args.is_empty()
            && self.launch_env.is_empty()
    }
}

/// Paths + values the executor and substitution need.
#[derive(Debug, Clone)]
pub struct RecipeCtx {
    pub wine: PathBuf,
    pub prefix: PathBuf,
    pub install_dir: PathBuf,
    pub bundle: PathBuf,
    pub cache: PathBuf,
    pub user: String,
    pub width: u32,
    pub height: u32,
    /// `script.variables` custom substitutions.
    pub vars: BTreeMap<String, String>,
}

impl RecipeCtx {
    /// Substitute Lutris variables (design 11 §3.2). Unknown-variable steps
    /// are the caller's problem: unresolvable strings stay verbatim and the
    /// scope check fails them later.
    pub fn substitute(&self, s: &str) -> String {
        let resolution = format!("{}x{}", self.width, self.height);
        let mut out = s.to_string();
        let pairs: [(&str, String); 9] = [
            ("$GAMEDIR", self.install_dir.to_string_lossy().into_owned()),
            ("$CACHE", self.cache.to_string_lossy().into_owned()),
            ("$PREFIX", self.prefix.to_string_lossy().into_owned()),
            ("$BUNDLE", self.bundle.to_string_lossy().into_owned()),
            ("$USER", self.user.clone()),
            ("$RESOLUTION_WIDTH", self.width.to_string()),
            ("$RESOLUTION_HEIGHT", self.height.to_string()),
            ("$RESOLUTION", resolution),
            ("$WINEBIN", self.wine.to_string_lossy().into_owned()),
        ];
        for (k, v) in &pairs {
            out = out.replace(k, v);
        }
        for (k, v) in &self.vars {
            out = out.replace(&format!("${k}"), v);
        }
        out
    }

    /// Canonicalise and require the path to stay inside the bundle.
    pub fn scope(&self, p: &Path) -> Result<PathBuf, String> {
        let canon = p
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", p.display()))?;
        let bundle = self
            .bundle
            .canonicalize()
            .map_err(|e| format!("cannot resolve bundle: {e}"))?;
        if canon.starts_with(&bundle) {
            Ok(canon)
        } else {
            Err(format!(
                "path escapes the bundle: {} (bundle: {})",
                canon.display(),
                bundle.display()
            ))
        }
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// Single-key-map helper: `files:`/`installer:` entries are `{key: payload}`.
fn single_key(step: &Value) -> Option<(String, &Value)> {
    step.as_object()
        .and_then(|o| o.iter().next().map(|(k, v)| (k.to_string(), v)))
}

/// File ids from `script.files` → URL (or `N/A:` user-prompt marker).
pub fn parse_files(script: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(list) = script.get("files").and_then(Value::as_array) {
        for entry in list {
            if let Some((id, v)) = single_key(entry) {
                out.push((id, s(v)));
            }
        }
    }
    out
}

/// Classify the script into a [`RecipePlan`] (nothing runs yet).
pub fn plan(script: &Value, ctx: &RecipeCtx) -> RecipePlan {
    let mut plan = RecipePlan::default();
    let empty = Vec::new();
    let steps = script
        .get("installer")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    for (idx, raw) in steps.iter().enumerate() {
        let verb = single_key(raw)
            .map(|(k, _)| k)
            .unwrap_or_else(|| "?".into());
        match single_key(raw) {
            Some((k, body)) if k == "task" => {
                let name = body.get("name").and_then(Value::as_str).unwrap_or("");
                match name {
                    "wineexec" => {
                        // CD-based scripts exec from $DISC — the offline flow
                        // has no disc; skip loudly.
                        if body
                            .get("executable")
                            .map(|e| s(e).contains("$DISC"))
                            .unwrap_or(false)
                        {
                            plan.skipped.push(Skipped {
                                index: idx,
                                verb: "task:wineexec".into(),
                                reason: "disc-path executable in the offline flow".into(),
                            });
                            continue;
                        }
                        let args = body
                            .get("args")
                            .map(|a| ctx.substitute(&s(a)))
                            .unwrap_or_default();
                        let exe_ref = body.get("executable").map(s).unwrap_or_default();
                        // A file-id reference = the script's GOG setup (the
                        // corpus's convention); gogo already installed, so the
                        // setup is not re-run — its flags are harvested.
                        let is_file_id = !exe_ref.contains(['/', '\\', '$'])
                            && parse_files(script).iter().any(|(id, _)| *id == exe_ref);
                        if is_file_id {
                            plan.installer_args
                                .extend(args.split_whitespace().map(str::to_string));
                            plan.steps.push((idx, Step::HarvestInstallerArgs { args }));
                        } else {
                            let exe = ctx.substitute(&exe_ref);
                            plan.steps.push((
                                idx,
                                Step::Wineexec {
                                    exe: PathBuf::from(exe),
                                    args,
                                },
                            ));
                        }
                    }
                    "winetricks" => {
                        let verbs = match body.get("app") {
                            Some(Value::String(v)) => vec![ctx.substitute(v)],
                            Some(Value::Array(a)) => {
                                a.iter().map(|v| ctx.substitute(&s(v))).collect()
                            }
                            _ => vec![],
                        };
                        if verbs.is_empty() {
                            plan.skipped.push(Skipped {
                                index: idx,
                                verb: "winetricks".into(),
                                reason: "no app verbs".into(),
                            });
                        } else {
                            plan.steps.push((idx, Step::Winetricks { verbs }));
                        }
                    }
                    _ => plan.skipped.push(Skipped {
                        index: idx,
                        verb: format!("task:{name}"),
                        reason: "unsupported task".into(),
                    }),
                }
            }
            Some((k, body)) if k == "extract" => {
                let dst = body
                    .get("dst")
                    .map(|d| ctx.substitute(&s(d)))
                    .unwrap_or_default();
                let file = body.get("file").map(s).unwrap_or_default();
                plan.steps.push((
                    idx,
                    Step::Extract {
                        file: PathBuf::from(ctx.substitute(&resolve_file(script, &file, ctx))),
                        dst: PathBuf::from(dst),
                    },
                ));
            }
            Some((k, body)) if k == "merge" || k == "copy" => {
                let (src, dst) = two_paths(script, body, ctx);
                plan.steps.push((idx, Step::Merge { src, dst }));
            }
            Some((k, body)) if k == "move" => {
                let (src, dst) = two_paths(script, body, ctx);
                plan.steps.push((idx, Step::Move { src, dst }));
            }
            Some((k, body)) if k == "write_file" => {
                let file =
                    PathBuf::from(ctx.substitute(&body.get("file").map(s).unwrap_or_default()));
                let data = body
                    .get("data")
                    .map(|d| ctx.substitute(&s(d)))
                    .unwrap_or_default();
                plan.steps.push((idx, Step::WriteFile { file, data }));
            }
            Some((k, body)) if k == "write_config" => {
                let file =
                    PathBuf::from(ctx.substitute(&body.get("file").map(s).unwrap_or_default()));
                let section = body
                    .get("data")
                    .and_then(|d| d.as_object())
                    .and_then(|o| o.iter().next())
                    .map(|(sec, vals)| (sec.clone(), vals.clone()));
                match section {
                    Some((sec, vals)) => {
                        let mut map = BTreeMap::new();
                        if let Some(o) = vals.as_object() {
                            for (kk, vv) in o {
                                map.insert(kk.clone(), vv.as_str().unwrap_or_default().to_string());
                            }
                        }
                        plan.steps.push((
                            idx,
                            Step::WriteConfig {
                                file,
                                section: sec,
                                values: map,
                            },
                        ));
                    }
                    None => plan.skipped.push(Skipped {
                        index: idx,
                        verb: "write_config".into(),
                        reason: "no data".into(),
                    }),
                }
            }
            Some((k, body)) if k == "chmodx" => {
                let file = PathBuf::from(ctx.substitute(&s(body)));
                plan.steps.push((idx, Step::Chmodx { file }));
            }
            Some((k, _)) if k == "insert_disc" || k == "insert-disc" || k == "execute" => {
                let reason = if k == "execute" {
                    "host shell step (design 11 §7); --allow-recipe-exec in v1.1".into()
                } else {
                    "offline installer flow has no disc".into()
                };
                plan.skipped.push(Skipped {
                    index: idx,
                    verb: k,
                    reason,
                });
            }
            _ => plan.skipped.push(Skipped {
                index: idx,
                verb,
                reason: "unknown step".into(),
            }),
        }
    }

    // `game:` → entry point + args; `system:` → env; `wine.overrides` → registry.
    if let Some(game) = script.get("game") {
        if let Some(exe) = game.get("exe").and_then(Value::as_str) {
            plan.launch_exe = Some(ctx.substitute(exe));
        }
        if let Some(args) = game.get("args") {
            match args {
                Value::String(a) => plan
                    .launch_args
                    .extend(a.split_whitespace().map(str::to_string)),
                Value::Array(a) => plan
                    .launch_args
                    .extend(a.iter().map(|v| ctx.substitute(&s(v)))),
                _ => {}
            }
        }
    }
    if let Some(env) = script
        .get("system")
        .and_then(|s| s.get("env"))
        .and_then(Value::as_object)
    {
        for (k, v) in env {
            plan.launch_env.insert(k.clone(), ctx.substitute(&s(v)));
        }
    }
    if let Some(over) = script
        .get("wine")
        .and_then(|w| w.get("overrides"))
        .and_then(Value::as_object)
    {
        // DllOverrides registry writes are applied as registry steps (order:
        // before user steps — they are prefix config).
        let key = "HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides".to_string();
        let values: BTreeMap<String, String> =
            over.iter().map(|(k, v)| (k.clone(), s(v))).collect();
        if !values.is_empty() {
            plan.steps
                .insert(0, (usize::MAX, Step::Registry { key, values }));
        }
    }
    plan
}

/// `extract`/`merge` file references may be a `files:` id or a path.
fn resolve_file(script: &Value, id_or_path: &str, ctx: &RecipeCtx) -> String {
    if let Some((_, url)) = parse_files(script)
        .into_iter()
        .find(|(id, _)| id == id_or_path)
    {
        // caller downloads into cache under the id
        let _ = url;
        return ctx.cache.join(id_or_path).to_string_lossy().into_owned();
    }
    ctx.substitute(id_or_path)
}

fn two_paths(script: &Value, body: &Value, ctx: &RecipeCtx) -> (PathBuf, PathBuf) {
    let get = |k: &str| -> String {
        body.get(k)
            .map(s)
            .map(|v| resolve_file(script, &v, ctx))
            .unwrap_or_default()
    };
    (PathBuf::from(get("src")), PathBuf::from(get("dst")))
}

/// Execute applied steps in order. `emit` reports each step; failures abort
/// with `MacAppError::Recipe` (design 11 §6). `files:` downloads are handled
/// by the caller (they precede steps and feed `resolve_file`).
pub struct Executor<'a> {
    pub ctx: &'a RecipeCtx,
    /// Registry/wineexec/winetricks callbacks live on the pipeline side (they
    /// need the runtime); the executor asks via these hooks.
    pub run_wine: &'a mut dyn FnMut(&[String]) -> Result<(), MacAppError>,
    pub run_winetricks: &'a mut dyn FnMut(&[String]) -> Result<bool, MacAppError>,
}

impl Executor<'_> {
    pub fn run(&mut self, plan: &RecipePlan) -> Result<(), MacAppError> {
        for (_idx, step) in &plan.steps {
            match step {
                Step::HarvestInstallerArgs { .. } => {} // applied by the caller pre-install
                Step::Wineexec { exe, args } => {
                    let exe = self
                        .ctx
                        .scope(exe)
                        .map_err(|e| MacAppError::Recipe(format!("wineexec: {e}")))?;
                    let mut argv = vec![exe.to_string_lossy().into_owned()];
                    argv.extend(args.split_whitespace().map(str::to_string));
                    (self.run_wine)(&argv)?;
                }
                Step::Winetricks { verbs } => {
                    // The hook locates the user's winetricks and runs the
                    // verbs; absence is a handled skip (hook emits the
                    // RecipeSkipped with the verb list — design 11 §3.1).
                    (self.run_winetricks)(verbs)?;
                }
                Step::Registry { key, values } => {
                    for (name, value) in values {
                        let argv = vec![
                            "reg.exe".to_string(),
                            "add".to_string(),
                            key.clone(),
                            "/v".to_string(),
                            name.clone(),
                            "/t".to_string(),
                            "REG_SZ".to_string(),
                            "/d".to_string(),
                            value.clone(),
                            "/f".to_string(),
                        ];
                        (self.run_wine)(&argv)?;
                    }
                }
                Step::Extract { file, dst } => {
                    let dst = self
                        .ctx
                        .scope(dst)
                        .map_err(|e| MacAppError::Recipe(format!("extract: {e}")))?;
                    let file = self
                        .ctx
                        .scope(file)
                        .map_err(|e| MacAppError::Recipe(format!("extract: {e}")))?;
                    extract_archive(&file, &dst)?;
                }
                Step::Merge { src, dst } | Step::Move { src, dst } => {
                    let is_move = matches!(step, Step::Move { .. });
                    let src = self.ctx.scope(src).map_err(|e| {
                        MacAppError::Recipe(format!(
                            "{verb}: {e}",
                            verb = if is_move { "move" } else { "merge" }
                        ))
                    })?;
                    let dst = self.ctx.scope(dst).map_err(|e| {
                        MacAppError::Recipe(format!(
                            "{verb}: {e}",
                            verb = if is_move { "move" } else { "merge" }
                        ))
                    })?;
                    if src.is_dir() {
                        copy_dir(&src, &dst)?;
                        if is_move {
                            std::fs::remove_dir_all(&src).ok();
                        }
                    } else {
                        if let Some(parent) = dst.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::copy(&src, &dst)?;
                        if is_move {
                            std::fs::remove_file(&src).ok();
                        }
                    }
                }
                Step::WriteFile { file, data } => {
                    let file = self
                        .ctx
                        .scope(file)
                        .map_err(|e| MacAppError::Recipe(format!("write_file: {e}")))?;
                    if let Some(parent) = file.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(file, data)?;
                }
                Step::WriteConfig {
                    file,
                    section,
                    values,
                } => {
                    let file = self
                        .ctx
                        .scope(file)
                        .map_err(|e| MacAppError::Recipe(format!("write_config: {e}")))?;
                    if let Some(parent) = file.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    write_ini(&file, section, values)?;
                }
                Step::Chmodx { file } => {
                    #[cfg(unix)]
                    {
                        let file = self
                            .ctx
                            .scope(file)
                            .map_err(|e| MacAppError::Recipe(format!("chmodx: {e}")))?;
                        use std::os::unix::fs::PermissionsExt;
                        let mut perm = std::fs::metadata(&file)?.permissions();
                        perm.set_mode(perm.mode() | 0o755);
                        std::fs::set_permissions(&file, perm)?;
                    }
                    #[cfg(not(unix))]
                    let _ = file;
                }
                Step::Mkdir { dirs } => {
                    for d in dirs {
                        let d = self
                            .ctx
                            .scope(d)
                            .map_err(|e| MacAppError::Recipe(format!("mkdir: {e}")))?;
                        std::fs::create_dir_all(d)?;
                    }
                }
                Step::Launch { .. } => {} // applied by the caller via launch.conf
                Step::Download { .. } => {} // applied by the caller before steps
            }
        }
        Ok(())
    }
}

/// zip / tar / tar.gz / tar.xz / 7z (via the shim readers, design 12 D5); rar
pub fn extract_archive(archive: &Path, dst: &Path) -> Result<(), MacAppError> {
    std::fs::create_dir_all(dst)?;
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let lower = name.to_lowercase();
    let err = |what: &str, e: String| MacAppError::Recipe(format!("extract {what}: {e}"));
    if lower.ends_with(".zip") {
        let file = std::fs::File::open(archive).map_err(|e| err("zip", e.to_string()))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| err("zip", e.to_string()))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| err("zip", e.to_string()))?;
            let Some(rel) = entry.enclosed_name() else {
                return Err(err("zip", "unsafe entry path".into()));
            };
            let out = dst.join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&out)?;
            } else {
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out_file = std::fs::File::create(&out)?;
                std::io::copy(&mut entry, &mut out_file)?;
            }
        }
        Ok(())
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        extract_tar(archive, dst, Some(flate2::read::GzDecoder::new))
    } else if lower.ends_with(".tar.xz") {
        extract_tar(archive, dst, Some(xz2::read::XzDecoder::new))
    } else if lower.ends_with(".tar") {
        extract_tar(archive, dst, None::<fn(std::fs::File) -> std::fs::File>)
    } else if lower.ends_with(".7z") {
        gogo_shims::extract_7z(archive, dst, &[]).map_err(|e| err("7z", e))?;
        Ok(())
    } else {
        Err(MacAppError::Recipe(format!(
            "extract: unsupported archive format '{name}' (zip/tar/7z; rar stays v2)"
        )))
    }
}

fn extract_tar<R: std::io::Read + 'static>(
    archive: &Path,
    dst: &Path,
    decode: Option<impl Fn(std::fs::File) -> R>,
) -> Result<(), MacAppError> {
    use std::io::Read as _;
    let file = std::fs::File::open(archive)?;
    let mut reader: Box<dyn std::io::Read> = match decode {
        Some(f) => Box::new(f(file)),
        None => Box::new(file),
    };
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    let mut tar = tar::Archive::new(&buf[..]);
    tar.unpack(dst)
        .map_err(|e| MacAppError::Recipe(format!("extract tar: {e}")))
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else if !ty.is_symlink() {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Minimal INI merge: rewrite `[section]` keys inside an existing file or
/// append the section. Flat files without sections are treated as one
/// unnamed section covering the whole file.
fn write_ini(
    file: &Path,
    section: &str,
    values: &BTreeMap<String, String>,
) -> Result<(), MacAppError> {
    let mut lines: Vec<String> = std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    for (k, v) in values {
        let mut replaced = false;
        let mut in_section = section.is_empty();
        for line in &mut lines {
            let trimmed = line.trim_start();
            if trimmed.starts_with('[') {
                in_section = trimmed.eq_ignore_ascii_case(&format!("[{section}]"));
                continue;
            }
            if in_section
                && trimmed.starts_with(k)
                && trimmed[k.len()..].trim_start().starts_with('=')
            {
                *line = format!("{k}={v}");
                replaced = true;
            }
        }
        if !replaced {
            if !section.is_empty()
                && !lines
                    .iter()
                    .any(|l| l.trim_start().eq_ignore_ascii_case(&format!("[{section}]")))
            {
                lines.push(format!("[{section}]"));
            }
            lines.push(format!("{k}={v}"));
        }
    }
    std::fs::write(file, lines.join("\n") + "\n")?;
    Ok(())
}

/// Wine invocation with the staged prefix (timeout + null stdio, 10 §2).
pub fn wine_command(wine: &Path, prefix: &Path, argv: &[String]) -> Command {
    let mut cmd = Command::new(wine);
    cmd.args(argv)
        .env("WINEPREFIX", prefix)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Run a wine argv inside the prefix with the usual timeout guard.
pub fn run_in_prefix(
    wine: &Path,
    prefix: &Path,
    argv: &[String],
    timeout: Duration,
) -> Result<(), MacAppError> {
    let mut cmd = wine_command(wine, prefix, argv);
    let status = run_with_timeout(&mut cmd, timeout);
    match status {
        Some(s) if s.success() => Ok(()),
        Some(s) => Err(MacAppError::Recipe(format!(
            "step failed: {} exited {s}",
            argv.first().map(String::as_str).unwrap_or("wine")
        ))),
        None => Err(MacAppError::Recipe(format!(
            "step failed: {} timed out",
            argv.first().map(String::as_str).unwrap_or("wine")
        ))),
    }
}

/// `C:\GOG\game\X.exe` → `<prefix>/drive_c/GOG/game/X.exe`.
pub fn windows_to_posix(prefix: &Path, win: &str) -> PathBuf {
    let trimmed = win.trim_start_matches("\\\\?\\");
    let mut parts = trimmed.split(['\\', '/']);
    let drive = parts.next().unwrap_or("C:");
    let letter = drive.chars().next().unwrap_or('c').to_ascii_lowercase();
    let mut out = prefix.join(format!("drive_{letter}"));
    for seg in parts {
        if !seg.is_empty() {
            out.push(seg);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn fixture(name: &str) -> Value {
        // fixtures are the raw API response; the script is results[0]
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap(),
        )
        .unwrap();
        v["results"][0]["script"].clone()
    }

    fn ctx(tmp: &Path) -> RecipeCtx {
        for d in ["Resources/prefix/drive_c/GOG/game", "Resources", "cache"] {
            std::fs::create_dir_all(tmp.join("B.app").join(d)).unwrap();
        }
        RecipeCtx {
            wine: tmp.join("B.app/Resources/wine/bin/wine"),
            prefix: tmp.join("B.app/Resources/prefix"),
            install_dir: tmp.join("B.app/Resources/prefix/drive_c/GOG/game"),
            bundle: tmp.join("B.app"),
            cache: tmp.join("B.app/cache"),
            user: "tester".into(),
            width: 1920,
            height: 1080,
            vars: BTreeMap::new(),
        }
    }

    #[test]
    fn cuphead_harvests_nogui_flags_and_plans_winetricks() {
        let script = fixture("lutris-cuphead-gog.json");
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let plan = plan(&script, &c);
        assert!(
            plan.installer_args.iter().any(|a| a == "/NOGUI"),
            "installer args must harvest /NOGUI: {:?}",
            plan.installer_args
        );
        assert!(
            plan.steps.iter().any(
                |(_, s)| matches!(s, Step::Winetricks { verbs } if verbs == &vec!["corefonts"])
            )
        );
        // cuphead-gog has no wine.overrides — nothing to registry-apply
        assert!(
            !plan
                .steps
                .iter()
                .any(|(_, s)| matches!(s, Step::Registry { .. }))
        );
    }

    #[test]
    fn pharaoh_script_classifies_extract_merge_and_config() {
        let script = fixture("lutris-pharaoh-cd-cnc-ddraw.json");
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let plan = plan(&script, &c);
        assert!(
            plan.steps
                .iter()
                .any(|(_, s)| matches!(s, Step::Extract { .. }))
        );
        assert!(
            plan.steps
                .iter()
                .any(|(_, s)| matches!(s, Step::Merge { .. }))
        );
        assert!(
            plan.steps
                .iter()
                .any(|(_, s)| matches!(s, Step::WriteConfig { section, .. } if section == "ddraw"))
        );
        // launch exe override from game:
        assert!(plan.launch_exe.is_some());
    }

    #[test]
    fn scope_rejects_bundle_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, b"x").unwrap();
        assert!(c.scope(&outside).is_err());
        let inside = c.bundle.join("Resources/prefix/drive_c/GOG/game/f.txt");
        std::fs::write(&inside, b"x").unwrap();
        assert!(c.scope(&inside).is_ok());
    }

    #[test]
    fn zip_extract_is_scoped_and_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        // build a zip with zip crate itself (dev parity with runtime use)
        let src_zip = c.cache.join("t.zip");
        std::fs::create_dir_all(&c.cache).unwrap();
        let file = File::create(&src_zip).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.add_directory("sub/", zip::write::SimpleFileOptions::default())
            .unwrap();
        use std::io::Write as _;
        zip.start_file("sub/a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"hello").unwrap();
        zip.finish().unwrap();
        let dst = c.cache.join("out");
        extract_archive(&src_zip, &dst).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.join("sub/a.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn ini_merge_rewrites_and_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("ddraw.ini");
        std::fs::write(&f, "[ddraw]\nrenderer=opengl\nwidth=640\n[other]\nk=v\n").unwrap();
        let mut values = BTreeMap::new();
        values.insert("renderer".into(), "gdi".into());
        values.insert("windowed".into(), "true".into());
        write_ini(&f, "ddraw", &values).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("renderer=gdi"));
        assert!(text.contains("windowed=true"));
        assert!(text.contains("width=640"));
        assert!(text.contains("k=v"));
    }

    #[test]
    fn unsupported_and_shell_steps_are_skipped_loud() {
        let script: Value = serde_json::json!({
            "installer": [
                {"execute": {"command": "rm -rf /"}},
                {"insert_disc": {"description": "insert cd"}},
                {"task": {"name": "create_prefix", "prefix": "$GAMEDIR"}},
                {"mystery_step": {}}
            ]
        });
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let plan = plan(&script, &c);
        assert_eq!(plan.steps.len(), 0);
        let reasons: Vec<&str> = plan.skipped.iter().map(|s| s.verb.as_str()).collect();
        assert!(reasons.contains(&"execute"));
        assert!(reasons.contains(&"insert_disc"));
        assert!(reasons.contains(&"task:create_prefix"));
        assert!(reasons.contains(&"mystery_step"));
    }

    #[test]
    fn executor_runs_file_ops_and_reports_registry_argv() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        // src file inside bundle; dst next to it
        let src = c.cache.join("patched.exe");
        std::fs::create_dir_all(&c.cache).unwrap();
        std::fs::write(&src, b"new-exe").unwrap();
        let game_file = c.install_dir.join("Game.exe");
        std::fs::write(&game_file, b"old-exe").unwrap();
        let plan = RecipePlan {
            steps: vec![(
                0,
                Step::Merge {
                    src: src.clone(),
                    dst: game_file.clone(),
                },
            )],
            ..Default::default()
        };
        let mut reg_argv_log: Vec<Vec<String>> = Vec::new();
        let mut run_wine = |argv: &[String]| -> Result<(), MacAppError> {
            reg_argv_log.push(argv.to_vec());
            Ok(())
        };
        let mut run_winetricks = |_: &[String]| -> Result<bool, MacAppError> { Ok(true) };
        Executor {
            ctx: &c,
            run_wine: &mut run_wine,
            run_winetricks: &mut run_winetricks,
        }
        .run(&plan)
        .unwrap();
        assert_eq!(std::fs::read(&game_file).unwrap(), b"new-exe");
        assert!(reg_argv_log.is_empty());
    }
}

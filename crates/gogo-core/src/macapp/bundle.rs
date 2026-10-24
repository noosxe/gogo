//! `.app` bundle assembly (`docs/design/10` §2.5–2.7): layout, Info.plist,
//! launcher stub, `launch.conf`, licensing notices, runtime copy, ad-hoc
//! codesign. Hand-rolled plist XML — no new dependency (design 10 §2.6).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::error::MacAppError;
use super::runtime::Runtime;

/// Layout of one generated bundle.
#[derive(Debug, Clone)]
pub struct Bundle {
    /// `<out>/<display>.app`
    pub root: PathBuf,
    pub contents: PathBuf,
    pub macos: PathBuf,
    pub resources: PathBuf,
    pub prefix: PathBuf,
}

/// Compute all paths for a bundle named `display` under `out_dir`.
pub fn layout(out_dir: &Path, display: &str) -> Bundle {
    let root = out_dir.join(format!("{display}.app"));
    let contents = root.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    Bundle {
        prefix: resources.join("prefix"),
        root,
        contents,
        macos,
        resources,
    }
}

/// Create the directory skeleton; refuse to clobber an existing bundle.
pub fn create_layout(bundle: &Bundle) -> Result<(), MacAppError> {
    if bundle.root.exists() {
        return Err(MacAppError::BundleExists {
            path: bundle.root.clone(),
        });
    }
    std::fs::create_dir_all(&bundle.macos)?;
    std::fs::create_dir_all(&bundle.resources)?;
    Ok(())
}

/// Escape XML text content / attribute values (the five predefined entities).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `Contents/Info.plist` (design 10 §2.6). `min_macos` is omitted when the
/// runtime has no requirement — an absent key is weaker than a wrong one.
pub fn write_info_plist(
    bundle: &Bundle,
    display: &str,
    slug: &str,
    min_macos: Option<&str>,
) -> Result<(), MacAppError> {
    let key = |k: &str| format!("  <key>{k}</key>\n");
    let val = |v: &str| format!("  <string>{}</string>\n", xml_escape(v));
    let mut plist = String::new();
    plist.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    plist.push_str(
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    );
    plist.push_str("<plist version=\"1.0\">\n<dict>\n");
    plist.push_str(&key("CFBundleIdentifier"));
    plist.push_str(&val(&format!("com.noosxe.gogo.{slug}")));
    plist.push_str(&key("CFBundleName"));
    plist.push_str(&val(display));
    plist.push_str(&key("CFBundleExecutable"));
    plist.push_str(&val(display));
    plist.push_str(&key("CFBundlePackageType"));
    plist.push_str("  <string>APPL</string>\n");
    plist.push_str(&key("CFBundleShortVersionString"));
    plist.push_str("  <string>1.0.0</string>\n");
    plist.push_str(&key("CFBundleVersion"));
    plist.push_str("  <string>1</string>\n");
    if let Some(min) = min_macos {
        plist.push_str(&key("LSMinimumSystemVersion"));
        plist.push_str(&val(min));
    }
    plist.push_str(&key("NSHighResolutionCapable"));
    plist.push_str("  <true/>\n");
    plist.push_str("</dict>\n</plist>\n");

    std::fs::write(bundle.contents.join("Info.plist"), plist)?;
    Ok(())
}

/// The renderer's licence notice (`LICENSE-<id>.txt`), written like the
/// runtime's: v2 bundles carry no renderer files, so the notice says where
/// they come from. wined3d is part of Wine (`LICENSE-wine.txt`).
pub fn write_renderer_notice(bundle: &Bundle, renderer: &str) -> Result<(), MacAppError> {
    renderer_notice(bundle, renderer, false)
}

/// A portable bundle's (#84) renderer notice: its files are inside it.
pub fn write_bundled_renderer_notice(bundle: &Bundle, renderer: &str) -> Result<(), MacAppError> {
    renderer_notice(bundle, renderer, true)
}

fn renderer_notice(bundle: &Bundle, renderer: &str, bundled: bool) -> Result<(), MacAppError> {
    let Ok(r) = super::renderer::RendererRef::parse(renderer) else {
        return Ok(());
    };
    let Some(entry) = super::catalog::find(r.renderer.id()) else {
        return Ok(());
    };
    let source = match &entry.discovery {
        Some(super::catalog::Discovery::Pinned(pin)) => format!(
            "https://github.com/{}/releases/tag/{} ({})",
            pin.repo, pin.tag, pin.asset
        ),
        _ => String::new(),
    };
    let notice = match entry.kind {
        super::catalog::Kind::Renderer {
            notice: Some(n), ..
        } => format!("\n{n}"),
        _ => String::new(),
    };
    let text = if bundled {
        format!(
            "This application bundles {} {}, licensed {}.\n\
             Source: {source}\n\
             Downloaded by `gogo mac-app` on the machine that built this bundle; gogo\n\
             itself ships none.\n\
             {notice}",
            entry.id,
            r.version().unwrap_or_default(),
            entry.license,
        )
    } else {
        format!(
            "This application runs {} {}, licensed {}, from gogo's local renderer\n\
             store on this Mac. It contains no {} files itself.\n\
             Source: {source}\n\
             Downloaded by `gogo mac-app` on the user's machine; gogo itself ships none.\n\
             {notice}",
            entry.id,
            r.version().unwrap_or_default(),
            entry.license,
            entry.id,
        )
    };
    std::fs::write(
        bundle.resources.join(format!("LICENSE-{}.txt", entry.id)),
        text,
    )?;
    Ok(())
}

/// Licensing notices inside the bundle (design 10 §8): Wine LGPL pointer for
/// every bundle; an extra GPTK EULA warning when the runtime came from a
/// user-supplied GPTK mount.
pub fn write_notices(bundle: &Bundle, runtime: &Runtime) -> Result<(), MacAppError> {
    write_wine_notice(bundle, runtime, true)
}

/// v2 launcher bundles (design 16) carry no Wine: the notice says where the
/// runtime it runs comes from instead.
pub fn write_launcher_notices(bundle: &Bundle, runtime: &Runtime) -> Result<(), MacAppError> {
    write_wine_notice(bundle, runtime, false)
}

fn write_wine_notice(bundle: &Bundle, runtime: &Runtime, bundled: bool) -> Result<(), MacAppError> {
    // Store entries with a Frameworks set carry per-dylib licence notes (#62).
    let notices = runtime.root.join("NOTICES.txt");
    if notices.is_file() {
        std::fs::copy(&notices, bundle.resources.join("LICENSE-runtime.txt"))?;
    }
    let source = if runtime.is_crossover() {
        let build = runtime.crossover_build().unwrap_or("Sikarugir-App/Engines");
        format!(
            "Source: CrossOver's Wine sources, https://www.codeweavers.com/crossover/source\n\
             (engine build: https://github.com/{build})"
        )
    } else {
        "Source: https://github.com/wine-mirror/wine".to_string()
    };
    if !bundled {
        std::fs::write(
            bundle.resources.join("LICENSE-wine.txt"),
            format!(
                "This application runs Wine ({}), licensed under the GNU LGPL v2.1, from\n\
                 gogo's local runtime store on this Mac. It contains no Wine binaries itself.\n\
                 {source}\n\
                 Wine is copyright the Wine project authors. Built by `gogo mac-app` on the\n\
                 user's machine; gogo itself ships no Wine binaries.\n",
                runtime.version
            ),
        )?;
        if runtime.mode == super::runtime::RuntimeMode::Gptk {
            std::fs::write(
                bundle.resources.join("README-GPTK.txt"),
                "This application runs Apple's Game Porting Toolkit Wine runtime from the\n\
                 user's own copy, under Apple's Game Porting Toolkit license. It embeds none\n\
                 of it. gogo never downloads or ships the GPTK itself.\n",
            )?;
        }
        return Ok(());
    }
    std::fs::write(
        bundle.resources.join("LICENSE-wine.txt"),
        format!(
            "This application bundles Wine ({}), licensed under the GNU LGPL v2.1.\n\
             {source}\n\
             Wine is copyright the Wine project authors. This bundle was produced by\n\
             `gogo mac-app` on the user's machine from the user's own runtime copy;\n\
             gogo itself ships no Wine binaries.\n",
            runtime.version
        ),
    )?;
    if runtime.mode == super::runtime::RuntimeMode::Gptk {
        std::fs::write(
            bundle.resources.join("README-GPTK.txt"),
            "This application embeds Apple's Game Porting Toolkit Wine runtime.\n\
             Apple's Game Porting Toolkit license applies to that embedded copy;\n\
             it was downloaded by the user from Apple under the user's own terms.\n\
             Redistributing this bundle embeds Apple-licensed software — do so at\n\
             your own risk. gogo never downloads or ships the GPTK itself.\n",
        )?;
    }
    Ok(())
}

/// `LICENSE-winetricks.txt` — written when recipe verbs ran through
/// winetricks (design 12 §8): the tool is LGPL-2.1+; components it installs
/// were fetched during conversion into the user's prefix — gogo ships none.
pub fn write_winetricks_notice(bundle: &Bundle) -> Result<(), MacAppError> {
    std::fs::write(
        bundle.resources.join("LICENSE-winetricks.txt"),
        "This application's game prefix was configured with winetricks\n\
         (https://github.com/Winetricks/winetricks), licensed under the GNU\n\
         LGPL v2.1-or-later. Components it installs (fonts, runtimes) were\n\
         downloaded during conversion on the user's machine and live in the\n\
         user's prefix; gogo itself ships no such components.\n",
    )?;
    Ok(())
}

/// `LICENSE-verbs.txt` for a portable bundle (#84): the files its winetricks
/// verbs installed at build (Microsoft's redistributable DirectX and
/// runtime DLLs, fonts) are inside it, and its launcher copies them into
/// the game's prefix. They keep their own vendors' terms.
pub fn write_verbs_notice(bundle: &Bundle, verbs: &[String]) -> Result<(), MacAppError> {
    std::fs::write(
        bundle.resources.join("LICENSE-verbs.txt"),
        format!(
            "This application carries the files these winetricks verbs installed when it\n\
             was built: {}.\n\
             They are redistributable components (Microsoft DirectX and runtime DLLs,\n\
             fonts) under their vendors' own licence terms, downloaded by winetricks\n\
             (https://github.com/Winetricks/winetricks) on the machine that built this\n\
             bundle, and copied into the game's Wine prefix on first launch. gogo itself\n\
             ships none of them.\n",
            verbs.join(", ")
        ),
    )?;
    Ok(())
}

/// Ad-hoc codesign argv (`codesign --force --deep --sign - <bundle>`) — split
/// out for unit testing; [`codesign_and_verify`] executes + verifies.
pub fn codesign_argv(bundle: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "codesign".into(),
        "--force".into(),
        "--deep".into(),
        "--sign".into(),
        "-".into(),
        bundle.as_os_str().to_os_string(),
    ]
}

/// Codesign behavior for one conversion (design 10 §2.7). macOS-only tooling:
/// tests (and non-mac hosts) use [`CodesignPolicy::Skip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodesignPolicy {
    /// `codesign --force --deep --sign -` + `--verify`.
    Adhoc,
    /// Test/dry mode: no codesign call at all.
    Skip,
}

/// Sign ad-hoc, then verify. `strict` adds `--deep --strict` (every bundle
/// since design 16: a bundle that fails it is a hard error, so #53 can't come
/// back silently).
pub fn codesign_and_verify(
    bundle: &Path,
    policy: CodesignPolicy,
    strict: bool,
) -> Result<(), MacAppError> {
    if policy == CodesignPolicy::Skip {
        return Ok(());
    }
    let args = codesign_argv(bundle);
    let out = std::process::Command::new(&args[0])
        .args(&args[1..])
        .output()
        .map_err(|e| MacAppError::Codesign {
            stderr: format!("cannot execute codesign: {e}"),
        })?;
    if !out.status.success() {
        return Err(MacAppError::Codesign {
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    let mut verify = std::process::Command::new("codesign");
    verify.arg("--verify");
    if strict {
        verify.args(["--deep", "--strict"]);
    }
    let verify = verify
        .arg(bundle)
        .output()
        .map_err(|e| MacAppError::Codesign {
            stderr: format!("cannot execute codesign: {e}"),
        })?;
    if !verify.status.success() {
        return Err(MacAppError::Codesign {
            stderr: format!(
                "verify failed after signing: {}",
                String::from_utf8_lossy(&verify.stderr).trim()
            ),
        });
    }
    Ok(())
}

/// Strip macOS metadata "detritus" (resource forks, FinderInfo, provenance,
/// quarantine) from every file in the bundle — `codesign` refuses to seal a
/// bundle whose files carry them (live finding 2026-10-18, GPTK runtime files
/// copied from a quarantined app). No-op when codesign is skipped. Best-effort
/// on hosts without `/usr/bin/xattr` (non-mac).
pub fn strip_detritus(bundle: &Path, policy: CodesignPolicy) -> Result<(), MacAppError> {
    if policy == CodesignPolicy::Skip || !Path::new("/usr/bin/xattr").exists() {
        return Ok(());
    }
    // Delete only the seal-breaking classes. NOT `xattr -cr`: it also targets
    // SIP-protected com.apple.provenance, which cannot be removed (exit 1) and
    // which codesign tolerates anyway (live finding 2026-10-18, third run).
    for attr in [
        "com.apple.FinderInfo",
        "com.apple.ResourceFork",
        "com.apple.quarantine",
    ] {
        let status = super::installer::run_with_timeout(
            Command::new("/usr/bin/xattr")
                .args(["-dr", attr])
                .arg(bundle),
            Duration::from_secs(120),
        );
        if status.is_none() {
            return Err(MacAppError::Codesign {
                stderr: format!("xattr -dr {attr} timed out — bundle metadata not cleared"),
            });
        }
    }
    Ok(())
}

/// Clone a game tree into the bundle (design 15 D4). Tries APFS
/// copy-on-write first (`cp -c` → clonefile(2)) — instant and space-free
/// until divergence — and falls back to a plain recursive copy for other
/// filesystems. `dst` must not exist yet; parents are created.
pub fn clone_tree(src: &Path, dst: &Path) -> Result<(), MacAppError> {
    if !src.is_dir() {
        return Err(MacAppError::Io(std::io::Error::other(format!(
            "game tree {} is not a directory",
            src.display()
        ))));
    }
    if dst.exists() {
        return Err(MacAppError::Io(std::io::Error::other(format!(
            "clone destination {} already exists",
            dst.display()
        ))));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(target_os = "macos")]
    {
        if std::process::Command::new("/bin/cp")
            .arg("-cRp")
            .arg(src)
            .arg(dst)
            .status()?
            .success()
        {
            return Ok(());
        }
    }
    let plain = std::process::Command::new("/bin/cp")
        .arg("-Rp")
        .arg(src)
        .arg(dst)
        .status()?;
    if plain.success() {
        Ok(())
    } else {
        Err(MacAppError::Io(std::io::Error::other(format!(
            "copying game tree {} -> {} failed",
            src.display(),
            dst.display()
        ))))
    }
}

/// Merge an overlay tree over `dst`, overlay files winning (design 15 §3:
/// the depot `gog-support/<id>/<id>/app/` files Galaxy merges into the
/// install dir). Missing directories are created; symlinks in the overlay
/// are skipped. Returns the merged files relative to `overlay`, sorted.
pub fn merge_overlay(overlay: &Path, dst: &Path) -> Result<Vec<PathBuf>, MacAppError> {
    fn walk(
        root: &Path,
        dir: &Path,
        dst: &Path,
        out: &mut Vec<PathBuf>,
    ) -> Result<(), MacAppError> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            let target = dst.join(&rel);
            if kind.is_dir() {
                std::fs::create_dir_all(&target)?;
                walk(root, &path, dst, out)?;
            } else if kind.is_file() {
                // Replace rather than write through: the target is usually an
                // APFS clone, possibly read-only, of a depot cache file.
                if target.symlink_metadata().is_ok() {
                    std::fs::remove_file(&target)?;
                }
                std::fs::copy(&path, &target)?;
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut merged = Vec::new();
    walk(overlay, overlay, dst, &mut merged)?;
    merged.sort();
    Ok(merged)
}

#[cfg(test)]
mod bundle_tests {
    use super::*;
    use crate::macapp::runtime::RuntimeMode;

    fn runtime_stub(mode: RuntimeMode) -> Runtime {
        let kind = mode.as_str();
        Runtime {
            mode,
            kind,
            root: PathBuf::from("/tmp/rt"),
            wine: PathBuf::from("/tmp/rt/bin/wine"),
            version: "wine-9.0".into(),
            min_macos: None,
        }
    }

    #[test]
    fn plist_carries_identity_and_optional_min_macos() {
        let tmp = tempfile::tempdir().unwrap();
        let b = layout(tmp.path(), "Heroes3 gold");
        create_layout(&b).unwrap();
        write_info_plist(&b, "Heroes3 gold", "heroes3-gold", Some("14.0")).unwrap();
        let plist = std::fs::read_to_string(b.contents.join("Info.plist")).unwrap();
        assert!(plist.contains("<key>CFBundleIdentifier</key>"));
        assert!(plist.contains("<string>com.noosxe.gogo.heroes3-gold</string>"));
        assert!(plist.contains("<string>Heroes3 gold</string>"));
        assert!(plist.contains("<string>14.0</string>"));
        assert!(plist.contains("<key>NSHighResolutionCapable</key>"));
        assert!(plist.contains("<dict>"));
    }

    #[test]
    fn plist_omits_min_macos_when_none() {
        let tmp = tempfile::tempdir().unwrap();
        let b = layout(tmp.path(), "G");
        create_layout(&b).unwrap();
        write_info_plist(&b, "G", "g", None).unwrap();
        let plist = std::fs::read_to_string(b.contents.join("Info.plist")).unwrap();
        assert!(!plist.contains("LSMinimumSystemVersion"));
    }

    #[test]
    fn plist_escapes_xml_entities() {
        let tmp = tempfile::tempdir().unwrap();
        let b = layout(tmp.path(), "A & B <test>");
        create_layout(&b).unwrap();
        write_info_plist(&b, "A & B <test>", "a-b-test", None).unwrap();
        let plist = std::fs::read_to_string(b.contents.join("Info.plist")).unwrap();
        assert!(plist.contains("A &amp; B &lt;test&gt;"));
        assert!(!plist.contains("A & B <"));
    }

    #[test]
    fn create_layout_refuses_existing_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let b = layout(tmp.path(), "G");
        create_layout(&b).unwrap();
        let err = create_layout(&b).unwrap_err();
        assert!(matches!(err, MacAppError::BundleExists { .. }));
    }

    #[test]
    fn notices_switch_on_runtime_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let b = layout(tmp.path(), "G");
        create_layout(&b).unwrap();
        let mut rt = runtime_stub(RuntimeMode::Brew);
        write_notices(&b, &rt).unwrap();
        assert!(b.resources.join("LICENSE-wine.txt").is_file());
        assert!(!b.resources.join("README-GPTK.txt").exists());

        rt.mode = RuntimeMode::Gptk;
        write_notices(&b, &rt).unwrap();
        assert!(b.resources.join("README-GPTK.txt").is_file());
    }

    #[test]
    fn codesign_argv_shape() {
        let args = codesign_argv(Path::new("/out/G.app"));
        let strs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
        assert_eq!(
            strs,
            ["codesign", "--force", "--deep", "--sign", "-", "/out/G.app"]
        );
    }

    #[test]
    fn merge_overlay_adds_and_replaces_files() {
        let tmp = tempfile::tempdir().unwrap();
        let overlay = tmp.path().join("app");
        std::fs::create_dir_all(overlay.join("ASSETS")).unwrap();
        std::fs::write(overlay.join("ASSETS/options.txt"), b"overlay").unwrap();
        std::fs::write(overlay.join("config.ini"), b"new").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/nowhere", overlay.join("link")).unwrap();

        let game = tmp.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        let old = game.join("config.ini");
        std::fs::write(&old, b"old").unwrap();
        let mut perms = std::fs::metadata(&old).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&old, perms).unwrap();

        let merged = merge_overlay(&overlay, &game).unwrap();
        assert_eq!(
            merged,
            vec![
                PathBuf::from("ASSETS/options.txt"),
                PathBuf::from("config.ini")
            ]
        );
        assert_eq!(
            std::fs::read(game.join("ASSETS/options.txt")).unwrap(),
            b"overlay"
        );
        assert_eq!(std::fs::read(&old).unwrap(), b"new", "overlay wins");
        assert!(!game.join("link").exists(), "symlinks are skipped");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn strip_detritus_clears_file_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let b = layout(tmp.path(), "G");
        create_layout(&b).unwrap();
        let f = b.resources.join("x.bin");
        std::fs::write(&f, b"data").unwrap();
        let set = std::process::Command::new("/usr/bin/xattr")
            .args(["-wx", "com.apple.ResourceFork", "41424344"])
            .arg(&f)
            .output()
            .unwrap();
        assert!(set.status.success());
        strip_detritus(&b.root, CodesignPolicy::Adhoc).unwrap();
        let out = std::process::Command::new("/usr/bin/xattr")
            .arg("-l")
            .arg(&f)
            .output()
            .unwrap();
        let list = String::from_utf8_lossy(&out.stdout);
        // SIP-protected com.apple.provenance is auto-re-added and codesign
        // tolerates it — the seal-breaking classes must be gone.
        assert!(!list.contains("ResourceFork"), "detritus remains: {list}");
        assert!(!list.contains("FinderInfo"), "detritus remains: {list}");
        assert!(!list.contains("ResourceFork"), "detritus remains: {list}");
    }
}

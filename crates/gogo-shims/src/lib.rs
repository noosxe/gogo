//! gogo-shims — Rust-made extractor shims (design 12 D5): `cabextract` and
//! `7z` CLI-compatible tools, exposed as a library (recipe `extract` reuses
//! the same readers) and via [`run_tool`] behind the CLI's hidden `__shim`
//! subcommand. gogo-core materializes tiny re-exec scripts that invoke them
//! and PATH-scopes them to winetricks invocations — users install nothing
//! (docs/15 §3: cabextract/7z are absent on stock macOS and pervasive in
//! winetricks verbs).
//!
//! CLI compatibility is derived from what winetricks actually invokes
//! (docs/15 §8.5): `cabextract [-q] [-L] [-d DIR|--directory=DIR]
//! [-F PAT|--filter=PAT] FILE…` with flags in varying order and FILEs that
//! are often self-extracting exes with embedded MSCF cabinets; `7z x FILE
//! -oDIR [-y] [FILENAMES…]`.
//!
//! Licenses (docs/15 §8.6): this crate is gogo code (MIT OR Apache-2.0);
//! readers are `cab` 0.6 (MIT, active 2026-08) and `sevenz-rust2` 0.24 (maintained fork; upstream sevenz-rust is RUSTSEC-2026-0246 unmaintained)
//! (Apache-2.0, pure Rust).

use std::path::{Path, PathBuf};

/// Extract cabinets from `file` — a bare `.cab` or a self-extracting `.exe`
/// with embedded MSCF cabinets (how `corefonts`/`vcrun*` payloads ship).
/// `filter` is a cabextract `-F` glob (`*`, `?`); `lowercase` mirrors `-L`.
/// Returns the number of files written (cabextract exits 1 when zero).
pub fn extract_cab(
    file: &Path,
    dest: &Path,
    filter: Option<&str>,
    lowercase: bool,
) -> Result<usize, String> {
    let data = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut written = 0usize;
    let mut any_cab = false;
    for offset in mscf_offsets(&data) {
        let slice = std::io::Cursor::new(&data[offset..]);
        let mut cabinet = match cab::Cabinet::new(slice) {
            Ok(c) => c,
            Err(_) => continue, // not a real cabinet — keep scanning
        };
        any_cab = true;
        // Collect names first: read_file needs the mutable borrow back.
        let names: Vec<String> = cabinet
            .folder_entries()
            .flat_map(|folder| folder.file_entries())
            .map(|entry| entry.name().to_string())
            .collect();
        for name in names {
            let norm = name.replace('\\', "/");
            let norm = norm.strip_prefix("./").unwrap_or(&norm);
            let matched = match filter {
                Some(f) => {
                    glob_match(f, norm) || glob_match(f, norm.rsplit('/').next().unwrap_or(norm))
                }
                None => true,
            };
            if !matched {
                continue;
            }
            let rel = if lowercase {
                norm.to_lowercase()
            } else {
                norm.to_string()
            };
            let out = dest.join(&rel);
            if !out.starts_with(dest) {
                return Err(format!("unsafe cabinet entry path: {name}"));
            }
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            let mut reader = cabinet
                .read_file(&name)
                .map_err(|e| format!("{name}: {e}"))?;
            let mut out_file =
                std::fs::File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            std::io::copy(&mut reader, &mut out_file).map_err(|e| format!("{name}: {e}"))?;
            written += 1;
        }
    }
    if !any_cab {
        return Err(format!(
            "{}: no cabinet found (not a .cab / SFX?)",
            file.display()
        ));
    }
    Ok(written)
}

/// Extract a 7z archive into `dest`. `only` filters entries by archive path
/// or basename (the `w_try_7z FILENAMES…` subset); empty extracts everything.
pub fn extract_7z(file: &Path, dest: &Path, only: &[String]) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    if only.is_empty() {
        sevenz_rust2::decompress_file(file, dest).map_err(|e| e.to_string())?;
        return Ok(());
    }
    // The reader API extracts whole archives; extract to a scratch dir and
    // move the matches, preserving their archive-relative paths.
    let scratch = dest.join(".gogo-7z-scratch");
    let _ = std::fs::remove_dir_all(&scratch);
    sevenz_rust2::decompress_file(file, &scratch).map_err(|e| e.to_string())?;
    let (mut moved_any, res) = walk_move_matches(&scratch, dest, only);
    let _ = &mut moved_any;
    let cleanup = std::fs::remove_dir_all(&scratch);
    res?;
    if !moved_any {
        cleanup.map_err(|e| format!("scratch cleanup: {e}"))?;
        return Err(format!("no entries matched {:?}", only));
    }
    Ok(())
}

fn walk_move_matches(scratch: &Path, dest: &Path, only: &[String]) -> (bool, Result<(), String>) {
    let mut moved = false;
    let mut stack = vec![scratch.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => return (moved, Err(format!("{}: {e}", dir.display()))),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(scratch) else {
                continue;
            };
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let base = rel_str.rsplit('/').next().unwrap_or(&rel_str).to_string();
            let hit = only
                .iter()
                .any(|f| f == &rel_str || f == &base || glob_match(f, &rel_str));
            if !hit {
                continue;
            }
            let out = dest.join(rel);
            if !out.starts_with(dest) {
                return (moved, Err(format!("unsafe 7z entry path: {rel_str}")));
            }
            if let Some(parent) = out.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                return (moved, Err(format!("{}: {e}", parent.display())));
            }
            if let Err(e) = std::fs::rename(&path, &out) {
                return (moved, Err(format!("{}: {e}", path.display())));
            }
            moved = true;
        }
    }
    (moved, Ok(()))
}

/// Offsets of plausible embedded cabinets: every `MSCF` signature; invalid
/// candidates are rejected by the parser on read.
fn mscf_offsets(data: &[u8]) -> Vec<usize> {
    data.windows(4)
        .enumerate()
        .filter(|(_, w)| w == b"MSCF")
        .map(|(i, _)| i)
        .collect()
}

/// fnmatch-style glob (`*` any sequence, `?` one char), case-sensitive — the
/// cabextract `-F` subset winetricks uses (`*MDX*`, `msvcp140.dll`).
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut star_t) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = pi;
            pi += 1;
            star_t = ti;
        } else if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Dispatch a shim CLI (`gogo __shim <tool> [args…]`). Returns the exit code:
/// 0 ok, 1 nothing extracted / failure, 2 usage — mirroring upstream tools.
pub fn run_tool(tool: &str, args: &[String]) -> i32 {
    match tool {
        "cabextract" => run_cabextract(args),
        "7z" => run_7z(args),
        other => {
            eprintln!("gogo-shims: unknown tool {other:?}");
            2
        }
    }
}

fn usage_err(msg: &str) -> i32 {
    eprintln!("gogo-shims: {msg}");
    2
}

fn run_cabextract(args: &[String]) -> i32 {
    let mut dest = PathBuf::from(".");
    let mut filter: Option<String> = None;
    let mut lowercase = false;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-q" | "--quiet" | "-l" | "--list" => {} // tolerated, no-op
            "-L" | "--lowercase" => lowercase = true,
            "-d" | "--directory" => match it.next() {
                Some(d) => dest = PathBuf::from(d),
                None => return usage_err("cabextract: -d needs a directory"),
            },
            "-F" | "--filter" => match it.next() {
                Some(f) => filter = Some(f.clone()),
                None => return usage_err("cabextract: -F needs a pattern"),
            },
            _ if arg.starts_with("--directory=") => {
                dest = PathBuf::from(&arg["--directory=".len()..]);
            }
            _ if arg.starts_with("--filter=") => {
                filter = Some(arg["--filter=".len()..].to_string());
            }
            "--version" | "-h" | "--help" => {
                println!("gogo-shims cabextract (cab crate, MSCF + embedded SFX)");
                return 0;
            }
            _ if arg.starts_with('-') => {} // tolerate unrelated flags
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if files.is_empty() {
        return usage_err("cabextract: no archive given");
    }
    let mut total = 0usize;
    for file in &files {
        match extract_cab(file, &dest, filter.as_deref(), lowercase) {
            Ok(n) => total += n,
            Err(e) => {
                eprintln!("gogo-shims: {e}");
                return 1;
            }
        }
    }
    if total == 0 {
        eprintln!("gogo-shims: no files extracted");
        return 1;
    }
    0
}

fn run_7z(args: &[String]) -> i32 {
    let mut dest = PathBuf::from(".");
    let mut subcmd: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-y" | "-bso0" | "-bse0" | "-bsp0" | "-sns" => {}
            "-o" => match it.next() {
                Some(d) => dest = PathBuf::from(d),
                None => return usage_err("7z: -o needs a directory"),
            },
            _ if arg.starts_with("-o") => dest = PathBuf::from(&arg[2..]),
            _ if arg.starts_with('-') => {}
            _ => {
                if subcmd.is_none() {
                    subcmd = Some(arg.clone());
                } else {
                    positional.push(arg.clone());
                }
            }
        }
    }
    match subcmd.as_deref() {
        Some("x") | Some("e") => {}
        Some(other) => return usage_err(&format!("7z: unsupported subcommand {other:?}")),
        None => return usage_err("7z: missing subcommand"),
    }
    let Some(archive) = positional.first() else {
        return usage_err("7z: no archive given");
    };
    let only: Vec<String> = positional[1..].to_vec();
    match extract_7z(Path::new(archive), &dest, &only) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("gogo-shims: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// A real cabinet built with the `cab` write API: two files.
    fn build_cab(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = cab::CabinetBuilder::new();
        {
            let folder = builder.add_folder(cab::CompressionType::None);
            for (name, _) in files {
                folder.add_file(name.to_string());
            }
        }
        let mut cursor = std::io::Cursor::new(Vec::new());
        let mut writer = builder.build(&mut cursor).unwrap();
        for (_, data) in files {
            let mut fw = writer.next_file().unwrap().unwrap();
            fw.write_all(data).unwrap();
        }
        writer.finish().unwrap();
        cursor.into_inner()
    }

    fn build_7z(dir: &Path) -> PathBuf {
        let archive = dir.join("fixture.7z");
        sevenz_rust2::compress_to_path(dir.join("payload"), &archive).unwrap();
        archive
    }

    #[test]
    fn glob_match_cases() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*MDX*", "MDX1_x86.cab"));
        assert!(glob_match("*.dll", "d3dx9_43.dll"));
        assert!(!glob_match("*.dll", "d3dx9_43.exe"));
        assert!(glob_match("msvcp140.dll", "msvcp140.dll"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "abbc"));
        assert!(glob_match("a*c", "abbbbc"));
    }

    #[test]
    fn cab_round_trip_with_filter_and_lowercase() {
        let tmp = tempfile::tempdir().unwrap();
        let cab = build_cab(&[
            ("dir/UPPER.dll", b"one" as &[u8]),
            ("dir/lower.txt", b"two"),
        ]);
        let cab_file = tmp.path().join("fixture.cab");
        std::fs::write(&cab_file, &cab).unwrap();

        let out = tmp.path().join("out");
        let n = extract_cab(&cab_file, &out, None, false).unwrap();
        assert_eq!(n, 2);
        assert_eq!(std::fs::read(out.join("dir/UPPER.dll")).unwrap(), b"one");

        // -F glob + -L lowercase
        let out2 = tmp.path().join("out2");
        let n = extract_cab(&cab_file, &out2, Some("*.dll"), true).unwrap();
        assert_eq!(n, 1);
        assert_eq!(std::fs::read(out2.join("dir/upper.dll")).unwrap(), b"one");
        assert!(!out2.join("dir/lower.txt").exists());
    }

    #[test]
    fn cab_embedded_in_sfx_exe() {
        // Self-extracting exe = junk prefix + embedded cabinet (docs/15 §8.5:
        // vcrun/corefonts payloads are extracted straight from exes).
        let tmp = tempfile::tempdir().unwrap();
        let cab = build_cab(&[("msvcp140.dll", b"redist" as &[u8])]);
        let mut exe = vec![0x4d; 0x400]; // DOS-stub-ish junk
        exe.extend_from_slice(&cab);
        let exe_file = tmp.path().join("vc_redist.exe");
        std::fs::write(&exe_file, &exe).unwrap();

        let out = tmp.path().join("out");
        let n = extract_cab(&exe_file, &out, None, false).unwrap();
        assert_eq!(n, 1);
        assert_eq!(std::fs::read(out.join("msvcp140.dll")).unwrap(), b"redist");
    }

    #[test]
    fn sevenz_round_trip_all_and_filtered() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = tmp.path().join("payload/nested");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("a.txt"), b"aaa").unwrap();
        std::fs::write(payload.join("b.bin"), b"bbb").unwrap();
        let archive = build_7z(tmp.path());

        let out = tmp.path().join("all");
        extract_7z(&archive, &out, &[]).unwrap();
        assert_eq!(std::fs::read(out.join("nested/a.txt")).unwrap(), b"aaa");
        assert_eq!(std::fs::read(out.join("nested/b.bin")).unwrap(), b"bbb");

        let out2 = tmp.path().join("one");
        extract_7z(&archive, &out2, &["a.txt".to_string()]).unwrap();
        assert_eq!(std::fs::read(out2.join("nested/a.txt")).unwrap(), b"aaa");
        assert!(!out2.join("nested/b.bin").exists());
        assert!(!out2.join(".gogo-7z-scratch").exists());

        let err = extract_7z(&archive, out2.parent().unwrap(), &["nope".to_string()]);
        assert!(err.is_err());
    }

    #[test]
    fn cli_parsing_matches_winetricks_invocations() {
        let tmp = tempfile::tempdir().unwrap();
        let cab = build_cab(&[("f.dll", b"x" as &[u8])]);
        let cab_file = tmp.path().join("x.cab");
        std::fs::write(&cab_file, &cab).unwrap();
        let out = tmp.path().join("d1");
        // flags in varying order, as winetricks emits them
        let code = run_tool(
            "cabextract",
            &[
                "-q".to_string(),
                "-d".to_string(),
                out.display().to_string(),
                "-L".to_string(),
                "-F".to_string(),
                "*.dll".to_string(),
                cab_file.display().to_string(),
            ],
        );
        assert_eq!(code, 0);
        assert!(out.join("f.dll").exists());

        // directory= form + reversed order
        let out2 = tmp.path().join("d2");
        let code = run_tool(
            "cabextract",
            &[
                cab_file.display().to_string(),
                format!("--directory={}", out2.display()),
                "-F".to_string(),
                "miss.sys".to_string(),
            ],
        );
        assert_eq!(code, 1); // nothing extracted

        // 7z: x + attached -o
        let payload = tmp.path().join("p");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("z.txt"), b"z").unwrap();
        let arch = tmp.path().join("z.7z");
        sevenz_rust2::compress_to_path(&payload, &arch).unwrap();
        let out3 = tmp.path().join("d3");
        let code = run_tool(
            "7z",
            &[
                "x".to_string(),
                arch.display().to_string(),
                format!("-o{}", out3.display()),
                "-y".to_string(),
            ],
        );
        assert_eq!(code, 0);
        assert!(out3.join("z.txt").exists());

        assert_eq!(run_tool("cabextract", &[]), 2);
        assert_eq!(run_tool("nope", &[]), 2);
    }
}

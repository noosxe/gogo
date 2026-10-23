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
/// `filter` is a cabextract `-F` glob (`*`, `?`, case-insensitive like
/// cabextract's); `lowercase` mirrors `-L`.
/// Returns the number of files written (cabextract exits 1 when zero).
pub fn extract_cab(
    file: &Path,
    dest: &Path,
    filter: Option<&str>,
    lowercase: bool,
) -> Result<usize, String> {
    let mut data = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let cabinets = find_cabinets(&data);
    if cabinets.is_empty() {
        return Err(format!(
            "{}: no cabinet found (not a .cab / SFX?)",
            file.display()
        ));
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut written = 0usize;
    for (offset, len) in cabinets {
        written += extract_one(&mut data[offset..offset + len], dest, filter, lowercase)?;
    }
    Ok(written)
}

/// `cabextract -l`: every file in every cabinet of `file`, as (name, size).
pub fn list_cab(file: &Path) -> Result<Vec<(String, u32)>, String> {
    let data = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let cabinets = find_cabinets(&data);
    if cabinets.is_empty() {
        return Err(format!(
            "{}: no cabinet found (not a .cab / SFX?)",
            file.display()
        ));
    }
    let mut out = Vec::new();
    for (offset, len) in cabinets {
        let cabinet = cab::Cabinet::new(std::io::Cursor::new(&data[offset..offset + len]))
            .map_err(|e| e.to_string())?;
        for folder in cabinet.folder_entries() {
            for entry in folder.file_entries() {
                out.push((entry.name().to_string(), entry.uncompressed_size()));
            }
        }
    }
    Ok(out)
}

/// Cabinets in `data` as (offset, length). Like cabextract's search, the scan
/// resumes after each cabinet's end (`cbCabinet`): a cabinet stored inside
/// another one (the DirectX redist's inner `.cab`s sit raw in LZX verbatim
/// blocks) is part of its payload, not a cabinet of its own (#59).
fn find_cabinets(data: &[u8]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut pos = 0usize;
    while let Some(hit) = data[pos..].windows(4).position(|w| w == b"MSCF") {
        let offset = pos + hit;
        let len = read_u32(data, offset + 8).map(|n| n as usize);
        match len {
            Some(len)
                if len >= 36
                    && offset + len <= data.len()
                    && cab::Cabinet::new(std::io::Cursor::new(&data[offset..offset + len]))
                        .is_ok() =>
            {
                found.push((offset, len));
                pos = offset + len;
            }
            _ => pos = offset + 1, // not a real cabinet — keep scanning
        }
    }
    found
}

/// One CFFILE record: where the file sits in its folder's uncompressed
/// stream, and where the record itself is (for [`extract_one`]'s patch).
struct CabFile {
    name: String,
    folder: u16,
    offset: u32,
    size: u32,
    record: usize,
}

/// The CFFILE table of a cabinet that `cab` has already accepted. The `cab`
/// crate hides file offsets, so this reads them from the header itself.
fn cab_file_table(cab_bytes: &[u8]) -> Option<Vec<CabFile>> {
    let mut at = read_u32(cab_bytes, 16)? as usize; // coffFiles
    let count = u16::from_le_bytes(cab_bytes.get(28..30)?.try_into().ok()?);
    let mut files = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let record = at;
        let size = read_u32(cab_bytes, at)?;
        let offset = read_u32(cab_bytes, at + 4)?;
        let folder = u16::from_le_bytes(cab_bytes.get(at + 8..at + 10)?.try_into().ok()?);
        let name_len = cab_bytes.get(at + 16..)?.iter().position(|&b| b == 0)?;
        at += 16 + name_len + 1;
        files.push(CabFile {
            name: String::new(), // filled from `cab`'s decoding of the name
            folder,
            offset,
            size,
            record,
        });
    }
    Some(files)
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Extract the matching files of one cabinet.
///
/// `cab::Cabinet::read_file` decodes a folder from its first block on every
/// call, so extracting N files of one big LZX folder (the DirectX redist is
/// a single 100 MB folder of 157 files) decodes it N times. Instead each
/// folder is streamed once: one CFFILE record per folder is patched (in this
/// in-memory copy) to span the whole folder, read through `cab`'s own
/// decoders, and cut into files at the offsets from [`cab_file_table`].
fn extract_one(
    cab_bytes: &mut [u8],
    dest: &Path,
    filter: Option<&str>,
    lowercase: bool,
) -> Result<usize, String> {
    use std::io::Read as _;
    let (mut table, folder_count) = {
        let cabinet =
            cab::Cabinet::new(std::io::Cursor::new(&*cab_bytes)).map_err(|e| e.to_string())?;
        let mut table = cab_file_table(cab_bytes).ok_or("truncated cabinet file table")?;
        // `cab` keeps each folder's files in CFFILE order: pair them up.
        let names: Vec<Vec<String>> = cabinet
            .folder_entries()
            .map(|f| f.file_entries().map(|e| e.name().to_string()).collect())
            .collect();
        let mut next = vec![0usize; names.len()];
        for file in &mut table {
            let f = file.folder as usize;
            file.name = names
                .get(f)
                .and_then(|n| n.get(next[f]))
                .ok_or("cabinet file table mismatch")?
                .clone();
            next[f] += 1;
        }
        (table, names.len())
    };
    let mut name_counts = std::collections::HashMap::new();
    for file in &table {
        *name_counts.entry(file.name.clone()).or_insert(0usize) += 1;
    }

    // Per folder: the wanted files in stream order, and the carrier record.
    let mut plans: Vec<(String, Vec<usize>)> = Vec::new();
    for folder in 0..folder_count as u16 {
        let mut wanted: Vec<usize> = (0..table.len())
            .filter(|&i| table[i].folder == folder && cab_filter_match(filter, &table[i].name))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        wanted.sort_by_key(|&i| table[i].offset);
        // read_file looks files up by name: the carrier's must be unique.
        let carrier = (0..table.len())
            .find(|&i| table[i].folder == folder && name_counts[&table[i].name] == 1)
            .ok_or_else(|| format!("folder {folder}: no uniquely named file to read it by"))?;
        let end = table
            .iter()
            .filter(|f| f.folder == folder)
            .map(|f| f.offset as u64 + f.size as u64)
            .max()
            .unwrap_or(0);
        let end = u32::try_from(end).map_err(|_| format!("folder {folder} too large"))?;
        let rec = table[carrier].record;
        cab_bytes[rec..rec + 4].copy_from_slice(&end.to_le_bytes());
        cab_bytes[rec + 4..rec + 8].copy_from_slice(&0u32.to_le_bytes());
        plans.push((table[carrier].name.clone(), wanted));
    }

    let mut cabinet =
        cab::Cabinet::new(std::io::Cursor::new(&*cab_bytes)).map_err(|e| e.to_string())?;
    let mut written = 0usize;
    for (carrier, wanted) in plans {
        let mut stream = None;
        let mut pos = 0u64;
        for i in wanted {
            let file = &mut table[i];
            let out = cab_out_path(dest, &file.name, lowercase)?;
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            let start = file.offset as u64;
            if stream.is_none() || start < pos {
                // First file, or an overlapping entry: (re)start the folder.
                stream = Some(
                    cabinet
                        .read_file(&carrier)
                        .map_err(|e| format!("{}: {e}", file.name))?,
                );
                pos = 0;
            }
            let reader = stream.as_mut().expect("opened above");
            std::io::copy(&mut reader.take(start - pos), &mut std::io::sink())
                .map_err(|e| format!("{}: {e}", file.name))?;
            let mut out_file =
                std::fs::File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            let n = std::io::copy(&mut reader.take(file.size as u64), &mut out_file)
                .map_err(|e| format!("{}: {e}", file.name))?;
            if n != file.size as u64 {
                return Err(format!(
                    "{}: truncated ({n} of {} bytes)",
                    file.name, file.size
                ));
            }
            pos = start + n;
            written += 1;
        }
    }
    Ok(written)
}

/// cabextract `-F`: the glob against the whole path or the basename,
/// ignoring case (winetricks relies on it: `-F '*d3dcompiler_43*x86*'`
/// selects `Jun2010_D3DCompiler_43_x86.cab`).
fn cab_filter_match(filter: Option<&str>, name: &str) -> bool {
    let Some(pattern) = filter else {
        return true;
    };
    let pattern = pattern.to_lowercase();
    let norm = name.replace('\\', "/").to_lowercase();
    glob_match(&pattern, &norm) || glob_match(&pattern, norm.rsplit('/').next().unwrap_or(&norm))
}

/// Where a cabinet entry lands under `dest`; rejects absolute and `..` paths.
fn cab_out_path(dest: &Path, name: &str, lowercase: bool) -> Result<PathBuf, String> {
    let norm = name.replace('\\', "/");
    let norm = norm.strip_prefix("./").unwrap_or(&norm);
    let rel = if lowercase {
        norm.to_lowercase()
    } else {
        norm.to_string()
    };
    let rel = Path::new(&rel);
    if !rel
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!("unsafe cabinet entry path: {name}"));
    }
    Ok(dest.join(rel))
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
    let mut list = false;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-q" | "--quiet" => {} // tolerated, no-op
            "-l" | "--list" => list = true,
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
    if list {
        for file in &files {
            match list_cab(file) {
                Ok(entries) => {
                    for (name, size) in entries {
                        println!("{size:>10} | {name}");
                    }
                }
                Err(e) => {
                    eprintln!("gogo-shims: {e}");
                    return 1;
                }
            }
        }
        return 0;
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
        build_cab_with(cab::CompressionType::None, files)
    }

    fn build_cab_with(ctype: cab::CompressionType, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = cab::CabinetBuilder::new();
        {
            let folder = builder.add_folder(ctype);
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

    /// Deterministic incompressible bytes (xorshift), so payloads span
    /// several 32 KiB data blocks even when compressed.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    /// #59: the DirectX Jun2010 redist is one cabinet whose files are
    /// cabinets, stored raw (LZX verbatim blocks), so `MSCF` shows up again
    /// inside the outer cabinet's data. The inner cabinet's bytes are split
    /// by the outer CFDATA headers, so reading it in place fails its
    /// checksums ("Checksum error in data block 0") — it must be skipped,
    /// and `-F` must match `Jun2010_D3DCompiler_43_x86.cab` ignoring case.
    #[test]
    fn cab_of_cabs_like_directx_redist() {
        let tmp = tempfile::tempdir().unwrap();
        let dll = noise(100_000, 43);
        let inner = build_cab_with(
            cab::CompressionType::MsZip,
            &[
                ("d3dcompiler_43.dll", &dll),
                ("D3DCompiler_43.inf", b"[ok]"),
            ],
        );
        let other = noise(70_000, 7);
        let outer = build_cab(&[
            ("Apr2005_d3dx9_25_x64.cab", &other),
            ("Jun2010_D3DCompiler_43_x86.cab", &inner),
            ("DXSETUP.exe", b"MZ"),
        ]);
        let mut exe = vec![0x4d; 0x600];
        exe.extend_from_slice(&outer);
        let exe_file = tmp.path().join("directx_Jun2010_redist.exe");
        std::fs::write(&exe_file, &exe).unwrap();

        // winetricks' first pass: pick the inner cab out of the redist.
        let out = tmp.path().join("tmp");
        let n = extract_cab(&exe_file, &out, Some("*d3dcompiler_43*x86*"), true).unwrap();
        assert_eq!(n, 1);
        let got = out.join("jun2010_d3dcompiler_43_x86.cab");
        assert_eq!(std::fs::read(&got).unwrap(), inner);

        // Second pass: the DLL out of the inner cab.
        let sys = tmp.path().join("system32");
        let n = extract_cab(&got, &sys, Some("d3dcompiler_43.dll"), true).unwrap();
        assert_eq!(n, 1);
        assert_eq!(std::fs::read(sys.join("d3dcompiler_43.dll")).unwrap(), dll);

        // Unfiltered: exactly the outer cabinet's files, nothing from inside.
        let all = tmp.path().join("all");
        assert_eq!(extract_cab(&exe_file, &all, None, false).unwrap(), 3);
        assert!(!all.join("d3dcompiler_43.dll").exists());
        let names: Vec<String> = list_cab(&exe_file)
            .unwrap()
            .into_iter()
            .map(|e| e.0)
            .collect();
        assert_eq!(
            names,
            [
                "Apr2005_d3dx9_25_x64.cab",
                "Jun2010_D3DCompiler_43_x86.cab",
                "DXSETUP.exe"
            ]
        );
    }

    /// Many files in one compressed folder are cut out of a single pass over
    /// the folder stream, in any CFFILE order, with exact contents.
    #[test]
    fn cab_streams_many_files_of_one_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let payloads: Vec<(String, Vec<u8>)> = (0..40)
            .map(|i| (format!("f{i:02}.bin"), noise(5_000 + i * 997, i as u64 + 1)))
            .collect();
        let files: Vec<(&str, &[u8])> = payloads
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        let cab_file = tmp.path().join("many.cab");
        std::fs::write(
            &cab_file,
            build_cab_with(cab::CompressionType::MsZip, &files),
        )
        .unwrap();
        let out = tmp.path().join("out");
        assert_eq!(extract_cab(&cab_file, &out, None, false).unwrap(), 40);
        for (name, data) in &payloads {
            assert_eq!(&std::fs::read(out.join(name)).unwrap(), data, "{name}");
        }
        let out2 = tmp.path().join("out2");
        assert_eq!(
            extract_cab(&cab_file, &out2, Some("F3?.BIN"), false).unwrap(),
            10
        );
        assert_eq!(std::fs::read(out2.join("f35.bin")).unwrap(), payloads[35].1);
    }

    #[test]
    fn cab_rejects_parent_dir_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let cab_file = tmp.path().join("evil.cab");
        std::fs::write(&cab_file, build_cab(&[("..\\escape.txt", b"x" as &[u8])])).unwrap();
        let err = extract_cab(&cab_file, &tmp.path().join("out"), None, false).unwrap_err();
        assert!(err.contains("unsafe"), "{err}");
        assert!(!tmp.path().join("escape.txt").exists());
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

        // -l lists, never extracts
        let out_l = tmp.path().join("dl");
        let code = run_tool(
            "cabextract",
            &[
                "-l".to_string(),
                format!("--directory={}", out_l.display()),
                cab_file.display().to_string(),
            ],
        );
        assert_eq!(code, 0);
        assert!(!out_l.exists());

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

//! Importing a folder or a zip into the skin folder (design 27 §10, design 26 S10 and
//! S12): good sources install, and a refused one says why and leaves nothing behind.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use gogo_skin::limits::{MAX_ARCHIVE_BYTES, MAX_ARCHIVE_ENTRIES};
use gogo_skin::{ImportError, SkinFolder, SkinPackage};
use proptest::prelude::*;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

type Files = Vec<(String, Vec<u8>)>;

fn walk(root: &Path, rel: &str, out: &mut Files) {
    for e in std::fs::read_dir(root.join(rel)).unwrap().flatten() {
        let name = e.file_name().into_string().unwrap();
        let path = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        if e.path().is_dir() {
            walk(root, &path, out);
        } else {
            out.push((path, std::fs::read(e.path()).unwrap()));
        }
    }
}

/// The Default skin, renamed to `mine`: a whole skin that loads.
fn skin() -> Files {
    let mut files = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../gogo-skins/skins/default"),
        "",
        &mut files,
    );
    files.sort();
    edit(&mut files, "skin.kdl", "skin \"default\"", "skin \"mine\"");
    files
}

fn edit(files: &mut Files, path: &str, from: &str, to: &str) {
    let file = files.iter_mut().find(|f| f.0 == path).unwrap();
    let text = String::from_utf8(file.1.clone()).unwrap();
    assert!(text.contains(from), "{path} has no {from}");
    file.1 = text.replace(from, to).into_bytes();
}

fn with(path: &str, data: &[u8]) -> Files {
    let mut files = skin();
    files.push((path.to_owned(), data.to_vec()));
    files
}

fn deflated() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

fn zip(files: &Files) -> Vec<u8> {
    zip_with(files, |_| {})
}

fn zip_with(files: &Files, more: impl FnOnce(&mut ZipWriter<Cursor<Vec<u8>>>)) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (path, data) in files {
        zip.start_file(path.as_str(), deflated()).unwrap();
        zip.write_all(data).unwrap();
    }
    more(&mut zip);
    zip.finish().unwrap().into_inner()
}

/// The offset of the central directory header of the entry `name`.
fn central(zip: &[u8], name: &str) -> usize {
    (0..zip.len() - 46)
        .find(|&i| {
            zip[i..].starts_with(b"PK\x01\x02") && zip[i + 46..].starts_with(name.as_bytes())
        })
        .expect("the entry is in the central directory")
}

struct Case {
    tmp: tempfile::TempDir,
    folder: SkinFolder,
}

impl Case {
    fn new() -> Case {
        let tmp = tempfile::tempdir().unwrap();
        let folder = SkinFolder::new(tmp.path().join("skins"));
        Case { tmp, folder }
    }

    fn archive(&self, data: &[u8]) -> PathBuf {
        let path = self.tmp.path().join("skin.zip");
        std::fs::write(&path, data).unwrap();
        path
    }

    fn source_folder(&self, files: &Files) -> PathBuf {
        // Any name: `skin.kdl` names the skin, not the folder it comes in.
        let dir = self.tmp.path().join("Some Download (1)");
        for (path, data) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, data).unwrap();
        }
        dir
    }

    fn import(&self, source: &Path) -> Result<(), ImportError> {
        let candidate = self.folder.read(source)?;
        let unsynced = self.folder.install(&candidate, false)?;
        assert!(unsynced.is_none(), "{unsynced:?}");
        Ok(())
    }

    /// The import is refused, saying `why`, and the skin folder stays empty.
    fn refused(&self, source: &Path, why: &str) {
        let err = self.import(source).expect_err("the source is refused");
        let lines = err.lines().join("\n");
        assert!(lines.contains(why), "no {why:?} in:\n{lines}");
        assert!(
            !lines.chars().any(|c| c.is_control() && c != '\n'),
            "{lines:?}"
        );
        assert!(
            !self.folder.root().exists(),
            "something was written: {lines}"
        );
    }

    fn installed(&self) -> Files {
        let mut out = Vec::new();
        walk(&self.folder.root().join("mine"), "", &mut out);
        out.sort();
        out
    }
}

fn refused_zip(data: &[u8], why: &str) {
    let case = Case::new();
    case.refused(&case.archive(data), why);
}

#[test]
fn a_zip_installs_what_it_holds_and_loads() {
    let case = Case::new();
    let mut files = with("assets/NOTICE.txt", b"free to use");
    files.sort();
    case.import(&case.archive(&zip(&files))).expect("installs");
    assert_eq!(case.installed(), files);
    assert_eq!(case.folder.names(), ["mine"]);
    let loaded = SkinPackage::load(&case.folder.path("mine").unwrap()).expect("loads");
    assert!(loaded.report().is_clean(), "{}", loaded.report());
    assert_eq!(
        case.folder.installed_version("mine").as_deref(),
        Some("0.1.0")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(case.folder.root().join("mine/skin.kdl")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o111, 0);
    }
}

#[test]
fn one_top_folder_is_left_out_and_system_files_are_passed_over() {
    let case = Case::new();
    let packed: Files = skin()
        .into_iter()
        .map(|(path, data)| (format!("mine-1.2/{path}"), data))
        .collect();
    let data = zip_with(&packed, |zip| {
        zip.add_directory("mine-1.2/", deflated()).unwrap();
        for junk in ["__MACOSX/mine-1.2/._skin.kdl", "mine-1.2/.DS_Store"] {
            zip.start_file(junk, deflated()).unwrap();
            zip.write_all(b"junk").unwrap();
        }
    });
    case.import(&case.archive(&data)).expect("installs");
    assert_eq!(case.installed(), skin());
}

#[test]
fn a_folder_installs_without_its_system_files() {
    let case = Case::new();
    let mut files = skin();
    files.push((".DS_Store".to_owned(), b"junk".to_vec()));
    files.push(("routes/.DS_Store".to_owned(), b"junk".to_vec()));
    case.import(&case.source_folder(&files)).expect("installs");
    assert_eq!(case.installed(), skin());
}

#[test]
fn an_installed_name_gives_way_only_to_a_replace() {
    let case = Case::new();
    let archive = case.archive(&zip(&skin()));
    case.import(&archive).expect("installs");

    let mut newer = with("NOTICE.txt", b"new");
    edit(&mut newer, "skin.kdl", "\"0.1.0\"", "\"0.2.0\"");
    newer.sort();
    let archive = case.archive(&zip(&newer));
    let candidate = case.folder.read(&archive).unwrap();
    let err = case.folder.install(&candidate, false).unwrap_err();
    assert!(
        matches!(&err, ImportError::Exists { version, .. } if version == "0.1.0"),
        "{err:?}"
    );
    assert_eq!(case.installed(), skin());

    case.folder.install(&candidate, true).expect("replaces");
    assert_eq!(case.installed(), newer);
    assert_eq!(case.folder.names(), ["mine"]);
}

#[test]
fn a_path_out_of_the_skin_is_refused() {
    for (name, why) in [
        ("../evil.kdl", "goes up with `..`"),
        ("assets/../../evil.png", "goes up with `..`"),
        ("/etc/evil.txt", "is absolute"),
        ("assets\\..\\evil.png", "uses a backslash"),
        ("C:/evil.txt", "contains `:`"),
        ("assets/./a.png", "has a `.` component"),
        ("assets//a.png", "has an empty component"),
        ("assets/a\u{1b}[2J.png", "contains a control character"),
        ("__MACOSX/../evil.txt", "goes up with `..`"),
    ] {
        refused_zip(&zip(&with(name, b"x")), why);
    }
    // The same inside a top folder.
    let packed: Files = with("../evil.txt", b"x")
        .into_iter()
        .map(|(path, data)| (format!("top/{path}"), data))
        .collect();
    refused_zip(&zip(&packed), "goes up with `..`");
}

#[test]
fn a_link_or_a_special_file_is_refused() {
    let link = zip_with(&skin(), |zip| {
        zip.add_symlink("assets/logo.png", "/etc/passwd", deflated())
            .unwrap();
    });
    refused_zip(&link, "is a symbolic link");

    let mut fifo = zip(&with("assets/pipe.txt", b"x"));
    let at = central(&fifo, "assets/pipe.txt");
    // The external attributes: the Unix mode is the upper half.
    fifo[at + 38..at + 42].copy_from_slice(&(0o010_644u32 << 16).to_le_bytes());
    refused_zip(&fifo, "is not a regular file");
}

#[cfg(unix)]
#[test]
fn a_link_in_a_folder_is_refused() {
    let case = Case::new();
    let dir = case.source_folder(&skin());
    std::fs::create_dir(dir.join("assets")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", dir.join("assets/notes.txt")).unwrap();
    case.refused(&dir, "is a symbolic link");
}

#[test]
fn an_encrypted_entry_is_refused() {
    let mut data = zip(&with("NOTICE.txt", b"x"));
    let at = central(&data, "NOTICE.txt");
    data[at + 8] |= 1;
    refused_zip(&data, "encrypted");
}

#[test]
fn two_entries_that_can_not_both_be_written_are_refused() {
    let mut files = with("assets/one.txt", b"1");
    files.push(("assets/two.txt".to_owned(), b"2".to_vec()));
    // The writer refuses a repeated name, so it is renamed after writing.
    let mut data = zip(&files);
    let mut i = 0;
    while i + 7 <= data.len() {
        if &data[i..i + 7] == b"two.txt" {
            data[i..i + 7].copy_from_slice(b"one.txt");
        }
        i += 1;
    }
    refused_zip(&data, "same name");

    let mut case_only = with("assets/a.txt", b"1");
    case_only.push(("assets/A.txt".to_owned(), b"2".to_vec()));
    refused_zip(&zip(&case_only), "appears twice");

    let mut folders = with("assets/img/a.txt", b"1");
    folders.push(("assets/Img/b.txt".to_owned(), b"2".to_vec()));
    refused_zip(&zip(&folders), "appears twice");

    let mut file_and_folder = with("assets/x.txt", b"1");
    file_and_folder.push(("assets/x.txt/y.txt".to_owned(), b"2".to_vec()));
    refused_zip(&zip(&file_and_folder), "appears twice");
}

#[test]
fn what_a_skin_can_not_hold_is_refused_by_name() {
    refused_zip(&zip(&with("README.md", b"x")), "README.md");
    refused_zip(&zip(&with("assets/inner.zip", b"x")), "inner.zip");
    let empty_dir = zip_with(&skin(), |zip| {
        zip.add_directory("docs/", deflated()).unwrap();
    });
    refused_zip(&empty_dir, "`docs` is not part of the skin layout");
    refused_zip(
        &zip(&with("assets/1/2/3/4/5/6/7/8/a.txt", b"x")),
        "nested too deeply",
    );

    let case = Case::new();
    case.refused(&case.source_folder(&with("README.md", b"x")), "README.md");
}

#[test]
fn too_much_is_refused() {
    refused_zip(&vec![0; MAX_ARCHIVE_BYTES + 1], "larger than");

    let many = zip_with(&skin(), |zip| {
        for i in 0..=MAX_ARCHIVE_ENTRIES {
            zip.add_directory(format!("assets/d{i}/"), deflated())
                .unwrap();
        }
    });
    refused_zip(&many, "entries");

    // One file over its kind's limit.
    refused_zip(
        &zip(&with("NOTICE.txt", &[b'a'; 65 * 1024])),
        "unpacks to more",
    );

    // Five fonts of 8 MiB that compress, but not past the ratio: together over the whole.
    let noise: Vec<u8> = (0u32..8 * 1024 * 1024)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let mut files = skin();
    for i in 0..5 {
        files.push((format!("assets/{i}.ttf"), noise.clone()));
    }
    let total = zip(&files);
    assert!(total.len() <= MAX_ARCHIVE_BYTES, "{}", total.len());
    refused_zip(&total, "unpacks to more");
}

#[test]
fn a_bomb_is_refused() {
    refused_zip(
        &zip(&with("assets/zeros.ttf", &vec![0; 2 * 1024 * 1024])),
        "times its size",
    );
}

#[test]
fn a_declared_size_is_not_trusted() {
    let mut data = zip(&with("NOTICE.txt", &[b'a'; 65 * 1024]));
    let at = central(&data, "NOTICE.txt");
    data[at + 24..at + 28].copy_from_slice(&10u32.to_le_bytes());
    refused_zip(&data, "NOTICE.txt");
}

#[test]
fn an_entry_whose_packed_bytes_are_not_its_own_is_refused() {
    let whole = zip(&with("NOTICE.txt", b"text"));
    let at = central(&whole, "NOTICE.txt");

    // A packed size that reaches past the archive.
    let mut long = whole.clone();
    long[at + 20..at + 24].copy_from_slice(&0x00ff_ffffu32.to_le_bytes());
    refused_zip(&long, "past the end");

    // The offset of another entry's header: both would unpack the same bytes.
    let mut shared = whole.clone();
    let other = central(&whole, "skin.kdl");
    shared.copy_within(other + 42..other + 46, at + 42);
    refused_zip(&shared, "share their data");
}

#[test]
fn what_reorders_or_hides_text_is_not_shown() {
    let name = "evil\u{202e}\u{200b}\u{2066}.exe";
    let case = Case::new();
    let err = case
        .import(&case.archive(&zip(&with(name, b"x"))))
        .expect_err("not a skin's file");
    let lines = err.lines().join("\n");
    assert!(lines.contains("evil???.exe"), "{lines}");
    assert!(
        !lines
            .chars()
            .any(|c| c != '\n' && gogo_skin::import::is_hidden(c)),
        "{lines:?}"
    );
}

#[test]
fn a_damaged_archive_is_refused() {
    refused_zip(b"not a zip at all", "not a readable zip");
    let whole = zip(&skin());
    refused_zip(&whole[..whole.len() / 2], "not a readable zip");

    let mut bad_crc = zip(&with("NOTICE.txt", b"text"));
    let at = central(&bad_crc, "NOTICE.txt");
    bad_crc[at + 16] ^= 0xff;
    // The local header's copy, 30 bytes before the name there.
    let local = (0..bad_crc.len() - 40)
        .find(|&i| {
            bad_crc[i..].starts_with(b"PK\x03\x04") && bad_crc[i + 30..].starts_with(b"NOTICE.txt")
        })
        .unwrap();
    bad_crc[local + 14] ^= 0xff;
    refused_zip(&bad_crc, "NOTICE.txt");
}

#[test]
fn a_skin_that_does_not_load_or_may_not_be_installed_is_refused() {
    // A route error alone refuses it too.
    let mut route = skin();
    let game = route.iter_mut().find(|f| f.0 == "routes/game.kdl").unwrap();
    game.1.extend_from_slice(b"\nnonsense {\n}\n");
    refused_zip(&zip(&route), "routes/game.kdl");

    let mut tokens = skin();
    tokens.retain(|f| f.0 != "tokens.kdl");
    refused_zip(&zip(&tokens), "tokens.kdl");

    let mut bad_name = skin();
    edit(&mut bad_name, "skin.kdl", "\"mine\"", "\"../mine\"");
    refused_zip(&zip(&bad_name), "skin name");

    for reserved in ["default", "instrument", "slab"] {
        let mut files = skin();
        edit(
            &mut files,
            "skin.kdl",
            "\"mine\"",
            &format!("\"{reserved}\""),
        );
        refused_zip(&zip(&files), "built-in");
    }

    let mut light = skin();
    edit(&mut light, "skin.kdl", "modes \"dark\"", "modes \"light\"");
    edit(
        &mut light,
        "tokens.kdl",
        "tokens \"dark\"",
        "tokens \"light\"",
    );
    refused_zip(&zip(&light), "dark");

    let mut no_manifest = skin();
    no_manifest.retain(|f| f.0 != "skin.kdl");
    refused_zip(&zip(&no_manifest), "no skin.kdl");
    let case = Case::new();
    case.refused(&case.source_folder(&no_manifest), "no skin.kdl");
}

#[test]
fn an_installed_skin_is_not_a_source() {
    let case = Case::new();
    case.import(&case.archive(&zip(&skin()))).unwrap();
    let err = case.folder.read(&case.folder.path("mine").unwrap());
    assert!(matches!(err, Err(ImportError::Source(_))), "{err:?}");
}

#[test]
fn only_a_skin_name_has_a_folder() {
    let case = Case::new();
    case.import(&case.archive(&zip(&skin()))).unwrap();
    std::fs::create_dir_all(case.folder.root().join("default")).unwrap();
    for name in ["..", ".", "", "mine/routes", "/etc", "Mine", "default"] {
        assert_eq!(case.folder.path(name), None, "{name}");
    }
    assert_eq!(case.folder.names(), ["mine"]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever an entry is called, nothing is written outside the skin's own folder.
    #[test]
    fn no_name_writes_outside_the_skin(name in "[a-zA-Z./\\\\: ]{1,24}", txt in any::<bool>()) {
        let case = Case::new();
        let name = if txt { format!("assets/{name}.txt") } else { name };
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for (path, data) in &skin() {
            zip.start_file(path.as_str(), deflated()).unwrap();
            zip.write_all(data).unwrap();
        }
        if zip.start_file(name.as_str(), deflated()).is_ok() {
            zip.write_all(b"x").unwrap();
        }
        let archive = case.archive(&zip.finish().unwrap().into_inner());
        let _ = case.import(&archive);
        let mut found = Vec::new();
        walk(case.tmp.path(), "", &mut found);
        for (path, _) in found {
            prop_assert!(
                path == "skin.zip" || path == "skins/.lock" || path.starts_with("skins/mine/"),
                "{path}"
            );
        }
    }
}

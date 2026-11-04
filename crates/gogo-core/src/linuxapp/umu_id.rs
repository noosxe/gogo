//! GOG → umu id (design 18 §4.1, O4, #170). gogo always runs a game with
//! `GAMEID=umu-<id>` and `STORE=gog`; this finds the id, in order:
//!
//! 1. The chosen Proton build's `protonfixes/umu-database.csv`: the row with
//!    `STORE` `gog` and `CODENAME` the GOG id gives `UMU_ID`. Same snapshot
//!    as the fixes that build runs, no network.
//! 2. Only when the build has no CSV (an unmanaged build without
//!    `protonfixes/`): umu-database's CSV from GitHub `main`, cached as
//!    `<cache>/umu-database.csv` with its ETag beside it. Refreshed with a
//!    conditional GET when older than [`MAX_AGE`] and a lookup needs it, or
//!    when the caller passes `refresh`. A failed fetch keeps the stale copy.
//! 3. No row anywhere: `umu-<gogid>`.
//!
//! A lookup never fails: what went wrong is in [`Lookup::warnings`]. gogo
//! never asks umu's online API (it lags the CSV and would send the library
//! one game at a time); the GitHub fetch is the only request. umu-database
//! is GPL-3.0: it is read in place or downloaded into the user's cache, never
//! vendored (R13), and tests use synthesized rows.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Where the CSV sits in a Proton build.
pub const BUILD_CSV: &str = "protonfixes/umu-database.csv";

/// umu-database's CSV on `main` (O4: freshest over reproducible).
pub const DB_URL: &str =
    "https://raw.githubusercontent.com/Open-Wine-Components/umu-database/main/umu-database.csv";

/// The cached copy's name under gogo's cache dir; its ETag is `<name>.etag`.
pub const CACHE_CSV: &str = "umu-database.csv";

/// How old the cached copy may get before a lookup refreshes it.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const UA: &str = concat!("gogo/", env!("CARGO_PKG_VERSION"));

/// Where an id came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A row in the build's own `protonfixes/umu-database.csv`.
    Build { csv: PathBuf },
    /// A row in the cached GitHub copy.
    Cache { csv: PathBuf },
    /// No row: `umu-<gogid>`.
    Fallback,
}

/// A resolved umu id: what `launch.conf` records as `UMU_ID`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UmuId {
    /// `umu-<id>`, as `GAMEID` takes it.
    pub id: String,
    pub source: Source,
}

impl std::fmt::Display for UmuId {
    /// The summary line: `umu-910830 (umu-database.csv in GE-Proton11-12)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            Source::Build { csv } => {
                // <build>/protonfixes/umu-database.csv → the build's dir name.
                let build = csv
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .map_or_else(|| "the Proton build".into(), |n| n.to_string_lossy());
                write!(f, "{} (umu-database.csv in {build})", self.id)
            }
            Source::Cache { .. } => {
                write!(f, "{} (umu-database.csv from GitHub, cached)", self.id)
            }
            Source::Fallback => write!(f, "{} (no umu-database row: the GOG id)", self.id),
        }
    }
}

/// A lookup's result: the id, plus anything the user should hear about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub umu: UmuId,
    pub warnings: Vec<String>,
}

/// The GitHub fallback's settings. [`GithubCache::real`] outside tests.
#[derive(Debug, Clone)]
pub struct GithubCache {
    /// gogo's cache dir (`state::cache::cache_dir()`, which `GOGO_HOME` flattens).
    pub dir: PathBuf,
    pub url: String,
    /// "Now", for the cached copy's age and the timestamp a 304 gives it.
    pub now: SystemTime,
}

impl GithubCache {
    pub fn real() -> Self {
        GithubCache {
            dir: crate::state::cache::cache_dir(),
            url: DB_URL.to_string(),
            now: SystemTime::now(),
        }
    }

    fn csv(&self) -> PathBuf {
        self.dir.join(CACHE_CSV)
    }

    fn etag(&self) -> PathBuf {
        self.dir.join(format!("{CACHE_CSV}.etag"))
    }
}

/// The umu id for GOG product `gogid` run with the Proton build at `build`
/// (the dir holding `proton`, managed or `path:`). `refresh` forces the
/// GitHub copy to be re-checked, when the build sends the lookup there.
pub async fn lookup(build: &Path, gogid: u64, refresh: bool) -> Lookup {
    lookup_with(build, gogid, refresh, &GithubCache::real()).await
}

/// [`lookup`] against an explicit cache dir, URL and clock.
pub async fn lookup_with(build: &Path, gogid: u64, refresh: bool, gh: &GithubCache) -> Lookup {
    let mut warnings = Vec::new();
    let codename = gogid.to_string();
    let fallback = || UmuId {
        id: format!("umu-{gogid}"),
        source: Source::Fallback,
    };

    // Rule 1. A CSV the build has but gogo can't read counts as none.
    let build_csv = build.join(BUILD_CSV);
    match read_csv(&build_csv) {
        Ok(Some(text)) => match find(&text, &codename) {
            Ok(found) => {
                warnings.extend(found.warnings);
                let umu = found.id.map_or_else(fallback, |id| UmuId {
                    id,
                    source: Source::Build { csv: build_csv },
                });
                return Lookup { umu, warnings };
            }
            Err(e) => warnings.push(format!("{}: {e}; ignoring it", build_csv.display())),
        },
        Ok(None) => {}
        Err(e) => warnings.push(format!("{}: {e}; ignoring it", build_csv.display())),
    }

    // Rule 2. Only a build without (usable) protonfixes data gets here.
    let why = format!(
        "{} has no protonfixes/umu-database.csv (typically an unmanaged build without \
         protonfixes), so its umu id comes from umu-database's CSV on GitHub",
        build.file_name().map_or_else(
            || build.display().to_string(),
            |n| n.to_string_lossy().into()
        ),
    );
    let Some(cached) = ensure_cache(gh, refresh, &why, &mut warnings).await else {
        return Lookup {
            umu: fallback(),
            warnings,
        };
    };
    let text = match read_csv(&cached) {
        Ok(Some(text)) => text,
        Ok(None) => String::new(),
        Err(e) => {
            warnings.push(format!("{}: {e}", cached.display()));
            String::new()
        }
    };
    match find(&text, &codename) {
        Ok(found) => {
            warnings.extend(found.warnings);
            let umu = found.id.map_or_else(fallback, |id| UmuId {
                id,
                source: Source::Cache { csv: cached },
            });
            Lookup { umu, warnings }
        }
        Err(e) => {
            warnings.push(format!(
                "{}: {e}; using umu-{gogid}, gogo's fallback",
                cached.display()
            ));
            Lookup {
                umu: fallback(),
                warnings,
            }
        }
    }
}

/// The file's text; `None` when it doesn't exist.
fn read_csv(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The cached GitHub copy to read, refreshing it first when it's missing, older
/// than [`MAX_AGE`], or `refresh` asks. `None`: there's no copy at all.
async fn ensure_cache(
    gh: &GithubCache,
    refresh: bool,
    why: &str,
    warnings: &mut Vec<String>,
) -> Option<PathBuf> {
    let csv = gh.csv();
    let age = std::fs::metadata(&csv)
        .and_then(|m| m.modified())
        // A timestamp in the future counts as fresh.
        .map(|t| gh.now.duration_since(t).unwrap_or_default())
        .ok();
    if let Some(age) = age
        && age < MAX_AGE
        && !refresh
    {
        return Some(csv);
    }
    // Only ask for a 304 when there's a copy it would keep.
    let etag = age
        .and_then(|_| std::fs::read_to_string(gh.etag()).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match fetch(gh, etag.as_deref()).await {
        Ok(()) => Some(csv),
        Err(e) => match age {
            Some(age) => {
                let days = age.as_secs() / 86_400;
                warnings.push(format!(
                    "{why}; refreshing it failed ({e}): using the cached copy from {days} \
                     day(s) ago"
                ));
                Some(csv)
            }
            None => {
                warnings.push(format!(
                    "{why}; fetching it failed ({e}) and there is no cached copy: using \
                     umu-<gogid>, gogo's fallback. protonfixes' GOG defaults still apply, \
                     but a game-specific fix may be missed"
                ));
                None
            }
        },
    }
}

/// One GET of the CSV, conditional on `etag`. Ok: the cached copy is current
/// (a 304, whose copy gets `now` as its timestamp, or a 200 written over it).
async fn fetch(gh: &GithubCache, etag: Option<&str>) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut req = client.get(&gh.url);
    if let Some(tag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    let resp = req.send().await.map_err(|e| e.without_url().to_string())?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED && etag.is_some() {
        let file = std::fs::File::options()
            .write(true)
            .open(gh.csv())
            .map_err(|e| e.to_string())?;
        file.set_modified(gh.now).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let new_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = resp
        .bytes()
        .await
        .map_err(|e| e.without_url().to_string())?;
    // Never replace a good copy with something that isn't the CSV (a portal page).
    header(&String::from_utf8_lossy(&body)).map_err(|e| format!("the download {e}"))?;

    std::fs::create_dir_all(&gh.dir).map_err(|e| e.to_string())?;
    write_atomic(&gh.csv(), &body, gh.now).map_err(|e| e.to_string())?;
    match new_etag {
        Some(tag) => write_atomic(&gh.etag(), tag.as_bytes(), gh.now),
        None => match std::fs::remove_file(gh.etag()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
    .map_err(|e| e.to_string())
}

/// Write via a temp file and rename, so a reader never sees half a file.
fn write_atomic(path: &Path, contents: &[u8], mtime: SystemTime) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let file = std::fs::File::create(&tmp)?;
    let res = (|| {
        use std::io::Write as _;
        let mut f = &file;
        f.write_all(contents)?;
        file.set_modified(mtime)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// What [`find`] makes of a CSV for one GOG id.
#[derive(Debug, Default, PartialEq, Eq)]
struct Found {
    id: Option<String>,
    warnings: Vec<String>,
}

/// Column indices of the fields the lookup reads.
struct Columns {
    store: usize,
    codename: usize,
    umu_id: usize,
}

/// The header's `STORE`, `CODENAME` and `UMU_ID` columns (names compared
/// trimmed and case-insensitively; their order doesn't matter).
fn header(text: &str) -> Result<Columns, String> {
    let first = Records::new(text).next().unwrap_or_default();
    let col = |name: &str| {
        first
            .iter()
            .position(|h| h.trim().eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("has no {name} column in its header"))
    };
    Ok(Columns {
        store: col("STORE")?,
        codename: col("CODENAME")?,
        umu_id: col("UMU_ID")?,
    })
}

/// The umu id for `codename` among the `gog` rows. Several matching rows:
/// the first in file order wins, and a warning names any other ids they give.
/// A matching row whose `UMU_ID` isn't `umu-…` is skipped with a warning.
fn find(text: &str, codename: &str) -> Result<Found, String> {
    let cols = header(text)?;
    let mut found = Found::default();
    let mut others: Vec<String> = Vec::new();
    for row in Records::new(text).skip(1) {
        let field = |i: usize| row.get(i).map_or("", |s| s.trim());
        if !field(cols.store).eq_ignore_ascii_case("gog") || field(cols.codename) != codename {
            continue;
        }
        let id = field(cols.umu_id);
        if id.len() <= "umu-".len() || !id.starts_with("umu-") {
            found.warnings.push(format!(
                "umu-database row for GOG id {codename} has UMU_ID {id:?}, not umu-<id>; \
                 skipping it"
            ));
            continue;
        }
        match &found.id {
            None => found.id = Some(id.to_string()),
            Some(first) if first != id && !others.iter().any(|o| o == id) => {
                others.push(id.to_string());
            }
            Some(_) => {}
        }
    }
    if let (Some(first), false) = (&found.id, others.is_empty()) {
        found.warnings.push(format!(
            "umu-database has several rows for GOG id {codename}: using the first, {first} \
             (also listed: {})",
            others.join(", ")
        ));
    }
    Ok(found)
}

/// RFC 4180 records over a whole file: `,` separates fields, a field that
/// starts with `"` runs to the closing `"` (commas and line breaks inside it
/// are data, `""` is a quote), and LF or CRLF ends a record. A leading BOM is
/// dropped and blank lines are skipped. Lenient where the RFC is strict: text
/// after a closing quote is kept, and an unclosed quote runs to the end.
struct Records<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Records<'a> {
    fn new(text: &'a str) -> Self {
        Records {
            chars: text
                .strip_prefix('\u{feff}')
                .unwrap_or(text)
                .chars()
                .peekable(),
        }
    }
}

impl Iterator for Records<'_> {
    type Item = Vec<String>;

    fn next(&mut self) -> Option<Vec<String>> {
        loop {
            self.chars.peek()?;
            let mut row = Vec::new();
            let mut field = String::new();
            let mut at_start = true;
            let mut quoted = false;
            while let Some(c) = self.chars.next() {
                match c {
                    '"' if quoted => {
                        if self.chars.peek() == Some(&'"') {
                            self.chars.next();
                            field.push('"');
                        } else {
                            quoted = false;
                        }
                    }
                    _ if quoted => field.push(c),
                    '"' if at_start => quoted = true,
                    ',' => {
                        row.push(std::mem::take(&mut field));
                        at_start = true;
                        continue;
                    }
                    '\r' if self.chars.peek() == Some(&'\n') => {}
                    '\n' => break,
                    _ => field.push(c),
                }
                at_start = false;
            }
            row.push(field);
            if row.len() == 1 && row[0].is_empty() {
                continue;
            }
            return Some(row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthesized rows in umu-database's shape (TITLE, STORE, CODENAME, UMU_ID,
    // two optional columns, EXE_STRINGS). No row is copied from the GPL file.
    const HEADER: &str = "TITLE,STORE,CODENAME,UMU_ID,COMMON ACRONYM (Optional),\
                          NOTE (Optional),EXE_STRINGS (Optional)\n";

    fn csv(rows: &[&str]) -> String {
        let mut s = HEADER.to_string();
        for r in rows {
            s.push_str(r);
            s.push('\n');
        }
        s
    }

    fn build_with(rows: Option<&[&str]>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("proton"), "#!/usr/bin/env python3\n").unwrap();
        if let Some(rows) = rows {
            std::fs::create_dir_all(dir.path().join("protonfixes")).unwrap();
            std::fs::write(dir.path().join(BUILD_CSV), csv(rows)).unwrap();
        }
        dir
    }

    /// A cache dir with a URL nothing listens on: a lookup that reaches the
    /// network fails the test by warning.
    fn offline(dir: &Path) -> GithubCache {
        GithubCache {
            dir: dir.to_path_buf(),
            url: "http://127.0.0.1:9/umu-database.csv".into(),
            now: SystemTime::now(),
        }
    }

    fn records(text: &str) -> Vec<Vec<String>> {
        Records::new(text).collect()
    }

    #[test]
    fn records_follow_rfc4180() {
        assert_eq!(
            records("a,b,,c\r\n\n\"x, y\",\"say \"\"hi\"\"\",\"two\nlines\"\nlast"),
            vec![
                vec!["a", "b", "", "c"],
                vec!["x, y", "say \"hi\"", "two\nlines"],
                vec!["last"],
            ]
        );
        assert_eq!(records("\u{feff}A,B\n"), vec![vec!["A", "B"]]);
        assert_eq!(records(""), Vec::<Vec<String>>::new());
        assert_eq!(records("a,\n"), vec![vec!["a", ""]]);
        // Lenient: an unclosed quote runs to the end, text after a quote is kept.
        assert_eq!(records("\"open,\nx"), vec![vec!["open,\nx"]]);
        assert_eq!(records("\"a\"b,c"), vec![vec!["ab", "c"]]);
    }

    #[test]
    fn header_is_found_by_name_not_position() {
        let text = "umu_id , codename,Title,store\numu-7,42,T,gog\n";
        assert_eq!(find(text, "42").unwrap().id.as_deref(), Some("umu-7"));
        let err = find("TITLE,STORE,CODENAME\nT,gog,42\n", "42").unwrap_err();
        assert!(err.contains("UMU_ID"), "{err}");
    }

    #[test]
    fn find_matches_store_and_codename_only() {
        let text = csv(&[
            "Synthetic Saga,egs,1000000001,umu-5000001,,,",
            "Synthetic Saga,gog,1000000011,umu-5000001,,,",
            "Made-Up Quest,gog,1000000001,umu-5000002,mq,,",
            "Made-Up Quest,GOG ,1000000099,umu-5000099,,,",
        ]);
        assert_eq!(
            find(&text, "1000000001").unwrap(),
            Found {
                id: Some("umu-5000002".into()),
                warnings: vec![]
            }
        );
        // Store compared trimmed and case-insensitively.
        assert_eq!(
            find(&text, "1000000099").unwrap().id.as_deref(),
            Some("umu-5000099")
        );
        // A codename is matched whole, not as a prefix.
        assert_eq!(find(&text, "100000000").unwrap().id, None);
    }

    #[test]
    fn duplicate_rows_take_the_first() {
        let text = csv(&[
            "Fictional Front,gog,1000000002,umu-5000010,,,",
            "Fictional Front GOTY,gog,1000000002,umu-5000010,,,",
            "Fictional Front Redux,gog,1000000002,umu-5000020,,,",
        ]);
        let found = find(&text, "1000000002").unwrap();
        assert_eq!(found.id.as_deref(), Some("umu-5000010"));
        assert_eq!(found.warnings.len(), 1);
        assert!(
            found.warnings[0].contains("umu-5000020"),
            "{:?}",
            found.warnings
        );
        // Agreeing duplicates say nothing.
        let text = csv(&[
            "Fictional Front,gog,1000000002,umu-5000010,,,",
            "Fictional Front,gog,1000000002,umu-5000010,,,",
        ]);
        assert!(find(&text, "1000000002").unwrap().warnings.is_empty());
    }

    #[test]
    fn rows_without_a_umu_id_are_skipped() {
        let text = csv(&[
            "Broken Bit,gog,1000000003,,,,",
            "Broken Bit,gog,1000000003,5000030,,,",
            "Broken Bit,gog,1000000003,umu-5000031,,,",
            "Short Row,gog,1000000004",
        ]);
        let found = find(&text, "1000000003").unwrap();
        assert_eq!(found.id.as_deref(), Some("umu-5000031"));
        assert_eq!(found.warnings.len(), 2, "{:?}", found.warnings);
        assert_eq!(find(&text, "1000000004").unwrap().id, None);
    }

    #[test]
    fn quoted_fields_keep_columns_in_place() {
        let text = csv(&[
            "\"Invented Wars: 40,000, \"\"Deluxe\"\"\",gog,1000000005,umu-5000050,,\
             \"a note, with commas\nand a line break\",",
            "\"Comma, Title\",\"gog\",\"1000000006\",\"umu-5000060\",,,",
        ]);
        assert_eq!(
            find(&text, "1000000005").unwrap().id.as_deref(),
            Some("umu-5000050")
        );
        assert_eq!(
            find(&text, "1000000006").unwrap().id.as_deref(),
            Some("umu-5000060")
        );
    }

    #[tokio::test]
    async fn build_csv_answers_without_the_network() {
        let build = build_with(Some(&["Synthetic Saga,gog,1000000001,umu-5000001,,,"]));
        let cache = tempfile::tempdir().unwrap();
        let gh = offline(cache.path());

        let hit = lookup_with(build.path(), 1000000001, true, &gh).await;
        assert_eq!(hit.umu.id, "umu-5000001");
        assert_eq!(
            hit.umu.source,
            Source::Build {
                csv: build.path().join(BUILD_CSV)
            }
        );
        assert!(hit.warnings.is_empty(), "{:?}", hit.warnings);

        // No row in the build's CSV: rule 3, with no GitHub fetch and no warning.
        let miss = lookup_with(build.path(), 1000000077, true, &gh).await;
        assert_eq!(
            miss.umu,
            UmuId {
                id: "umu-1000000077".into(),
                source: Source::Fallback
            }
        );
        assert!(miss.warnings.is_empty(), "{:?}", miss.warnings);
        assert!(!cache.path().join(CACHE_CSV).exists());
    }

    #[test]
    fn display_names_the_source() {
        let build = Path::new("/x/GE-Proton11-12");
        let id = |source| UmuId {
            id: "umu-5000001".into(),
            source,
        };
        assert_eq!(
            id(Source::Build {
                csv: build.join(BUILD_CSV)
            })
            .to_string(),
            "umu-5000001 (umu-database.csv in GE-Proton11-12)"
        );
        assert_eq!(
            id(Source::Cache {
                csv: PathBuf::from("/c/umu-database.csv")
            })
            .to_string(),
            "umu-5000001 (umu-database.csv from GitHub, cached)"
        );
        assert_eq!(
            id(Source::Fallback).to_string(),
            "umu-5000001 (no umu-database row: the GOG id)"
        );
    }

    // The GitHub fallback, against wiremock. "Now" is injected; nothing sleeps.

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const DAY: Duration = Duration::from_secs(86_400);

    fn gh(server: &MockServer, dir: &Path, now: SystemTime) -> GithubCache {
        GithubCache {
            dir: dir.to_path_buf(),
            url: format!("{}/umu-database.csv", server.uri()),
            now,
        }
    }

    fn mtime(p: &Path) -> SystemTime {
        std::fs::metadata(p).unwrap().modified().unwrap()
    }

    /// The cache as a previous fetch at `at` left it.
    fn seed_cache(dir: &Path, rows: &[&str], etag: Option<&str>, at: SystemTime) {
        write_atomic(&dir.join(CACHE_CSV), csv(rows).as_bytes(), at).unwrap();
        if let Some(tag) = etag {
            std::fs::write(dir.join(format!("{CACHE_CSV}.etag")), tag).unwrap();
        }
    }

    #[tokio::test]
    async fn first_fetch_stores_the_csv_and_its_etag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/umu-database.csv"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"v1\"")
                    .set_body_string(csv(&["Made-Up Quest,gog,1000000001,umu-5000002,,,"])),
            )
            .expect(1)
            .mount(&server)
            .await;
        let build = build_with(None);
        let cache = tempfile::tempdir().unwrap();
        let dir = cache.path().join("gogo-cache");
        let now = SystemTime::now();

        let got = lookup_with(build.path(), 1000000001, false, &gh(&server, &dir, now)).await;
        assert_eq!(
            got.umu,
            UmuId {
                id: "umu-5000002".into(),
                source: Source::Cache {
                    csv: dir.join(CACHE_CSV)
                }
            }
        );
        assert!(got.warnings.is_empty(), "{:?}", got.warnings);
        assert_eq!(
            std::fs::read_to_string(dir.join("umu-database.csv.etag")).unwrap(),
            "\"v1\""
        );

        // A GOG id with no row in it: rule 3, quietly.
        let miss = lookup_with(build.path(), 1000000002, false, &gh(&server, &dir, now)).await;
        assert_eq!(miss.umu.source, Source::Fallback);
        assert!(miss.warnings.is_empty(), "{:?}", miss.warnings);
    }

    #[tokio::test]
    async fn a_fresh_copy_is_used_without_a_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let build = build_with(None);
        let cache = tempfile::tempdir().unwrap();
        let fetched = SystemTime::now();
        seed_cache(
            cache.path(),
            &["Made-Up Quest,gog,1000000001,umu-5000002,,,"],
            Some("\"v1\""),
            fetched,
        );

        let now = fetched + MAX_AGE - Duration::from_secs(60);
        let got = lookup_with(
            build.path(),
            1000000001,
            false,
            &gh(&server, cache.path(), now),
        )
        .await;
        assert_eq!(got.umu.id, "umu-5000002");
        assert!(got.warnings.is_empty(), "{:?}", got.warnings);
    }

    #[tokio::test]
    async fn an_old_copy_gets_a_conditional_get_and_a_304_keeps_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("If-None-Match", "\"v1\""))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;
        let build = build_with(None);
        let cache = tempfile::tempdir().unwrap();
        let fetched = SystemTime::UNIX_EPOCH + 1_000 * DAY;
        seed_cache(
            cache.path(),
            &["Made-Up Quest,gog,1000000001,umu-5000002,,,"],
            Some("\"v1\""),
            fetched,
        );
        let before = std::fs::read(cache.path().join(CACHE_CSV)).unwrap();

        let now = fetched + 8 * DAY;
        let got = lookup_with(
            build.path(),
            1000000001,
            false,
            &gh(&server, cache.path(), now),
        )
        .await;
        assert_eq!(got.umu.id, "umu-5000002");
        assert!(got.warnings.is_empty(), "{:?}", got.warnings);
        assert_eq!(std::fs::read(cache.path().join(CACHE_CSV)).unwrap(), before);
        assert_eq!(mtime(&cache.path().join(CACHE_CSV)), now);
    }

    #[tokio::test]
    async fn a_200_replaces_the_old_copy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("If-None-Match", "\"v1\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"v2\"")
                    .set_body_string(csv(&["Made-Up Quest,gog,1000000001,umu-5000003,,,"])),
            )
            .expect(1)
            .mount(&server)
            .await;
        let build = build_with(None);
        let cache = tempfile::tempdir().unwrap();
        let fetched = SystemTime::UNIX_EPOCH + 1_000 * DAY;
        seed_cache(
            cache.path(),
            &["Made-Up Quest,gog,1000000001,umu-5000002,,,"],
            Some("\"v1\""),
            fetched,
        );

        let now = fetched + 8 * DAY;
        let got = lookup_with(
            build.path(),
            1000000001,
            false,
            &gh(&server, cache.path(), now),
        )
        .await;
        assert_eq!(got.umu.id, "umu-5000003");
        assert_eq!(
            std::fs::read_to_string(cache.path().join("umu-database.csv.etag")).unwrap(),
            "\"v2\""
        );
        assert_eq!(mtime(&cache.path().join(CACHE_CSV)), now);
    }

    #[tokio::test]
    async fn a_failed_refresh_keeps_the_stale_copy_and_warns() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        let build = build_with(None);
        let cache = tempfile::tempdir().unwrap();
        let fetched = SystemTime::UNIX_EPOCH + 1_000 * DAY;
        seed_cache(
            cache.path(),
            &["Made-Up Quest,gog,1000000001,umu-5000002,,,"],
            Some("\"v1\""),
            fetched,
        );

        let now = fetched + 10 * DAY;
        let got = lookup_with(
            build.path(),
            1000000001,
            false,
            &gh(&server, cache.path(), now),
        )
        .await;
        assert_eq!(got.umu.id, "umu-5000002");
        assert!(matches!(got.umu.source, Source::Cache { .. }));
        assert_eq!(got.warnings.len(), 1, "{:?}", got.warnings);
        let w = &got.warnings[0];
        assert!(w.contains("503") && w.contains("10 day(s) ago"), "{w}");
        assert!(w.contains("unmanaged build without protonfixes"), "{w}");
        assert_eq!(mtime(&cache.path().join(CACHE_CSV)), fetched);
    }

    #[tokio::test]
    async fn no_copy_and_no_network_is_the_fallback_with_a_warning() {
        let build = build_with(None);
        let cache = tempfile::tempdir().unwrap();

        let got = lookup_with(build.path(), 1000000001, false, &offline(cache.path())).await;
        assert_eq!(
            got.umu,
            UmuId {
                id: "umu-1000000001".into(),
                source: Source::Fallback
            }
        );
        assert_eq!(got.warnings.len(), 1, "{:?}", got.warnings);
        let w = &got.warnings[0];
        assert!(w.contains("no cached copy"), "{w}");
        assert!(w.contains("unmanaged build without protonfixes"), "{w}");
        assert!(!cache.path().join(CACHE_CSV).exists());
    }

    #[tokio::test]
    async fn refresh_forces_a_fetch_of_a_fresh_copy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("If-None-Match", "\"v1\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"v2\"")
                    .set_body_string(csv(&["Made-Up Quest,gog,1000000001,umu-5000003,,,"])),
            )
            .expect(1)
            .mount(&server)
            .await;
        let build = build_with(None);
        let cache = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        seed_cache(
            cache.path(),
            &["Made-Up Quest,gog,1000000001,umu-5000002,,,"],
            Some("\"v1\""),
            now,
        );

        let got = lookup_with(
            build.path(),
            1000000001,
            true,
            &gh(&server, cache.path(), now),
        )
        .await;
        assert_eq!(got.umu.id, "umu-5000003");
    }

    #[tokio::test]
    async fn a_download_that_is_not_the_csv_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>portal</html>"))
            .expect(1)
            .mount(&server)
            .await;
        let build = build_with(None);
        let cache = tempfile::tempdir().unwrap();
        let fetched = SystemTime::UNIX_EPOCH + 1_000 * DAY;
        seed_cache(
            cache.path(),
            &["Made-Up Quest,gog,1000000001,umu-5000002,,,"],
            None,
            fetched,
        );

        let now = fetched + 8 * DAY;
        let got = lookup_with(
            build.path(),
            1000000001,
            false,
            &gh(&server, cache.path(), now),
        )
        .await;
        assert_eq!(got.umu.id, "umu-5000002");
        assert_eq!(got.warnings.len(), 1, "{:?}", got.warnings);
        assert!(got.warnings[0].contains("UMU_ID") || got.warnings[0].contains("STORE"));
    }

    #[tokio::test]
    async fn an_unreadable_build_csv_counts_as_none() {
        // A build CSV without the needed columns: warned about, then rule 2.
        let build = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(build.path().join("protonfixes")).unwrap();
        std::fs::write(build.path().join(BUILD_CSV), "garbage\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        seed_cache(
            cache.path(),
            &["Made-Up Quest,gog,1000000001,umu-5000002,,,"],
            None,
            SystemTime::now(),
        );

        let got = lookup_with(build.path(), 1000000001, false, &offline(cache.path())).await;
        assert_eq!(got.umu.id, "umu-5000002");
        assert!(matches!(got.umu.source, Source::Cache { .. }));
        assert_eq!(got.warnings.len(), 1, "{:?}", got.warnings);
    }
}

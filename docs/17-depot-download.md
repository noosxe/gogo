# 17 — Depot (Model B) downloads via gog-warp

Date: 2026-10-20 · Status: research for design 14 · All gog-warp facts verified against
the pinned checkout `55ddb010be07d41357144440be3fda65fbb8993a`
(`~/.cargo/git/checkouts/gog-warp-b65ccd132a2e06b0/55ddb01`).

## 1. Goal

Download games straight from GOG's **content system** ("depot" / Model B) — the same
channel Galaxy uses — instead of running offline installers. Output lands in the
**user-level cache directory** (disposable by definition), and a later phase feeds
`mac-app` from that cache instead of pre-downloaded installer files.

This was deferred in doc 10 §1 ("Model A only in v1"); the mac-app work (design 11–13)
now needs it: depot downloads give exact game files without running Windows installers
under wine — the single biggest fragility of the current pipeline.

## 2. What gog-warp already ships (verified from source)

### 2.1 Build & manifest discovery (`src/core.rs`)

- `Core::get_builds(product_id: &str, platform: Platform, password: Option<String>)
  -> BuildResponse` — auth via `ensure_auth()` + `obtain_galaxy_token()`, i.e. the same
  session our `Session` (auth/mod.rs) already restores. `password` covers
  password-protected builds (pass `None` v1).
- `BuildResponse { total_count, count, items: Vec<Build> }`.
- `Build { build_id, product_id, os: Platform, branch, version_name, tags,
  public, date_published: DateTime<Utc>, generation, urls: Vec<Endpoint> }` —
  `generation` is 1 (legacy XML) vs 2 (JSON depot manifests).
- `Core::get_manifest(&Build) -> Manifest` — enum `V1(..)`/`V2(..)`, fetched per
  build's `urls`.
- `Core::get_dependencies_manifest() -> DependenciesManifest` — GOG's shared
  dependency depot (dotnet, directx setupees, …) used by Galaxy-side installs.

### 2.2 Manifest surface (`src/content_system/types/mod.rs`)

- `Manifest::install_directory() -> String` (the directory name the downloader wants),
  `dlcs() -> Vec<String>`, `languages()`, `dependencies()`, `needs_isi()`,
  `install_size(..) -> SizeInfo { disk_size, download_size }`.
- `Manifest::get_depots(client, creds?, language, dlcs)` — internal; the downloader
  calls it in `prepare()`.

### 2.3 The downloader (`src/content_system/downloader/mod.rs`, 1253 lines)

Builder (all optional but `core` + `manifest` are required, checked in `build()`):

| setter | meaning |
|---|---|
| `core(Core)` | clone of `Core` for tokens + reqwest |
| `manifest(Manifest, &build_id)` | what to install |
| `language(String)` / `old_language(String)` | v2 code (`en-US`) / v1 name (`English`) |
| `dlcs(Vec<String>)` / `old_dlcs` | DLC depots to include |
| `install_root(PathBuf)` | parent dir; downloader joins `install_directory()` |
| `install_path(PathBuf)` | exact dir instead (mutually exclusive with `install_root`) |
| `support_root(PathBuf)` | where `gog-support` files land (default: inside install path) |
| `game_dependencies(DependenciesManifest)` | enable dependency resolution |
| `global_dependencies_root(PathBuf)` | shared dep cache; default: game directory |
| `verify(bool)` | full re-verification after download |

Lifecycle (order enforced — `download()` errors with "did you forget prepare()?" ):

1. `take_progress_receiver() -> Option<Receiver<DownloadState>>` — mpsc, take-once.
2. `prepare()` — fetches depot file lists, computes the plan.
3. `get_required_space() -> i64` — disk preflight number.
4. `download()` — workers, chunked v2 depots reconstructed, v1 files fetched directly.

Also: `get_cancellation() -> CancellationToken` (forward our ctrl-c), and
`set_max_speed(i32)` (bytes/sec; `-1` = unlimited; a Mutex-guarded runtime knob).

`DownloadState` (`downloader/progress.rs`):
`Preparing | Allocating(f32) | Verifying(f32) |
Downloading(DownloadProgress { downloaded, written, total_download, total_size }) |
Finished`.

### 2.4 What we do NOT get

- No per-file MD5 chunk XML like Model A — v2 depots are content-addressed chunks with
  their own integrity; verification is gog-warp's `verify` pass.
- No integration with our `engine/` (chunk workers, `GlobalLimiter`): the downloader
  owns its worker pool; the only speed knob is `set_max_speed`.
- No install records / state writes — purely a file-tree producer.

## 3. Cache directory conventions

- macOS (Apple, *File System Programming Guide* — "Library/Caches"): per-app disposable
  data at `~/Library/Caches/<bundle-id-or-name>`; the system may purge on demand. Exactly
  the right home for depot payloads that later tooling re-derives.
- `dirs::cache_dir()` (already a workspace dep, 7.0): `~/Library/Caches` on macOS,
  `$XDG_CACHE_HOME`/`~/.cache` on Linux.
- Linux: the download itself is **stubbed** this phase (clear error, no partial trees);
  the path derivation stays identical so a later phase only fills in behavior.

## 4. Facts from prior docs that carry over

- Doc 01 §2: gog-warp scope = auth/tokens/owned-ids **+** (now activated) Model B
  machinery; pin stays a rev-pin, bump = deliberate commit.
- Doc 08: gog-warp's downloader is battle-tested by Playtron (their OS ships it) —
  credible to lean on rather than reimplement.
- Doc 10 §1: "depot downloads ship with a ratatui TUI" — superseded by this doc's
  decision to ship the CLI lane first (mac-app need), TUI whenever it comes.

## 5. Risks / notes

- `set_max_speed(i32)`: bytes/sec in an `i32` caps at ~2 GiB/s — fine.
- Windows-only build lane is what we want (wine target); macOS-native builds are not
  the mac-app pipeline's input, so platform selection is fixed to `Platform::Windows`
  in v1.
- The downloader's default `gog-support` and dependency placement land *inside* the
  install path unless roots are provided — our layout (design 14 D2) pins both
  explicitly so the cache stays self-describing.
- **macOS pre-allocation (live finding 2026-10-20):** upstream's `allocate()`
  (`downloader/utils.rs`) is `#[cfg(target_os = "linux")]`-only; on macOS it
  returns `pre allocation not implemented` before any byte moves. Fixed in our
  fork `noosxe/gog-warp` rev `30a8303b` (F_PREALLOCATE → F_ALLOCATEALL →
  `set_len` ladder), offered upstream as playtron-os/gog-warp#5. gogo pins the
  fork until that lands; the pin stays a deliberate-commit git rev either way.

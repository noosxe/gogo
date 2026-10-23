# 10 — macOS app bundles (`gogo mac-app`)

Convert a downloaded GOG **Windows offline installer** into a **self-contained
macOS `.app`** that double-click launches and can be dropped into
`/Applications`. Grounded in discovery `docs/13` (landscape, licensing,
installer mechanics — gathered 2026-10-18).

**Boundary (README non-goals still hold):** this is a one-shot *artifact
conversion*, not install management. gogo tracks no installed-library, does no
launching beyond the generated launcher stub, and never updates a converted
game — `gogo update` still manages installer files, not bundles.

**Platform:** macOS only for now. On Linux the subcommand exists but fails
fast with exit 2 and a one-line explanation (a Linux counterpart is future
work, §10).

## 1. CLI surface (03 §1 carries the user-facing summary)

```
gogo mac-app <installer-path> [--name <title>] [--runtime <auto|brew|gptk|path>]
             [--runtime-path <dir>] [--out <dir>] [--json] [--yes]
```

- `<installer-path>` — the `setup_*.exe`; sibling `.bin` parts are picked up
  automatically (they must sit in the same directory; 13 §2).
- `--name` — bundle title (defaults to a sanitized installer stem:
- `--name` — bundle title. Default: derived from the installer stem —
  `setup_` stripped, separators spaced (`setup_heroes3_gold.exe` →
  `Heroes3 gold`); slug rules in §5.
- `--runtime` — Wine source (§3). Default `auto`.
- `--out` — output directory for `Title.app` (default: cwd).
- `--yes` — skip the size confirmation (13 §4: bundles land at
  hundreds of MB; without `--yes` on a TTY, print the plan and confirm, mirroring
  the `download --all` rule, 03 §3).
- `--json` / `--json-progress` — same NDJSON channel as downloads (07 §4),
  new event variants (§8).

Automation rules apply unchanged: results on stdout, progress on stderr,
stable exit codes, no prompts unless `--yes` is owed.

## 2. Pipeline

> **Amended 2026-10-23 (design 16):** in the default (non-portable) bundle the prefix
> and the game files leave the bundle. gogo builds them under
> `~/Library/Application Support/gogo/games/<gogid>/{prefix,game}` (design 16 §2, §4),
> the bundle carries `gogo-launch.toml` + a generated `launch.conf` (design 16 §1), and
> nothing is written into `Foo.app` after signing. Steps 2–4 below describe the
> pre-#66 fat bundle. *#84:* `--portable` no longer puts a prefix in the bundle either.
> Its launcher creates one on the Mac it runs on (design 16 §10).

```
resolve runtime → stage prefix → silent install → locate entry point
  → write launcher + Info.plist → ad-hoc codesign → summary
```

1. **Resolve runtime** (§3) — a directory containing a runnable Wine (probed,
   never assumed).
2. **Stage prefix** — fresh `WINEPREFIX` at
   `Title.app/Contents/Resources/prefix`; `wineboot -u` initializes it.
3. **Silent install** — `wine <installer> /VERYSILENT /SUPPRESSMSGBOXES
   /NORESTART /DIR="C:\\GOG\\<name>"` with the staged prefix. Installer exit
   code non-zero → fail loud (exit 7, `InstallerFailed`, keep partial bundle
   for diagnosis, say so in the error).
4. **Entry point** — parse `goggame-*.info` under
   `prefix/drive_c/GOG/<name>/`, prefer `primaryTask`/first `gameTask`
   `.exe` path (`.lnk` is resolved to its target via prefix scan; if nothing
   resolves: fail loud with the directory listing in `--json` output).
   Written to `Resources/launch.conf` (`wine=`, `game=`, `prefix=`).
5. **Launcher** — `Contents/MacOS/<name>`: a small `#!/bin/bash` stub that
   `cd`s relative to `$0`, sources `launch.conf`, sets `WINEPREFIX`, and
   `exec`s the bundled wine on the game exe. No absolute paths to the
   conversion location — the bundle is relocatable *after* creation (13 §6.4).
6. **Info.plist** — hand-written XML: `CFBundleIdentifier
   com.noosxe.gogo.<slug>`, `CFBundleExecutable <name>`, `CFBundleName`,
   `CFBundlePackageType APPL`, `LSMinimumSystemVersion` from the runtime's
   requirement, `NSHighResolutionCapable`. No new dependency.
7. **Codesign** — `codesign --force --deep --sign - Title.app` (ad-hoc;
   mandatory on arm64). Locally created → no quarantine xattr → no
   notarization needed. Verify with `codesign --verify` before declaring
   success.

## 3. Runtime resolution

> **Amended 2026-10-19 (design 12):** runtime resolution grows a **catalog + store**
> layer — `--runtime` accepts catalog ids (`staging` / `devel` / `stable`),
> downloads are cached under `~/Library/Application Support/gogo/runtimes/`, and
> selection is interactive and recipe-aware (recipe `wine.version` is advisory,
> design 12 §6–§7). The table below remains the contract for **local** resolution
> (brew / gptk / path), now the fallback lanes; `brew`'s "recommended default"
> status passed to `wine-staging` (catalog, docs/15 §2).

`--runtime` (default `auto`; config `mac_app.runtime`, 06 gains a `[mac_app]`
section when implemented):

| mode | source | notes |
|---|---|---|
| `brew` | `$(brew --prefix)/opt/wine/bin` (legacy) **or** `$(brew --prefix)/bin` (cask-linked — `gcenx/wine/game-porting-toolkit` is the 2026 install path; WineHQ casks disabled 2026-09-01, Gatekeeper; live finding, docs/13 §6.2) | fallback lane (design 12 §5); wine-crossover casks are gone (docs/15 §1) — 2026-10-19 |
| `path` | any dir containing `bin/wine*` via `--runtime-path` | escape hatch; probed, fail-loud if `wine` doesn't run |
| `gptk` | user-supplied wine tree **with GPTK libs already overlaid** via `--runtime-path` (live finding, docs/13 §6.2: the raw Apple dmg ships no Wine — only D3DMetal + DX overlay libs) | Apple EULA is the user's; gogo never downloads or bundles it (13 §3) |
| `auto` | `brew` if present, else a mounted GPTK volume, else error | error message names all three remedies |

Resolution records which runtime was used into `Resources/runtime.txt` —
debugging aid and honesty marker (GPTK vs wine changes support expectations).

> **Amended 2026-10-23 (design 16):** the default bundle **references** its runtime
> by store id (`runtime = "<id>/<ver>"` in `gogo-launch.toml`) instead of copying it.
> Renderers that replace Wine builtins (DXMT) come as a derived store runtime (design 16
> §3). Copying, as described in the next paragraph, remains only for `--portable` (#84,
> design 16 §10: the engine, its dylibs, the game and the verbs' files).

**The runtime is copied into the bundle** (`Contents/Resources/wine/`):
self-contained by construction; hundreds of MB per game is the accepted cost
(13 §4). A shared-runtime variant is future work (§10).

## 4. Errors & exit codes

New taxonomy `MacAppError` (07 §1 layout): `RuntimeNotFound`,
`RuntimeInvalid`, `InstallerFailed { exit }`, `EntryPointNotFound`,
`Codesign { stderr }`, `UnsupportedPlatform`, `Io`.

Normative: **`MacAppError::*` → exit 7** (new; severity-ordered after
disk=6, 03 §5 / 07 §2). Usage errors still exit 2 (bad flag values, non-macOS
platform).

## 5. Naming & paths

- Bundle: `<name>.app` in `--out`; refuse to overwrite an existing bundle
  without `--yes` (same confirmation semantics as download overwrite rules).
- Windows-side install dir fixed: `C:\GOG\<name>` — deterministic, no spaces
  after slugification (`name.to_lowercase()` ASCII-folded, non-alphanumerics
  → `-`, trimmed).

## 6. Events (`--json-progress`, 07 §4 channel)

New `ProgressEvent` variants, externally tagged as usual:
`RuntimeResolved { kind, path }`, `PrefixStaged`, `InstallStarted`,
`InstallFinished { bytes }`, `BundleAssembled`, `Codesigned`. `Done` remains
terminal.

## 7. Testing (08 conventions)

- **Unit:** slug/name derivation; `Info.plist` + launcher templating (golden
  strings); `goggame-*.info` → entry-point selection (primary vs first task,
  `.lnk` fallback); runtime probe logic with injected paths.
- **Integration:** full pipeline against a **fake runtime** — a shell script
  named `wine` that logs argv and exits 0 (no network, no real Wine); asserts
  bundle layout, `launch.conf`, plist fields, installer argv exactness.
- **Live smoke (manual, opt-in):** env-gated like 08 §5; requires real brew
  wine + a user-provided installer. Not CI.

## 8. Security & licensing of produced bundles

- Wine LGPL notice + source pointer written to
  `Contents/Resources/LICENSE-wine.txt` at bundle time (13 §3).
- GPTK-based bundles get an extra `README-GPTK.txt`: Apple's EULA applies to
  the embedded runtime; redistribute at your own risk (it is the user's
  copy of a user-downloaded artifact).
- No telemetry. Network after installer staging is limited to the **named
  exceptions** (design 12 D8): runtime/tool downloads, recipe `files:`, and
  winetricks verb fetches; the installer itself is offline.

## 9. gogo-core layout (01 §1 tree gains)

```
├── macapp/              # mac-app conversion (design 10; macOS only)
│   ├── mod.rs           # pipeline orchestration, plan → execute
│   ├── runtime.rs       # resolution + probe (brew/gptk/path), binary-name sniffing
│   ├── installer.rs     # silent InnoSetup invocation, .bin sibling check
│   ├── info.rs          # goggame-*.info parsing → entry point
│   ├── bundle.rs        # .app layout, Info.plist, launcher stub, launch.conf
│   ├── lutris.rs        # consume Lutris installer scripts: client + cache (design 11)
│   ├── interpret.rs     # script JSON → declarative plan (overrides, file ops, flags)
│   └── error.rs         # MacAppError taxonomy (exit 7)
```

`gogo-cli` wires the clap variant and maps `MacAppError` → exit 7; no domain
logic in the frontend.

## 10. Future (explicitly out of scope here)

- Per-game recipes (registry tweaks, installer flags, entry-point
  overrides from a community repo) — now designed in **design 11**.
- Shared-runtime bundles (tiny `.app` referencing the store) — the **download
  store** itself is now v1 (design 12 §4); reference-style bundles remain future.
  *(Amended 2026-10-23: now designed in design 16.)*
- DXVK/MoltenVK toggle for non-GPTK runtimes (licenses permit; 13 §3).
- MojoSetup branch for mac-native installers (13 §2) — extraction-only, no
  Wine.
- Linux counterpart (prefix + `.desktop`); Windows never (README non-goal).

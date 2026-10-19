# 14 — Recipe Systems (per-game community knowledge for making games run)

**Research date:** 2026-10-19
**Method:** Primary documentation and source review of the established per-game
configuration systems: Lutris installer scripts, Winetricks verbs, Bottles
dependency manifests, PlayOnLinux/Phoenicis scripts, Porting Kit wrappers and
CrossOver CrossTies. Follow-up to docs/13 (macOS app bundles): `gogo mac-app`
works end-to-end for well-behaved titles (live-verified with Pharaoh Gold,
2026-10-18); this doc surveys how the community encodes the knowledge that
makes *everything else* work, and what gogo should adopt.

## 1. The problem: installers succeed, games still fail

A silent Inno Setup install plus a Wine prefix gets the files on disk. What it
does not get right, for a large fraction of the Windows catalogue:

| Failure class | Typical fix | Example |
|---|---|---|
| Old DirectDraw/renderer engines | `ddraw.dll` override (`native,builtin`) or registry renderer pin | 1997–2003 isometric titles |
| Installer dies silently under `/VERYSILENT` | add undocumented GOG flag `/NOGUI`, use `/SILENT` | Cuphead, SW: Battlefront II (encoded in Lutris GOG scripts) |
| Wrong default resolution / windowed | registry values under the game's `HKCU` key | Impressions games (Pharaoh/Caesar), LucasArts |
| Needs a newer C runtime / d3dcompiler | `vcrun2019`, `d3dx9` redistribute | post-2015 titles |
| Launcher stub exits instead of game | launch the real exe with args | GOG Galaxy-dependent launchers |
| ESYNC/Msync, GPU quirks | env vars (`WINEESYNC`, `MESA_*,` `__GL_*`) | varied |

None of this is expressible today: `gogo mac-app` has exactly one strategy.
The knowledge exists — it lives in per-game community databases. The question
is which encoding and distribution model gogo should reuse.

## 2. Systems survey

### 2.1 Lutris installer scripts (the corpus leader)

Lutris games are configured by community-contributed YAML scripts
(`docs/installers.rst` in the Lutris repo, read 2026-10-19):

- **Sections:** `game` (exe, args, prefix, arch, working_dir), `files`
  (download/user-supplied inputs), `installer` (ordered steps), `wine`
  (prefix options, `overrides`), `system` (env vars).
- **Step vocabulary (declarative):** `task` (runner tasks: `wineexec`,
  `create_prefix`, …), `execute`, `extract`, `merge`/`copy`, `move`,
  `write_file`, `write_config`, `chmodx`, `insert_disc`, … — plus arbitrary
  `execute: command:` shell.
- **Variables:** `$GAMEDIR`, `$CACHE`, `$USER`, `$RESOLUTION[_WIDTH|_HEIGHT]`,
  `$WINEBIN`, `$INPUT_*`, script-defined `variables:`, `$SCRIPTDIR`.
- **Distribution:** lutris.net API serves installers by slug; the client fetches
  and runs them. Thousands of GOG games have scripts; the GOG ones drive GOG's
  own offline installers through `wineexec` and encode the `/NOGUI`-class flags.
- **Trust model: none.** Scripts are arbitrary YAML-embedded commands running
  with user privileges. Lutris is a GUI app where users accept this; a CLI
  copying that model wholesale would be a remote-code-execution channel.

**Takeaways:** the YAML shape, the variable substitution model, and the
per-GOG-installer flag lore are exactly right; the arbitrary-shell escape
hatch is exactly wrong for gogo.

### 2.2 Winetricks

The canonical per-game fix database. Each verb is a **shell function**
(`w_metadata` + `w_<verb>_load`) in one giant script — arbitrary code by
construction, executed inside the user's prefix.

- **Takeaway:** do not adopt the format. Do steal the *taxonomy*: winetricks
  verbs are effectively the industry-standard names for "what a game needs"
  (`dlls`, `fonts`, `runtimes`, `settings`). A gogo recipe should be able to
  express the same needs declaratively; bridging to the real winetricks
  binary (when the user has it) is a future opt-in, not a v1 dependency
  (gogo never ships or downloads winetricks — licensing stance of docs/13 §3).

### 2.3 Bottles dependency manifests (closest distribution model)

`bottlesdevs/dependencies` is a community GitHub repo of per-component JSON
manifests (`Name`, `Version`, `Executable`, `Steps`, …). The Bottles client
fetches the repo and installs components by pinned URL + checksum. No shell —
the client interprets a small step vocabulary, and binary downloads are
checksum-pinned.

- **Takeaways:** GitHub-repo-as-registry with client-side interpretation is a
  proven, low-infrastructure model (no API to run, forks/PRs for review, full
  history). Checksum-pinned binary fetching is the right pattern for the
  eventual redistributable support — deferred in v1 (see §4).

### 2.4 PlayOnLinux / Phoenicis scripts

Per-app scripts in a community git repo, but the script *engine* is a
JS/DSL-like interpreter — arbitrary logic again, plus tight coupling to the
GUI wizard model. Nothing to adopt beyond the "repo of per-app files" shape.

### 2.5 Porting Kit wrappers / CrossOver CrossTies

Porting Kit distributes opaque per-game wrapper "ports" (closed format, GUI
install — docs/13 §1). CrossOver's CrossTies are per-application XML install
recipes with a versioned, undocumented-ish vocabulary, proprietary and tied to
CrossOver's runtime. Not usable, but they validate the premise: the commercial
standard is exactly "per-game declarative recipe interpreted by the client".

## 3. What gogo adopts

| Aspect | Choice | Source |
|---|---|---|
| Format | YAML, `schema: gogo-recipe/1`, one file per game | Lutris (2.1) |
| Key | **GOG game id** (from `goggame-<id>.info`, which gogo already parses — design 10 §2); slug as informational fallback | gogo-native |
| Vocabulary | small whitelisted verb set (registry, file ops, env/args/exe, installer flags) — **no arbitrary command execution in v1** | Bottles model, not Lutris's `execute` |
| Variables | `$PREFIX`, `$INSTALL`, `$BUNDLE`, `$SLUG` | Lutris, reduced to prefix-scoped reality |
| Distribution | community git repo (`noosxe/gogo-recipes`), fetched as a branch tarball into a local cache; local config dir overrides for authoring | Bottles (2.3) |
| Binary downloads | **out of scope v1** (no third-party fetches); when added: pinned URL + sha256 + explicit license note per Bottles | docs/13 §3 licensing stance |
| Exec verb | out of scope v1; future, gated behind `--allow-recipe-exec` | trust model |
| Wine runtime needs | advisory `runtime:` section (min version, DX backend hint) — gogo never downloads runtimes (13 §3) | gogo design 10 §3 |

Full contract: **design 11** (`docs/design/11-recipes.md`).

## 4. Trust and licensing notes

- Recipes are **data, not code**: gogo interprets a closed verb set; a recipe
  cannot run a shell command in v1. Path traversal out of the bundle is a
  validation error, not a behaviour.
- The recipes repo is reviewed like any code PR; recipe files carry a
  `revision:` and `notes:` so `gogo recipes show` can present provenance.
- gogo ships no binaries and the v1 recipe vocabulary performs no network
  fetches, so the docs/13 §3 licensing matrix is unaffected.
- Windows-redistributable steps (`vcrun2019`, `d3dx9`) are the known v2 gap;
  the Bottles pattern (pinned URL + checksum + license field) is the
  designated shape when added.

## 5. Open questions carried into design 11

1. Fetch mechanics without adding a git dependency → branch tarball over the
   existing HTTP stack, revision-keyed cache (11 §3).
2. Registry writes without a shell → `wine reg.exe` invocation through the
   resolved runtime (fake-runtime testable, 11 §7).
3. Entry-point override vs `goggame-*.info` → recipe `launch.exe` wins (11 §5).
4. YAML crate choice — `serde_yaml` is unmaintained (archived 2024); use the
   maintained fork `serde-yaml-ng` unless cargo-deny objects (11 §8).

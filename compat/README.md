# compat/ — gogo's per-game compat DB (L3)

One `<gogid>.toml` per GOG product id, compiled into gogo at build time
(`crates/gogo-core/build.rs`). Schema: docs/19 §6.4, parsed by
`crates/gogo-core/src/macapp/compat.rs`. The same schema is used by the user
override file `~/Library/Application Support/gogo/compat/<gogid>.toml` (L4),
which wins over this directory field by field (design 16 decision 5).

```toml
# every key, for illustration (the real Rebel Galaxy entry is 1435582019.toml)
title    = "Rebel Galaxy"
exe      = "RebelGalaxyGOG.exe"
renderer = "dxmt"                     # wined3d | dxmt[/ver] | cnc-ddraw[/ver] | d3dmetal[/ver]
runtime  = "winecx/24.0.7_7"          # store <id>[/<ver>]
verbs    = ["d3dcompiler_43"]         # winetricks verbs, added to the L1 auto ones
dll_overrides = { ddraw = "n,b" }
env      = { WINEMSYNC = "1" }
registry = [{ key = 'HKCU\Software\Wine\Direct3D', name = "renderer", type = "REG_SZ", data = "gl" }]
args     = ["-windowed"]
variant  = "Rebel Galaxy (safe mode)" # a goggame playTasks name; see below
replace_command = { from = "GoGLauncher.exe", to = "RebelGalaxyGOG.exe" }
status   = "boots"                    # unknown | broken | boots | plays
notes    = "boots to the main menu"   # one line; shown as "known broken: <notes>"
lane     = "depot"                    # depot | installer: the build lane status/notes/tested were seen on
tested   = [{ runtime = "winecx/24.0.7_7+dxmt-v0.80", host = "m4pro/macos-27.0.1", date = 2026-10-22 }]
source   = "gogo-host"                # gogo-vm, or "umu-protonfixes:<file>@<sha>"
```

Maps (`env`, `dll_overrides`) merge per key across layers; every other field
is replaced whole, except `verbs` (#128): an entry's verbs **add** to the L1
auto verbs picked from the game's imports (in order, no duplicates). To take
one away write `-<verb>` (`verbs = ["-xinput"]`), and `-*` drops all of them
(`verbs = ["-*", "d3dx9"]` = exactly `d3dx9`). The same goes for L4 over L3.
`gogo mac-app compat <gogid>` shows each verb with the layer that added it.

`variant` (#85) picks one of the game's runnable `goggame-*.info` `playTasks`
(TDR 2000's `-f` / `-s`, an alternate exe, a settings tool) instead of the
default one. The build bakes them into the bundle's `gogo-launch.toml` as
`[[variant]]`s; `gogo mac-app compat <gogid>` lists them and marks the one
that runs. Setup resolves it, like every other L4 value: an edit restamps,
so the next launch runs setup, which swaps in that variant's exe, working
dir and args. An `exe` / `args` in L4 still win over a variant. An unknown
name is an error that lists the names there are (setup fails, and the
launcher logs it and runs the last applied config), never a silent fallback
to the default. It is meant for L4; an L3 `variant` works the same way, below
L4's.

`lane` (#56) scopes `status`, `notes` and `tested` to one build lane, for a
game that differs between them: Pharaoh asks for its CD when built from the
depot and doesn't when built from the offline installer. On a bundle from the
other lane `doctor` reads it as untested and quotes it (`untested from the
offline installer (the depot build: known broken: …)`). Without `lane` the
status counts for both. A layer that sets `status` sets `lane` with it, so an
L4 `status` without one drops L3's.

Seed (#75), see docs/19 §5:

- `source = "gogo-host"`: a human-watched host pass of a real bundle
  (`scripts/host-pass.sh`); `gogo-vm`: a `scripts/vm-test.sh` smoke run. The VM
  has no Metal 3 and no OpenGL, so a VM row never vouches for DXMT, D3DMetal
  or OpenGL rendering.
- `tested` rows carry no renderer: the runtime id says it, a derived
  `winecx/24.0.7_7+dxmt-v0.80` for DXMT/D3DMetal, the base id otherwise.
  `host` is `m4pro/macos-<ver>` or `vm/macos-<ver>`.
- `status`: `plays` = rendering, input and audio judged by eye; `boots` =
  reaches the game's own UI or first-run dialog; `broken` also covers "runs
  but unplayable", with the reason in `notes`.
- `source = "umu-protonfixes:<file>@<sha>"`: hand-translated from that file
  of [umu-protonfixes](https://github.com/Open-Wine-Components/umu-protonfixes),
  BSD-2-Clause (`NOTICE-umu-protonfixes`); Proton-only knobs are dropped. Not
  run by gogo, so no `status`.

Every file here is tested (`compat_dir_entries_are_sourced_and_tested`): a
numeric name, no unknown keys, a `source`, and a `tested` row for any
`status` other than `unknown`. To try an entry before adding it
here, point `GOGO_COMPAT_DIR` at a directory of `<gogid>.toml` files: they
shadow the compiled-in ones (and are still L3). What the build bakes and what
setup applies is in docs/design/16 §5 (#74).

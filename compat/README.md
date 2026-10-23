# compat/ — gogo's per-game compat DB (L3)

One `<gogid>.toml` per GOG product id, compiled into gogo at build time
(`crates/gogo-core/build.rs`). Schema: docs/19 §6.4, parsed by
`crates/gogo-core/src/macapp/compat.rs`. The same schema is used by the user
override file `~/Library/Application Support/gogo/compat/<gogid>.toml` (L4),
which wins over this directory field by field (design 16 decision 5).

```toml
title    = "Rebel Galaxy"
exe      = "RebelGalaxyGOG.exe"
renderer = "dxmt"                     # wined3d | dxmt[/ver] | cnc-ddraw[/ver] | d3dmetal[/ver]
runtime  = "winecx/24.0.7_7"          # store <id>[/<ver>]
verbs    = ["d3dcompiler_43"]         # winetricks verbs
dll_overrides = { ddraw = "n,b" }
env      = { WINEMSYNC = "1" }
registry = [{ key = 'HKCU\Software\Wine\Direct3D', name = "renderer", type = "REG_SZ", data = "gl" }]
args     = ["-windowed"]
replace_command = { from = "GoGLauncher.exe", to = "RebelGalaxyGOG.exe" }
status   = "boots"                    # unknown | broken | boots | plays
notes    = "boots to the main menu"   # one line; shown as "known broken: <notes>"
tested   = [{ runtime = "winecx-24.0.7_7", host = "m4pro/macos-27.0.1", date = 2026-10-22 }]
source   = "gogo-vm"                  # or "umu-protonfixes:<file>@<sha>"
```

Maps (`env`, `dll_overrides`) merge per key across layers; every other field
is replaced whole. Entries are seeded in #75. To try an entry before adding it
here, point `GOGO_COMPAT_DIR` at a directory of `<gogid>.toml` files: they
shadow the compiled-in ones (and are still L3). What the build bakes and what
setup applies is in docs/design/16 §5 (#74).

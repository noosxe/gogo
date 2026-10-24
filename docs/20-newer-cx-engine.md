# 20 — A newer CrossOver-source engine, and a second runtime source (#83)

**Research date:** 2026-10-24
**Builds on:** docs/19 §3.1, §6.1 (watch items), §7, §8.2 Q4; catalog arch fields (#82)
**Status:** research plus a non-default catalog entry, `winecx26` (§3). Facts are marked **verified** (GitHub
API, a source read, or a binary inspection) or **reported** (the project's own README, not
re-checked).

## 0. Summary

- **No Gcenx/Sikarugir CX 25/26 engine exists.** The newest CrossOver-source engine
  in Sikarugir `Engines` is still `WS12WineCX24.0.7_7`, which is gogo's pin (**verified**,
  release asset list).
- **Heroic's `wine-crossover` mirror can't be a second source for our pin.** It has a
  single asset, CX **23.7.1**-1, so it is neither the same bytes nor the same version
  (**verified**).
- **Tested (§3):** CX 26.3 passes headless `wineboot`, the depot, teardown,
  gog-support and DXMT VM cases, and the Pharaoh installer itself. But installer-lane
  Pharaoh spins with no window in the VM, where CX24 shows it. So `winecx26` is a
  non-default catalog entry, waiting for a host pass.
- **There is a CX 26.3 (Wine 11.0) engine:** Highball's `highball-engine`. CI builds it
  from CodeWeavers' LGPL `crossover-sources-26.3.0.tar.gz`, pinned by sha256, plus 24
  Highball patches. It is LGPL-2.1 and each release asset is sha256-pinned. This is the
  only candidate worth a catalog entry and a test (§1.2).
- **`WineSikarugir11.0` is gated on purpose.** Its `ntdll.so` calls
  `getenv("SikarugirAppWine11")` right after it loads the apiset, and calls `exit(1)` when
  that variable is unset (**verified**, disassembly, §2). That is the "child exits after
  apiset load" from docs/19. The Sikarugir launcher sets the variable. gogo should not set
  it itself to get round the gate (§2.3).

## 1. Candidates

### 1.1 Survey

| Candidate | Version / Wine base | Arch, WoW64 | Asset, size | Licence, attribution | Signing | Checksums |
|---|---|---|---|---|---|---|
| Sikarugir `WS12WineCX24.0.7_7` (today's pin) | CX 24.0.7, wine-9.0 | x86_64 (Rosetta); new-style WoW64: `lib/wine/` has `i386-windows`, `x86_64-windows`, `x86_64-unix`, no `i386-unix`; `wine-preloader` present | `Sikarugir-App/Engines` `v1.0`, 172,348,356 B | LGPL-2.1 (Wine); the repo has no LICENSE file; credit Gcenx/Sikarugir | ad-hoc (`com.codeweavers.CrossOver.wineloader`) | GitHub asset digest `sha256:203f9e9f…` |
| Sikarugir `WS12WineSikarugir11.0_1` | wine-11.0 + Sikarugir patches | x86_64, new-style WoW64; **no `wine-preloader`**, `wine` has a 4 GB `__PAGEZERO` | same release, 167,322,744 B (2026-10-01) | LGPL-2.1; credit Gcenx/Sikarugir | ad-hoc | asset digest `sha256:67e29fb3…` |
| Sikarugir `WS12WineSikarugir10.0_8` | wine-10.0 + Sikarugir patches | x86_64 | same release, 166,321,980 B | LGPL-2.1 | ad-hoc (assumed, same pipeline) | asset digest |
| Heroic `wine-crossover` `23.7.1-1` | CX 23.7.1 (wine 8.0, **reported**) | x86_64 | `Wine-Crossover-23.7.1-1.tar.xz`, 200,466,784 B | LGPL; credit CodeWeavers, Heroic | not checked | asset digest `sha256:7e1ef682…` |
| **Highball `highball-engine`** Intel line | **CX 26.3, wine-11.0** + 24 patches (`patches/`) | x86_64 (Rosetta); configure `--enable-archs=i386,x86_64`, i.e. **new-style WoW64** | `engine-wine-11.0-20261014T003801Z-0024-app-packages-0023-spatial-mixer.tar.gz`, 334,884,359 B (includes wine-mono, wine-gecko, DXMT v0.80) | LGPL-2.1+ (repo LICENSE); `NOTICE.md` lists each component's licence; credit CodeWeavers (sources), Gauthier Piarrette (patches, build) | ad-hoc: the workflow has no `codesign` step and the linker signs ad-hoc | asset digest `sha256:1d1a130c…` plus a `.sha256` asset |
| Cider `engine-cider-cx26.3-r1` | CX 26.3, wine-11.0 + Highball 0001–0013 + 2 Cider patches | x86_64 | `cider-cx26.3-r1-x86_64.tar.xz`, 248,783,676 B | **repo has no licence** (GitHub API 404); 1 star, created 2026-09-27 | not checked | asset digest plus a `.sha256` asset |
| Highball `highball-engine` arm64 line | WineHQ 11.18 + FEX 2610 (ARM64EC) | **arm64**, no Rosetta | `engine-arm64-wine-11.18-…-r6.tar.gz`, 402,870,000 B | LGPL / MIT per NOTICE | needs Highball's Developer-ID-signed loader (§1.3) | asset digest |
| CrossOver 26.3 (commercial) | wine-11.0 | x86_64 | 387 MB demo zip (Gcenx's MacPorts `crossover` port) | proprietary | Developer ID (**reported**) | MacPorts sha256 |

Sources: GitHub releases API for each repo (2026-10-24); `highball-engine/inputs.json`,
`.github/workflows/build.yml`, `NOTICE.md`; `otool -l`, `codesign -dv` and `ls` on the
two Sikarugir engines that were already on disk from the docs/19 runs.

Also checked and ruled out: `Sikarugir-App/wine` and `Gcenx/wine` (only `master`, plus
`stable`/`oldstable` on Gcenx's; no CX branch), `Sikarugir-foss-sources` (the Configure
app only), Gcenx's `homebrew-wine` (GPTK cask only) and `macports-wine` (its `crossover`
port repackages the commercial demo).

### 1.2 Highball CX 26.3 — what using it would mean

- **Source provenance is clean.** CodeWeavers' tarball
  `media.codeweavers.com/pub/crossover/source/crossover-sources-26.3.0.tar.gz` answers
  `HTTP 200`, 149,054,023 B (**verified**, HEAD request today; docs/19's earlier 403 was
  that probe, not a block on the file). Highball pins its sha256 and builds on
  `macos-15-intel` with mingw-gcc.
- **It is not pristine CX.** Its 24 patches include CrossOver-tree reverts (msync),
  macOS fixes and fiber/QoS handling. Each one has a row in `patches/README.md` with its
  upstream status. That is more patching than Sikarugir's CX24 carries.
- **New-style WoW64, like our CX24.** The CX24 engine we ship is new-style WoW64 too (no
  `i386-unix`) and still runs 32-bit Inno installers without the stack overflow that
  wine-staging hits (docs/19 §5.4). So WoW64 mode alone does not predict the #56 failure.
  CX 26.3 has to be tested on the installer lane (Pharaoh) before it can be the default.
- **Frameworks.** Highball pairs it with Sikarugir's Wrapper `Template-1.0.11` (84.5 MB)
  for `DYLD_FALLBACK_LIBRARY_PATH` and GStreamer. gogo's winecx entry already carries a
  Template `Frameworks` set (`4b5f89ee…`, Template 1.0.24), which should satisfy the same
  `@rpath` names. Confirm with `otool -L` once the archive is local.
- **Layout.** The tarball is `engine/{bin,lib,share}` plus `renderers/`, `frameworks/`
  and `tools/`, not `wswine.bundle/`. A catalog entry needs `subdir = "engine"` (or the
  equivalent) and has to map the result onto gogo's `Frameworks/wswine.bundle` layout.
- **Maintenance risk.** A young, fast-moving project: 55 releases in `highball-engine`, Intel builds every few
  days. Highball itself has 1.8k stars. Pin one asset by digest. A newer Highball build is
  a deliberate bump, like any catalog change.
- **Arch fields (#82):** `arch = X86_64Rosetta`, `vendor_signed = false`.

### 1.3 Related to docs/19 §7 (post-Rosetta)

Highball ships an **arm64 Wine 11.18 + FEX** engine today (preview, one game recipe).
Its loader (`spike/wineloader/main.c`, LGPL) is Wine's `loader/main.c` linked with
`-pagezero_size 0x170000000 -x86_64_layout_emulation`. It is then signed inside
`Highball.app` with Developer ID and a provisioning profile, because the low-memory
mapping needs Apple's cross-architecture entitlement. The engine's own `bin/wine` is a
symlink to that signed binary (**reported**, source comments). This is the "use a runtime
signed by whoever holds the entitlement, by reference" case from docs/19 §8.2 Q2. It
exists, but the signed half lives in another app's bundle, so gogo can't reference it
without Highball installed. Nothing to do until a standalone signed loader ships.

## 2. `WineSikarugir11.0` outside its launcher

### 2.1 What the Sikarugir launcher sets

The launcher (`Template-1.0.24.app/Contents/MacOS/Sikarugir`, universal, closed source;
`Sikarugir-foss-sources` holds only the Configure app) was read with `strings`
(**verified**):

- **Paths:** engine at `Contents/SharedSupport/wine/{bin,lib}`; `WINEPREFIX` from the
  wrapper; `PATH` = engine `bin` + `/usr/bin:/bin:/usr/sbin:/sbin`.
- **DYLD:** `DYLD_FALLBACK_LIBRARY_PATH` = `Contents/Frameworks` plus
  `/usr/lib:/usr/libexec:/usr/lib/system:/opt/wine/lib`; `GST_PLUGIN_PATH` points at
  `Frameworks/GStreamer.framework/…/gstreamer-1.0`.
- **No `WINELOADER`, no preloader:** neither the launcher nor the engine ships a
  `wine-preloader`.
- **Wine env:** `WINEDLLPATH`, `WINEDLLPATH_PREPEND` (renderer dirs under
  `Contents/Frameworks/renderer/{dxmt,d3dmetal,dxvk,d9vk,cnc_ddraw}`), `WINEMSYNC`,
  `WINEDEBUG`, `WINEDLLOVERRIDES` (`mscoree`/`mshtml`), `WINEBOOT_HIDE_DIALOG`.
- **Renderers:** `VK_DRIVER_FILES` (MoltenVK ICD), `MVK_CONFIG_*`,
  `DXMT_ALLOW_CROSS_PROCESS_SWAPCHAIN`, `CX_ROOT`, `CX_FWD_COMPAT_GL_CTX`,
  `ROSETTA_ADVERTISE_AVX`.
- **The gate variable `SikarugirAppWine11`,** set beside `WINEDLLPATH_PREPEND`.

### 2.2 Why the child exits

In the engine's `lib/wine/x86_64-unix/ntdll.so` (`wine-11.0 (Sikarugir)`), the
`load_apiset_dll` path ends like this (`otool -tV`, offsets from the 11.0_1 build):

```
1fb45  leaq  "SikarugirAppWine11", %rdi
1fb4c  callq _getenv
1fb54  je    1fc21          ; unset → 6cb60: exit(1)
1fb5a  …                    ; set → mprotect(localtime) and write a jump into it, continue
```

When the variable is unset, the process exits with status 1 and prints nothing. That
matches the docs/19 observation exactly. No env var, `Frameworks/` set or preloader that
gogo was missing would have changed it. When the variable is set, ntdll also patches libc's
`localtime` in place to jump to its own code. Our env has never contained it, which is
why Frameworks and DYLD fixes alone failed in the docs/19 run.

### 2.3 Can gogo run it?

Technically yes: export one variable. **Recommendation: no.**

- The gate is deliberate. Its only job is to stop the engine running outside Sikarugir.
  Setting the variable from gogo goes round the maintainer's intent. The LGPL would allow
  rebuilding from source, but gogo shouldn't quietly flip a switch the publisher added to
  keep the engine in Sikarugir.
- The engine's own value to gogo is small anyway: a non-CX Wine 11 base with Sikarugir
  patches (DXMT ext-escape). The CX lineage is what fixed our 32-bit installers and
  OpenGL (docs/19 §5).
- If a Wine 11 Sikarugir engine is ever wanted, ask Gcenx (Sikarugir issues) whether
  third-party launchers may use it, then record the answer here. Until then it stays out
  of the catalog, and docs/19's watch item closes as "gated by design".

## 3. Test results (2026-10-24, after the go)

All on the host (M4 Pro, macOS 27.0.1) and the `gog-golden` VM (macOS 27.0). The host
runs used a scratch `GOGO_HOME` that was an APFS clone of the real state, with the
cloned prefixes deleted first, so every build made a fresh prefix.

### 3.1 Headless `wineboot -u` (scratch prefix, no window)

| Engine | Env | Result |
|---|---|---|
| `winecx26` (Highball CX 26.3) | gogo's winecx `Frameworks/` on `DYLD_FALLBACK_LIBRARY_PATH`, `mscoree=` | exit 0, 14 s; 839 `system32` + 877 `syswow64` files; no FreeType/GnuTLS errors |
| `winecx26` | same, **without** `mscoree=` | exit 0, 14 s: no wine-mono MSI loop, unlike CX24 (docs/19 §8.2 Q5) |
| `WineSikarugir11.0_1` | Frameworks, no `SikarugirAppWine11` | exit 1 after 3 s, `run_wineboot failed to start wineboot 1`: the gate (§2.2) |
| `WineSikarugir11.0_1` | same + `SikarugirAppWine11=1` | exit 0, 19 s; 836 + 876 files. Six `try_map_free_area mmap() … 0x7ffffff70000` errors that do no harm |

So WineSikarugir11 does run under gogo once the variable is set. That confirms §2.2 and
leaves §2.3's recommendation as it is.

Harness note: a timeout wrapper that runs `/usr/bin/perl` (or any SIP-protected binary)
strips `DYLD_*`, and Wine then can't find FreeType. Use a shell watchdog instead.

### 3.2 `gogo runtimes install winecx26` and builds

- Install: digest checked, the `engine/` tree moved into the store layout with the
  Template 1.0.24 `Frameworks/`, `wine --version` probe `wine-11.0`, 1.0 GiB unpacked.
- `wine --version` doesn't say CrossOver, so `Runtime::is_crossover()` now also asks the
  catalog (`CX_LINEAGE`). Without that, DXMT and D3DMetal refused to derive a runtime on
  it. With it, `--renderer dxmt` derives `winecx26/26.3-20261014+dxmt-v0.80`.
- Installer lane (#56): `setup_pharaoh_gold_2.1.0.15.exe` installs silently, exit 0,
  45 s, **no stack overflow**. cnc-ddraw is picked (L1).

### 3.3 VM matrix (#68 smoke cases, adapted to `winecx26`)

| Case | Bundle | Result |
|---|---|---|
| depot-lane | TAG (1207659013), depot, wined3d | `ok`: window up, `bundle_modified` false, no Gatekeeper dialog left |
| runtime (cx24 case) | same | `missing_libs` empty, no TCC prompt |
| teardown-kill (#103) | same, `--kill kill` | `teardown.ok` true |
| gog-support | Carmageddon TDR 2000 (1146738698) | `ok`, no `File.cpp:73` |
| renderer-l1-dxmt | Neo Cab (1669531175), `+dxmt-v0.80` | `running_no_window`, as expected in the VM (no Metal 3); DXMT logs `Using feature level D3D_FEATURE_LEVEL_11_0`; `missing_libs` empty: **pass** by the smoke check |
| installer-lane Pharaoh (cnc-ddraw) | Pharaoh Gold (1207659046) from the offline installer | **`running_no_window` after 45 s and after 120 s**: `Pharaoh.exe` at 100 % CPU, no window |
| same, `--renderer wined3d` | same | **`running_no_window`** after 90 s: not cnc-ddraw |
| baseline: same bundle on `winecx` (CX24) | same | `ok`: `Pharaoh.exe` window 1024×768 |

The Pharaoh regression has one clue. In 120 s the run log holds 5,496 copies of
`fixme:d3dkmt:NtGdiDdDDIOpenAdapterFromHdc (…): stub`, so the game seems to retry a
D3DKMT adapter open in a loop instead of creating its window. Whether real hardware
behaves differently (the VM's paravirtual GPU is no Apple GPU) is for the host pass. As
it stands, **CX 26.3 is not "at least as good" as CX24**: `winecx26` stays a selectable,
non-default entry.

## 4. Next steps

1. `winecx26` is now a **non-default** catalog entry (§3). Still to do: the user's host
   pass (`scripts/host-pass.sh`, with `--runtime winecx26`): Neo Cab on DXMT and D3DMetal,
   Snake Core on OpenGL, Rebel Galaxy 32-bit, and Pharaoh from the installer, the VM
   regression above. The default pin moves only if CX 26.3 is at least as good on all of
   them. If Pharaoh also spins on the host, look at the d3dkmt loop first: it may be
   one of Highball's patches or the CX 26.3 tree, and the place to report it is
   highball-engine.
2. No second source exists for `winecx/24.0.7_7`. To protect against the Sikarugir asset
   disappearing, keep the store's N-1 copy (docs/19 §6.1). The other option is building CX
   24.0.7 from CodeWeavers' tarball, using `highball-engine`'s workflow as the template.
   That is a larger job; file it only if the asset goes away.
3. Re-check Sikarugir `Engines` for a `WineCX25`/`WineCX26` asset when the next Sikarugir
   Template ships.

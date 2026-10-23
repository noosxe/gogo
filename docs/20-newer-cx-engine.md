# 20 — A newer CrossOver-source engine, and a second runtime source (#83)

**Research date:** 2026-10-24
**Builds on:** docs/19 §3.1, §6.1 (watch items), §7, §8.2 Q4; catalog arch fields (#82)
**Status:** research. No catalog change in this doc. Facts are marked **verified** (GitHub
API, a source read, or a binary inspection) or **reported** (the project's own README, not
re-checked).

## 0. Summary

- **No Gcenx/Sikarugir CX 25/26 engine exists.** The newest CrossOver-source engine
  in Sikarugir `Engines` is still `WS12WineCX24.0.7_7`, which is gogo's pin (**verified**,
  release asset list).
- **Heroic's `wine-crossover` mirror can't be a second source for our pin.** It has a
  single asset, CX **23.7.1**-1, so it is neither the same bytes nor the same version
  (**verified**).
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

## 3. Next steps

1. Highball CX 26.3 as a **non-default** catalog entry (`winecx26`, say), pinned by asset
   digest, after a go on the 335 MB download. Steps: headless `wineboot` into a scratch
   prefix, then the #68 VM matrix (`scripts/vm-test.sh`), then the user's host pass
   (`scripts/host-pass.sh`; Neo Cab on DXMT/D3DMetal, Snake Core on OpenGL, Pharaoh from
   the installer, Rebel Galaxy 32-bit). The default pin moves only if CX 26.3 is at least
   as good on all of them.
2. No second source exists for `winecx/24.0.7_7`. To protect against the Sikarugir asset
   disappearing, keep the store's N-1 copy (docs/19 §6.1). The other option is building CX
   24.0.7 from CodeWeavers' tarball, using `highball-engine`'s workflow as the template.
   That is a larger job; file it only if the asset goes away.
3. Re-check Sikarugir `Engines` for a `WineCX25`/`WineCX26` asset when the next Sikarugir
   Template ships.

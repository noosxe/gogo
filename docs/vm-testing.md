# Launch-testing `.app` bundles in a Tart VM

`gogo mac-app` bundles are launch-tested inside a throwaway macOS VM, never on the
developer's Mac: a launched game grabs focus and full-screens over whatever you were doing.
`scripts/vm-test.sh` wraps [Tart](https://tart.run) so that one command clones a clean
guest, runs the bundle like a freshly downloaded app, collects evidence and deletes the
clone. Strategy context: [design/08-testing.md §5.1](design/08-testing.md).

Findings and limits below were measured on 2026-10-22: host M4 Pro, macOS 27.0.1, Tart
2.40.1, guest `macos-golden-gate-base` (macOS 27.0, 26A428).

## Prerequisites (host, once)

| What | How |
|---|---|
| Apple Silicon Mac, macOS ≥ guest version | guest must not be newer than the host |
| Tart | `brew install openai/tools/tart`. The project moved from `cirruslabs` to `openai`, and the old `cirruslabs/cli` tap is stale and fails on Homebrew 7. Homebrew 7 also asks once for `brew trust --formula openai/tools/softnet`. |
| ~45 GB free disk | base image ≈ 41 GB on disk; clones are APFS copy-on-write and cost only what they write |
| `jq`, `shellcheck` | in the Nix dev shell; the script re-execs itself under `nix develop` if `jq` is missing |

## One-time golden image

```sh
scripts/vm-test.sh golden            # create, or re-provision an existing gog-golden
scripts/vm-test.sh golden --refresh  # delete and rebuild from the newest base image
```

Clones `ghcr.io/cirruslabs/macos-golden-gate-base:latest` (falls back to
`macos-tahoe-base`; override with `GOGO_VM_BASE`) to `gog-golden`, sets 4 CPU / 8 GB,
and provisions it over SSH. Idempotent:

- **SSH key login**: per-host key in `~/.local/state/gogo-vm/id_ed25519`, installed via
  the image's `admin`/`admin` password once (no `sshpass`; uses `SSH_ASKPASS`).
- **Rosetta**: installed if missing (the current image already ships it).
- **Gatekeeper back ON.** Cirrus images ship with `spctl` assessments *disabled*. Without
  this, `spctl` says "accepted" for anything and the test proves nothing.
- **Screen-capture consent pre-approved.** On macOS 15 and later, an SSH `screencapture`
  makes macOS show *"com.apple.sshd-session is requesting to bypass the system private window
  picker"* on top of the game. The script writes far-future dates into replayd's
  `ScreenCaptureApprovals` for `sshd-keygen-wrapper` (the TCC-responsible binary) to stop it.
- No sleep, no screensaver, no crash-reporter dialogs, no Software Update banners.
- Shut down from inside the guest (`shutdown -h`), not `tart stop`. A plain `tart stop`
  lost the provisioning once.

**No manual steps.** The Cirrus image already grants Accessibility, ScreenCapture,
PostEvent and AppleEvents in TCC to `sshd-keygen-wrapper` and `osascript`, with SIP off.
So `osascript -e 'tell application "System Events" to click at {x, y}'` works over SSH.
No `cliclick` and no click-through needed.

## Daily use

```sh
scripts/vm-test.sh run "/path/Game.app" --secs 30   # full clone → test → delete
scripts/vm-test.sh run Game.app --strict-gatekeeper # stop at the Gatekeeper dialog
scripts/vm-test.sh run Game.app --no-quarantine     # skip the download simulation
scripts/vm-test.sh run Game.app --keep              # keep the stopped clone for poking
scripts/vm-test.sh shell [Game.app]                 # interactive clone, deleted on exit
scripts/vm-test.sh status                           # golden, clones, running count (JSON)
scripts/vm-test.sh cleanup                          # stop + delete every gog-test-* clone
```

What `run` does:

1. `tart clone gog-golden gog-test-<slug>-<ts>`, then boot it headless with the results
   dir shared as `/Volumes/My Shared Files/out`.
2. Copies the bundle to `~/Applications` by streaming `tar` over SSH. A virtiofs
   share + `ditto` doesn't work: Wine prefixes contain absolute symlinks (`dosdevices/z:`
   → `/`, `drive_c/users/<you>/Desktop` → `/Users/<you>/Desktop`), and virtiofs fails
   those with ELOOP.
3. Sets `com.apple.quarantine` (`0081;<hex time>;Safari;`), then records
   `codesign --verify --deep --strict` and `spctl -a -vv`.
4. `open`s the bundle via LaunchServices, like a double-click, with Wine's stdout/stderr
   sent to the results dir. After 8 s it classifies Gatekeeper: `allowed`, `prompted`
   (process present but SIGSTOPped behind the first-launch dialog) or `blocked` (no
   process). Then it screenshots the dialog. Unless `--strict-gatekeeper` is set, it
   kills the held launch, strips quarantine (what "Open Anyway" does) and relaunches.
5. Waits `--secs`, then collects: process list, on-screen windows (CoreGraphics, from
   JXA), `screencapture -x`, Wine logs from the prefix, a `log show` slice (syspolicyd,
   XProtect, wine), crash reports and guest info.
6. Stops and deletes the clone, even on error or Ctrl-C.

**Two-VM limit.** macOS allows at most two macOS guests at once. `run`/`shell` take a slot
lock (`~/.local/state/gogo-vm/slot.lock`) and *queue* for up to `--wait-secs` (default 1800)
while two `tart run` processes exist. Linux VMs (Docker Desktop) don't count.

## Results (`target/vm-test/<slug>-<timestamp>/`)

Exit code: `0` ok · `1` app failed (Gatekeeper-stopped / exited / no window) · `2` usage ·
`3` infrastructure (Tart/VM/SSH; says nothing about the app). The last stdout line is the
path of `summary.json`, and the summary is printed just before it:

```json
{
  "status": "ok | running_no_window | exited | did_not_launch | gatekeeper_prompted | gatekeeper_blocked | infra_error",
  "exit_code": 0,
  "gatekeeper": "not_quarantined | allowed | prompted | blocked",
  "codesign_verify_rc": "1", "spctl_rc": "1",
  "launched": true, "running_after_secs": true,
  "windows": [{"owner": "wine", "pid": 839, "layer": 26, "w": 1024, "h": 768}],
  "processes": ["839 S C:\\GOG\\Pharaoh Gold\\Pharaoh.exe", "..."],
  "screenshot": ".../screenshot.png", "gatekeeper_screenshot": ".../gatekeeper.png",
  "files": {"codesign": "...", "spctl": "...", "stdout": "...", "stderr": "...",
            "prefix_logs": "...", "system_log": "...", "vm_log": "..."}
}
```

`ok` means "alive with a window". It does **not** mean "renders correctly", so always look
at `screenshot.png`. Other files: `gatekeeper.log`, `processes.txt`, `crash-reports.txt`,
`guest-info.txt`, `open.txt`.

## Known VM limitations

- **GPU is "Apple Paravirtual device"**: Metal GPU families Apple5 + Mac2 only, **no Metal 3**
  (no Apple7/8). So **D3DMetal / GPTK cannot work in the guest** (they need Metal 3), and
  DXVK/MoltenVK and wined3d-GL paths may behave differently from real hardware. Performance
  numbers mean nothing. Use the VM for "does it start, is it self-contained, what does
  Gatekeeper say", and real hardware for rendering and performance.
- **SIP is disabled** in Cirrus images, and TCC is pre-granted to `sshd`/`osascript`. That's
  needed for automation, but it isn't a stock Mac: anything SIP- or TCC-sensitive must be
  re-checked on real hardware.
- **Guest has its own Homebrew** (`/opt/homebrew`, only `tart-guest-agent`). It has no Wine,
  FreeType, XQuartz or MoltenVK, so missing-library bugs surface here.
- **Rosetta**: present. Launching an x86_64 Wine bundle on macOS 27 posts *"App Update
  Required — … includes a component that will not open in macOS 28"* (the Rosetta
  retirement notice). It shows up in screenshots.
- The display is 1024×768 (2048×1536 Retina), and fullscreen games take all of it.
- Boot can fail with `VZErrorDomain Code=1` if the disk is busy. The script retries 3×, and
  checks running state from the process table, never `tart list` (which opens the
  ASIF disk images and raced a booting VM for its lock).
- No audio verification. The window list can't read Wine window titles (they aren't
  exposed to Accessibility).

## What the first real run found (Pharaoh Gold, `--from-depot`, wine-staging 11.18, cnc-ddraw)

1. **Every gogo bundle's signature is invalid at birth.** `codesign` reports files
   added after signing: `prefix/{system,user,userdef}.reg`. wineserver flushes the
   registry lazily *after* the build's signing step. With Gatekeeper on, a quarantined
   bundle is therefore rejected as **"damaged and can't be opened. You should move it to
   the Trash."** That dialog has no "Open Anyway" button.
2. **Bundles aren't self-contained: FreeType.** In the guest, Wine logs *"Wine cannot
   find the FreeType font library"*, so the runtime picks it up from the host's Homebrew
   at build/run time. The game then shows a black fullscreen window plus an 11×58 px
   dialog, probably a message box whose text measured zero width.
3. **The prefix leaks the builder's identity.** `drive_c/users/<builder>/{Desktop,…}` are
   absolute symlinks into the builder's home. That breaks `codesign --strict` (ENOENT on
   other Macs, TCC "Operation not permitted" on the builder's own Mac) and embeds the
   username.

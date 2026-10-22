# AGENTS.md — instructions for coding agents working in this repo

**gogo** — a Rust CLI for GOG.com: sign in, list your library, download offline installers
(resumable, MD5-verified, rate-limited). Discovery research in `docs/01`–`docs/12`, the v1
implementation contract in `docs/design/` — **read the design docs before writing code**;
they encode live-verified API facts and deliberate decisions.

## Development environment — MANDATORY

**All work happens inside the Nix dev shell defined by `flake.nix`. Never use compilers,
tools, or binaries from the host system — no exceptions.**

- Interactive (direnv installed): `direnv allow` once, then everything below just works.
- Non-interactive / scripts / background tasks: prefix every command:

  ```sh
  nix develop -c <command>
  # e.g. nix develop -c cargo build
  ```

- The flake pins everything by commit hash (`flake.lock` is committed) and the Rust
  toolchain **1.99.0** is written explicitly in `flake.nix`. Do not "just use" a system
  `cargo`/`rustc`/`cc` — inside the shell they resolve to the store paths, outside it they
  are wrong. If a tool is missing, add it to the flake (pinned), not via ad-hoc installs.
- Toolchain/input bumps are deliberate commits: update the hash/version in `flake.nix`,
  re-run `nix flake lock`, verify, commit both files together.
- **macOS dev signing**: debug builds are ad-hoc signed per build, so the keychain
  re-prompts after every rebuild. `.cargo/config.toml` installs a `runner` that re-signs
  `cargo run`/`cargo test` binaries with a stable identity (Apple Development cert;
  `GOGO_CODESIGN_IDENTITY` overrides) — one "Always Allow" survives rebuilds. For direct
  `target/debug/gogo` runs, `scripts/dev-sign.sh` once. Best-effort: no identity → plain exec.

- **Launch-testing `.app` bundles / games**: never `open` them on the host (it steals the
  user's focus). Use `scripts/vm-test.sh run <X.app> [--secs N]` (Tart VM; how-to in
  `docs/vm-testing.md`) and read the summary JSON it prints.

## Canonical checks (run inside the shell)

```sh
cargo fmt --check              # formatting
cargo clippy --all-targets -- -D warnings
cargo nextest run              # tests (see docs/design/08-testing.md)
cargo deny check               # licenses + advisories
```

## Repo conventions

- **Conventional commits** (`feat:`, `fix:`, `docs:`, `chore:`, `build:`, `test:`, `ci:`),
  imperative subject, body for context.
- **PRs are opened, never merged, by agents**: `main` is rule-protected (changes must
  land through a PR with all required checks green). An agent's job ends at opening the
  PR and verifying CI — merging is always a human decision. Never run `gh pr merge`,
  enable auto-merge, or merge on the user's behalf.
- Secrets and personal data never enter git: `gogo-probe/` (probe output — OAuth tokens,
  account data) is gitignored by design; never print or commit token/code values; URLs in
  logs are redacted (see `docs/design/07-errors-and-observability.md` §4).
- Scope: **Linux + macOS only, no Windows** target — but downloading Windows installers
  (for Wine/Proton use) is a first-class use case.
- Dependencies: gog-warp is a git rev-pin on `playtron-os/gog-warp` (at or after
  the merge of #5, macOS pre-allocation), not on crates.io; reqwest stays on the 0.12
  series to match it (one TLS stack). The `noosxe/gog-warp` fork is kept in sync with
  upstream for future patches: pin it only while an upstream PR is pending, then
  switch back to playtron-os once it merges. Crate-version table: `docs/design/01-architecture.md`.

## Issue tracking

- Every issue gets one or two `type:` labels (bug, feature, docs, chore, ci, release, research)
  and one or more `area:` labels (auth, library, downloads, depot, mac-app, wine-runtime, cli,
  tui, packaging, licensing). Add `phase:v1` / `phase:v2` only for roadmap-scoped items (design 09).
- Work is grouped under `epic` issues using GitHub's native sub-issues (not task lists):
  v0.1.0 release (#49), mac-app & Wine runtime (#50), docs/research/hygiene (#51),
  depot downloads (#10), TUI (#11), v2 backlog (#12). File the issue, then link it:

  ```sh
  p=$(gh api repos/noosxe/gogo/issues/<epic> --jq .node_id)
  c=$(gh api repos/noosxe/gogo/issues/<N> --jq .node_id)
  gh api graphql -f query='mutation($p:ID!,$c:ID!){addSubIssue(input:{issueId:$p,subIssueId:$c}){subIssue{number}}}' -f p=$p -f c=$c
  ```

- `dependencies` / `rust` belong to Dependabot; leave them alone. Agents may label issues,
  but ask before closing issues or deleting labels.

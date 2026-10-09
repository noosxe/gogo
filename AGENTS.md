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
- Secrets and personal data never enter git: `gogo-probe/` (probe output — OAuth tokens,
  account data) is gitignored by design; never print or commit token/code values; URLs in
  logs are redacted (see `docs/design/07-errors-and-observability.md` §4).
- Scope: **Linux + macOS only, no Windows** target — but downloading Windows installers
  (for Wine/Proton use) is a first-class use case.
- Dependencies: gog-warp is a git rev-pin, not on crates.io; reqwest stays on the 0.12
  series to match it (one TLS stack). Crate-version table: `docs/design/01-architecture.md`.

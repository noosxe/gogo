# 07 — Rust Ecosystem Survey

Crates relevant to a GOG CLI downloader. Versions/proof-points **verified 2026-10-08** via
crates.io API. gog-warp's `Cargo.toml` is included as ground truth (it is a working GOG client
in Rust).

## 1. What gog-warp uses (working GOG client in Rust today)

```toml
async-compression = { version = "0.4", features = ["zlib", "tokio"] }
bincode = "1.3"
chrono = { version = "0.4", features = ["serde"] }
derive-getters, futures, libc, log, parking_lot
md-5 = "0.10"
reqwest = { version = "0.12", features = ["json", "stream"] }
serde, serde_json, serde_repr
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["io", "compat"] }
url = "2"
# build-deps (xdelta3 patching): bindgen, cc, rand
# dev-deps: indicatif, simple_logger
```

Implication: the **whole feature set needs nothing exotic** — this is a mainstream stack.

## 2. Candidate crates by concern (latest stable versions as of research date)

### CLI & UX
| Crate | Version | Notes |
|---|---|---|
| `clap` | 4.6.7 | derive API; subcommands, env-var fallback built in |
| `clap_complete` | 4.6.11 | shell completions |
| `indicatif` | 0.18.6 | progress bars/multi-bars; standard choice |
| `console` | 0.16.6 | terminal abstraction (underlies indicatif/dialoguer) |
| `dialoguer` | 0.12.0 | interactive prompts (e.g. "select installer variant") |
| `rpassword` | 7.5.4 | hidden password input (only needed if headless login ever added) |
| `comfy-table` | 8.0.1 | pretty `list` output |
| `owo-colors` | 4.4.0 | colors |
| `crossterm` | 0.29.0 | raw terminal control (probably not needed if using indicatif) |
| `bytesize` | 2.7.0 | human-readable sizes ("1.1 GB") |
| `ctrlc` | 3.5.2 | graceful Ctrl-C (flush resume state) |

### Async & HTTP
| Crate | Version | Notes |
|---|---|---|
| `tokio` | 1.53.2 | runtime; `features=["full"]` |
| `reqwest` | 0.13.5 | `json`, `stream` (and `rustls` feature to avoid OpenSSL); gog-warp pins 0.12 |
| `futures` | 0.3.34 | streams combinators |
| `tokio-util` | 0.7.19 | `io`/`compat` (stream→AsyncRead for compression) |
| `async-compression` | 0.4.50 | zlib decode of depot manifests/chunks |
| `governor` | 0.10.4 | token-bucket rate limiting — natural fit for a bytes/sec limiter shared across workers |
| `url` | 2.5.8 | URL templating for secure-link `url_format` |
| `httpdate` | — | if ever needed for cache headers |

### Serialization & data
| Crate | Version |
|---|---|
| `serde` / `serde_json` | 1.0.229 |
| `toml` | 1.1.7 (spec 1.1) — candidate config format |
| `md-5` | 0.11.0 — checksums (GOG uses MD5 everywhere) |
| `sha2` | 0.11.0 — future-proofing only |
| `chrono` | time math for `expires_in` |
| `bincode`/`serde_json` | resume-state files |

### Config, dirs, errors, secrets
| Crate | Version | Notes |
|---|---|---|
| `dirs` | 7.0.0 | XDG/AppData/Application Support resolution |
| `config` / `figment` | — | layered config (defaults < file < env < CLI); either works with `serde`+`toml` |
| `anyhow` | 1.0.104 | app-level error type |
| `thiserror` | 2.0.21 | library-level error enums (API errors, 401 vs 5xx vs 416 taxonomy) |
| `keyring` | 4.2.0 | OS keychain token storage (macOS Keychain / Windows CredMan / Secret Service) |
| `zeroize` | 1.9.1 | scrub tokens from memory |
| `tracing` + `tracing-subscriber` | 0.1.44 | structured logging with verbosity flags |

### Potential native dependencies to scope carefully
- **xdelta3** (Model B patching only): gog-warp compiles the C library via `bindgen`+`cc`. If
  we stay on Model A (installers), this drops out entirely.
- Opening the browser for login: no crate verified during discovery — candidates to check at
  design time: `webbrowser` / `open`. Trivial to shell out instead.
- XML parsing of installer checksum files: `quick-xml` (not yet verified — checksum XML is
  simple enough for a tiny hand-rolled parse if preferred).

## 3. Existing Rust GOG libraries (evaluate before writing our own)

| Crate | Status | Relevance |
|---|---|---|
| **`gog-warp`** (playtron-os, by imLinguin/Heroic) | **active**, modern (reqwest 0.12, tokio 1.37-era) | Full auth + library + v1/v2 content-system downloader + patching/verify. **Apache-2.0**; **not on crates.io, no git tags** → git dependency pinned by rev (doc 10 §4). Core API surface mapped in doc 10 §2. |
| `gog` (nicohman/gog-rs), v0.5.0, ~28k downloads | stale (pre-Galaxy-2, reqwest-old) | Historic endpoint coverage; token handling ideas. |
| `gog-sync` | stale | Library-sync-to-folder concept. |
| `wyvern` v1.4.1 (nicohman) | stale | Rust CLI precedent: interactive login, Linux install focus. |
| `gog-conjure` (Myxelium) | small | Another Rust GOG client; worth a skim. |

Other-language references worth mining during design: heroic-gogdl (Python, best-documented
v1/v2 flow), lgogdownloader (C++, 12+ years of edge cases), habedi/gogg (Go, installer-focused,
recent), RikudouSage/GogDownloader (PHP, installer-focused, cron-oriented).

## 4. Toolchain notes

- Single-binary cross-platform distribution (Linux/macOS/Windows) via `cargo build --release`
  + CI matrix; rustls keeps us off system OpenSSL (macOS/Windows friendliness).
- MSRV: pick latest stable at design time; nothing above suggests an old toolchain constraint.

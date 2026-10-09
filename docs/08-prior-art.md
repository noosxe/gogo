# 08 — Prior Art: Existing GOG Tools

Survey of the tools studied during discovery (all inspected at source level, 2026-10-08).
Star counts are approximate point-in-time values.

## 1. Comparison table

| Tool | Language | Focus | Auth | Content model | Active? |
|---|---|---|---|---|---|
| **LGOGDownloader** (Sude-) | C++ | Full-featured CLI downloader | Headless form login (+2FA) → OAuth code via cookies | A + B (`--galaxy-install`) | Yes (12+ yrs) |
| **heroic-gogdl** (Heroic) | Python | Download module for Heroic launcher | OAuth code (via Heroic) / CLI `auth --code` | B (v1+v2, patches, deps, cloud saves, launch) | Yes |
| **gog-warp** (playtron-os, imLinguin) | **Rust** | Library, not an app | Browser-code → token; multi-token store | B (v1+v2, deps, patches, verify) | Yes |
| **GogDownloader** (RikudouSage) | PHP | Bulk installers to disk / cron / NAS | Interactive `login` + `login-code` | A (installers, extras, saves, movies) | Yes |
| **gogg** (habedi) | Go | Simple CLI+GUI downloader | Browser-code login | A (installers, patches, extras, layouts) | Yes |
| **minigalaxy** (sharkwouter) | Python/GTK | Minimal Linux GOG client | Embedded webkit login | A (download + install Linux/Windows) | Yes |
| **wyvern** (nicohman) | Rust | CLI Linux GOG client | Interactive | A/B (Linux install focus) | Stale |
| **gog / gog-rs** (nicohman) | Rust | API library | token file | partial | Stale |
| **galaxy-integrations-python-api** (gogcom) | Python | Official plugin sample | Galaxy-style OAuth | library/state endpoints | Reference only |
| **gog_content_system_downloader** (MattMills) | Python | Bulk depot archiver incl. historical builds | Bearer | B | Small/utility |

## 2. What each does especially well (steal-worthy ideas)

- **lgogdownloader**: the most complete feature matrix — per-game config overrides, blacklists,
  directory templates, cover art download, changelog/serial extraction, MD5-verified resume,
  retry taxonomy (never retry 416), separate metadata threads, both content models under one
  CLI. Its `website.cpp` is the only maintained reference for headless form login with 2FA.
- **heroic-gogdl**: cleanest separation of download "managers" (v1 vs v2 vs linux-native),
  JSON machine output (`info`, `calculate-download-size`), workers-count knob, manifest caching
  for `repair`. Best living documentation of the v2 depot pipeline.
- **gog-warp**: proves the modern Rust stack end-to-end (reqwest streaming + tokio semaphore +
  zlib chunks + xdelta3) and shows multi-endpoint CDN failover and runtime speed caps.
- **GogDownloader (PHP)**: best bulk/archival UX — local SQLite metadata with migrations,
  `--new-only/--updated-only`, cron-friendly, S3 output backend, naming-scheme migration
  command, idle-timeout for NAS.
- **gogg**: best minimal scope discipline — list/search/export, filtered downloads, checksum
  verification, resume, total-size planning, Docker image, optional GUI on the same core.
- **minigalaxy**: simplest viable client; shows how little is truly required (token + list +
  download + install), and the webview login alternative.

## 3. Failure modes / warnings visible in the prior art

- Headless login breaks whenever GOG tweaks login HTML or challenges with CAPTCHA
  (lgogdownloader carries years of fixes for exactly this; community docs warn about CAPTCHA).
- gogdl wraps `secure_link` failures in blind recursive retries with `time.sleep(0.2)` — works,
  but a bounded backoff (like gog-warp's) is the better pattern; infinite recursion on a
  persistent error is a known anti-pattern to avoid.
- Token expiry handling differs (re-login vs transparent refresh); tools that only stored
  access tokens forced users to re-login every ~72h — the recurring complaint in their issue
  trackers. Store + refresh the `refresh_token`.
- Old tools that parsed `manualUrl` with session cookies break when GOG rotates cookie names;
  tools using the OAuth downlink JSON survive unchanged. Prefer token-based flows everywhere.

## 4. Gap analysis vs our target (Rust CLI, offline installers, config, rate limiting)

| Capability | Exists in Rust today? |
|---|---|
| Sign-in flow | Yes (gog-warp) — app-level polish still needed |
| List owned games | Yes (gog-warp) |
| **Offline installer downloads (Model A) with checksums/resume** | **No maintained Rust implementation** (lgogdownloader C++ / gogg Go / PHP only) → this is the gap our app fills |
| Depot downloads (Model B) | Yes (gog-warp) |
| Rate limiting, config file, cross-platform polish | No established Rust GOG app does all of these |

Conclusion recorded for the design phase: Model A in Rust is genuinely unoccupied; Model B
could be v2 scope via reuse of `gog-warp` or omission.

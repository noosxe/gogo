# 06 — Configuration Research

What existing GOG tools expose as configuration, and where they put it. Findings only —
format/UX decisions belong to the design phase.

## 1. Config file locations (observed)

| Tool | Location | Format |
|---|---|---|
| heroic-gogdl | `$XDG_CONFIG_HOME/heroic_gogdl` / `%APPDATA%\heroic_gogdl` / `~/Library/Application Support/heroic_gogdl`; overridable via `GOGDL_CONFIG_PATH` env var | JSON (credentials + manifests dir) |
| lgogdownloader | `~/.config/lgogdownloader/` (config, cookies, blacklist, cache, game-specific configs) | INI-ish key=value |
| minigalaxy | per-XDG config, stores `token`, `refresh_token`, `download_dir`, `lang`, `keep_installers`, `install_dir`, `username`, `timezone` (config.py) | plain text file |
| RikudouSage/GogDownloader | SQLite DB + config command (`config:set/config:show`) | DB rows |
| gogg | CLI flags + persisted config file (`gogg config` sets download dir etc.) | JSON |

Cross-platform dir conventions all resolve to:

- Linux: `$XDG_CONFIG_HOME` (default `~/.config`) / `~/.local/share` for data
- macOS: `~/Library/Application Support/<app>` (config and data)
- Windows: `%APPDATA%` (roaming)

Rust `dirs` crate provides exactly these. An env override (like `GOGDL_CONFIG_PATH`) is a
recurring convenience for power users/headless servers.

## 2. Settings other tools expose (candidate space for ours)

### Download behavior
- Download **directory** (every tool) + optional **subdirectory templates** (lgogdownloader:
  `{GAME}/{PLATFORM}/...` patterns; gogg:RomM/Lutris-compatible layouts)
- **Parallel workers/threads** (gogdl `--workers-count`, lgogdownloader `--threads`,
  `--info-threads`)
- **Chunk size** and retry counts (lgogdownloader)
- **Rate limit / max speed** (gog-warp `set_max_speed`; lgogdownloader historically had
  `--speed` limit support)
- **Idle timeout** (GogDownloader `--idle-timeout` for NAS)
- Resume on/off, skip-if-exists ("only missing or newer", "keep installers after install" in
  minigalaxy)

### Content selection (per-run flags in all tools; some persistable)
- **Platform filter**: windows / mac / linux (gogdl `--platform`, GogDownloader `--os`)
- **Language filter** (multi-select; gogdl has a generated languages map `languages.py`)
- **Content types**: installers / extras / patches / language-packs / DLCs / movies / covers
  (lgogdownloader `--include/--exclude` bit flags; gogg filters; GogDownloader `--dlc` etc.)
- **Build/branch selection**: `--branch`, `--password` (heroic-gogdl), build id pinning
- **Version pinning / skip updated** (`--updated-only`, `--new-only`, `--skip-known-versions`)

### Auth/session
- Token persistence (remember me on/off), `login`/`logout`/`status` commands
- Multiple stored credentials keyed by client_id (gogdl's credentials JSON design)

### Metadata/cache
- Cache dir for product JSON/manifests (lgogdownloader, gogdl)
- Local database of library + file info (GogDownloader) with `update-database` and filters

### Display
- Progress bars, verbosity/log level, `--json` output (gogdl prints JSON for machine
  consumption in `info`/`calculate_download_size`)

## 3. Precedence pattern (de-facto standard across these tools)

1. CLI flags (highest)
2. Environment variables (e.g. `GOGDL_CONFIG_PATH`)
3. Config file
4. Built-in defaults

Rust ecosystem supports this pattern with `clap` + `config`/`figment` + `serde` (doc 07).

## 4. Rate-limiting config notes specifically

- The only app-level speed knob observed in a Rust codebase is gog-warp's `set_max_speed`
  (i64, mid-download adjustable — implies a shared limiter object, not per-request config).
- Python tools historically implemented limits via chunk+sleep; C++ via curl `CURLOPT_MAX_SEND/RECV_SPEED`.
- A config surface like "max concurrent requests", "max concurrent chunks", "bytes/sec cap"
  covers all behaviors observed across tools; whether to expose all three is a design decision.

# 23 — The Galaxy SDK and Comet (#356)

**Research date:** 2026-11-21. **Method:** source reading through the GitHub API of
[imLinguin/comet](https://github.com/imLinguin/comet) (main at v0.4.1) and
[Heroic Games Launcher](https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher)
(main, 2026-11), Comet's README and wiki, and #356's host-pass observations. Nothing was
downloaded or run, no game was launched and no account was used. Behaviour is described in
our own words; no code was taken. PCGamingWiki refused the fetch, so its claim is quoted
from #356. The design that follows from this doc is [design 23](design/23-galaxy-service.md).

## 0. Why

GOG's Windows builds of many games ship the **GOG Galaxy SDK** (`Galaxy.dll` /
`Galaxy64.dll`; Cyberpunk 2077 also `REDGalaxy64.dll`). In a Galaxy install, the SDK talks to
Galaxy's local **Communication Service** for sign-in, achievements, stats, leaderboards and
multiplayer. A `gogo linux-app` build runs the game with nothing answering it. Two Cyberpunk
2077 (1423049311) symptoms on a host pass point there (#356):

- a ~30 s wait at the start screen's second key press (and when leaving the benchmark);
- the game's own CD PROJEKT RED cross-progression saves don't show: Heroic's install on the
  same account lists a second playthrough the gogo build doesn't, and neither prefix nor
  GOG's cloud-save container (design 22) holds it.

Heroic runs **Comet**, an open-source stand-in for that service, next to the game.

## 1. Comet upstream

| | |
|---|---|
| Repo | <https://github.com/imLinguin/comet>, one maintainer (imLinguin); continues Yepoleb's earlier Python Comet |
| Licence | **Apache-2.0** (`LICENSE`; `Cargo.toml` carries no `license` field). gogo (MIT OR Apache-2.0) can fetch it, or ship it, with the licence text and a notice line |
| Language | Rust, edition 2024; `reqwest` 0.13, tokio, sqlx |
| Releases | v0.1.1 (2024-07) … v0.3.2 (2026-01-04), v0.4.0 (2026-10-26), **v0.4.1 (2026-10-28)**; a few a year |
| Assets | bare binaries per target (`comet-x86_64-unknown-linux-gnu` 14.6 MB for v0.4.1, aarch64, macOS, Windows), `dummy-service.zip`, `steam-deck.zip`; GitHub's `sha256:` digest from v0.3.1 on, no checksum files |
| Linking | the Linux builds are glibc, dynamically linked: on stock NixOS they don't start (no `/lib64` loader), umu's R1 again (design 18 §2.1) |
| Maturity | "work-in-progress" (wiki); Heroic ships it on by default as an experimental feature since 2.15.0 but pins **v0.2.0**; open issues include #80 (Rise of the Tomb Raider crashes with Comet), #16 (no full offline mode), #73, #52 |
| Stance | the README says Comet is meant for launchers (Heroic, Lutris, "open for Pull Requests" for Minigalaxy) and can't replace the official service. No CONTRIBUTING file, no AI policy |

## 2. How it runs

### 2.1 Comet's side

- **CLI:** `--access-token`, `--refresh-token`, `--user-id`, `--username` (required); or one
  of `--from-heroic` / `--from-lutris` / `--from-wyvern`, which read those three values from
  that launcher's config file; `-q/--quit`. Subcommands `preload <client_id>
  <client_secret>` (caches a game's achievements and stats for offline use) and `overlay`.
  Environment: `COMET_LOG` (log filter, default `info`) and `COMET_IDLE_WAIT` (default 15 s).
- **Credentials** are read once, at start, **before** the port is bound. `--from-heroic`
  reads `heroic/gog_store/auth.json` under `$XDG_CONFIG_PATH` (sic: not the standard
  `XDG_CONFIG_HOME`), default `~/.config`, then Heroic's Flatpak dir, and takes the entry for
  the Galaxy client id. A missing or bad file is a panic.
- **Tokens at run time.** A game identifies itself with its client id and secret; Comet
  exchanges the Galaxy refresh token for a per-game token with `without_new_session=1`,
  the exchange gogo's cloud saves use (design 22 §1.1), so the user's stored session isn't
  rotated. Tokens live in memory only; nothing is written back. The Galaxy access token
  itself is never refreshed by Comet (it serves the notifications websocket and user data),
  so a session longer than its hour is a point to check.
- **Transport:** one TCP listener on the fixed **`127.0.0.1:9977`**, Galaxy's protobuf
  protocol. One Comet per host: a second one panics on bind. One Comet serves several games
  at once (tokens per client id, connections counted).
- **`--quit`:** exits once a client has connected and none is left after `COMET_IDLE_WAIT`
  seconds. If no client ever connects it never quits. Only SIGINT is handled gracefully.
- **Its files:** on start it fetches the Windows **GalaxyPeer** redist (about 100 MiB, per its
  Steam Deck guide) into `$XDG_DATA_HOME/comet/redist`, retrying in the background; its
  achievements/stats DB is `$XDG_DATA_HOME/comet/gameplay`, its config
  `$XDG_CONFIG_HOME/comet/config.toml`.
- **Secrets in its log.** At the default `info` level it logs every game's client id **and
  client secret** when the game identifies itself. At `warn` it logs a failed exchange with
  the HTTP error's debug form, whose URL carries the refresh token and the client secret as
  query parameters. Heroic keeps all of it in a log file. (Draft upstream report: #494.)

### 2.2 Heroic's side

- **Start** (`src/backend/storeManagers/gog/games.ts`): just before running the game, when a
  GOG user with a username exists and the global experimental setting `cometSupport` isn't
  off (default on; per-game is Heroic's open #3942): `comet --from-heroic --username <name>
  --quit`, output into a "comet" log. No token in argv or the environment; Comet reads
  Heroic's own auth file, which Heroic maintains.
- **Stop:** the child is killed when the game's runner command returns.
- **Where:** a host process beside Wine; nothing runs inside the prefix or the container.
- **Dummy service** (`src/backend/launcher.ts`): at prefix setup Heroic copies Comet's dummy
  `GalaxyCommunication` exe into `C:\ProgramData\GOG.com\Galaxy\redists\`, registers it with
  `sc create`, and writes the Galaxy client path into the registry; errors don't stop it.
  Some SDK versions wake that service before connecting (Comet's `dummy-service/README.md`).
- **Pin:** `meta/downloadHelperBinaries.ts` pins Comet `v0.2.0` and fetches the per-OS
  binary and the dummy exe from Comet's releases.

### 2.3 The container

pressure-vessel (umu's container) shares the host's network namespace, so a game inside it
reaches a Comet on the host's `127.0.0.1:9977`; Heroic's umu launches rely on the same. In
`scripts/linux-e2e.sh` the game's sandbox has its own network namespace with only `lo`, and
`launch.sh` runs inside it, so a Comet started by `launch.sh` is on the same `lo`. Not yet
live-checked for gogo (#490).

## 3. What Comet serves

From the README's request list:

- **Game:** library info, **auth info (the SDK's sign-in)**, user stats get/update/delete,
  achievements get/unlock/clear/delete, leaderboards (all but global stats), time played,
  topic subscriptions.
- **Overlay:** game session start, overlay init, achievement notifications, web page,
  invitation dialog. There is no Linux overlay; the Windows one is an explicit download.
- **Not served:** global stats, join/invite notifications; no friends or presence.
- **Cloud saves: none.** In Heroic the launcher's backend (gogdl) syncs GOG cloud saves before
  and after a run; Comet has no cloud-storage code.

### 3.1 Cross-progression (Cyberpunk 2077)

Cyberpunk's cross-platform saves (Settings → Gameplay) sign in to a CD PROJEKT RED or GOG
account and live on CD PROJEKT RED's own service, which keeps the latest save per type; they
are not GOG cloud saves (#356 checked GOG's container). PCGamingWiki, as quoted in #356, says
the game lists them only when started through GOG Galaxy or REDlauncher, i.e. when signed in.

The likely chain: the game asks the Galaxy SDK for a signed-in user; with nothing on 9977
the SDK never signs in; the game can't reach the cross-progression service. Comet answers
the SDK's sign-in, and Comet's wiki lists Cyberpunk 2.21 with achievements working through
Comet v0.2.0 in Heroic, so the sign-in works for this game; and Heroic, running Comet, lists
the save. Not verified: whether the flow needs only the SDK sign-in or also the overlay's
web page path. The cheap check is on Heroic with `cometSupport` off (design 23, H3).

### 3.2 Cloud saves and gogo

No overlap: GOG cloud saves stay gogo's (design 22, #464). The only rule is order: the pull
before the game (and Comet) starts, the push after the prefix is idle. A cross-progression
save the game downloads lands in the prefix's save dir and is from then on an ordinary local
save, pushed to GOG like any other.

## 4. The ~30 s wait

- With nothing listening, the SDK's connection to 127.0.0.1:9977 is refused at once, and
  Comet's wiki marks Cyberpunk "service not required". A local wait for a missing service
  alone doesn't obviously take 30 s.
- A better fit: the second press loads the save list, cross-progression included; the game
  asks for a signed-in user or calls CD PROJEKT RED's service, and waits for an online
  timeout. One cause would then explain both symptoms.
- In the e2e harness the game sandbox has **no network**, and docs/linux-e2e.md's Cyberpunk
  table shows `space` to the remembered disk's menu in about 4.5 s. Those runs pass
  `-skipStartScreen`, so they may not walk the same screen, but they suggest the wait
  doesn't happen offline.
- So Comet plausibly fixes it, by giving the SDK a signed-in user at once. Unproven: #486
  times the second press offline in e2e and has host-pass steps (sockets during the wait,
  the network off, Heroic without Comet).

## 5. Games that benefit

Any Windows build that ships the Galaxy SDK and uses achievements, stats, leaderboards,
Galaxy multiplayer or a sign-in for the publisher's service. Comet's wiki compatibility
table lists about 35 working games, among them Cyberpunk 2077, DOOM (2016), DOOM + DOOM II,
DOOM 3 BFG, Ghostrunner, Indiana Jones and the Great Circle, Kingdom Come: Deliverance I and
II, Wolfenstein: The New Order and The Old Blood, Quake II, Heretic + Hexen (crossplay),
Horizon Zero Dawn, Cuphead, Homeworld: Deserts of Kharak, the Metal Slug games, Stardew
Valley (multiplayer only), BATTLETECH and Tomb Raider GOTY. Cuphead, DOOM 3 BFG, KCD, Indiana
Jones and Homeworld need the dummy service (#492). Known bad: Rise of the Tomb Raider
(Comet #80) and STONKS-9800. Ghostrunner keeps saves per Galaxy user id, so turning Comet on
can hide existing saves: one reason it is opt-in. gogo's PE scan already sees the Galaxy DLL
(it ignores it for graphics), so which built games ship the SDK can come from the scan
(#489).

## 6. Draft upstream report

For @noosxe to file on imLinguin/comet (#494):

> **Secrets in the default log, and a way to pass credentials without argv**
>
> 1. At the default `info` level, every game's client secret is logged when it identifies
>    itself, and at `warn` a failed token exchange is logged with the HTTP error's debug
>    form, whose URL carries the refresh token and the client secret. Launchers that keep
>    Comet's log end up with both on disk and in pasted bug reports. Suggest dropping the
>    secret from the info line and logging request errors without their URL.
> 2. Credentials can only be passed as arguments, which every local user can read in
>    `/proc/<pid>/cmdline`, or through another launcher's config file. Suggest reading them
>    from stdin or an inherited file descriptor (the same three fields as the Heroic
>    import), or an explicit credentials-file option. Side note: the Heroic import reads
>    `XDG_CONFIG_PATH`, not `XDG_CONFIG_HOME`.

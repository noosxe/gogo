# 04 — Download Paths & Content System Research

There are **two distinct content models** on GOG. Understanding the split is the single most
important discovery outcome for scoping the app.

- **Model A — Classic offline installers** (`setup.exe`, mac pkg/dmg, Linux mojosetup `.sh`,
  plus extras): what "download my offline installers" means. Used by lgogdownloader, gogg,
  RikudouSage/GogDownloader.
- **Model B — Galaxy depot content** (raw game files as chunked depots): what GOG Galaxy and
  Heroic download to *install* games without installers. Used by heroic-gogdl, gog-warp,
  lgogdownloader's `--galaxy-install`.

## 1. Model A — offline installers

### 1.1 Resolving a download URL

### 1.1 Resolving a download URL (live-verified 2026-10-08)

The modern products API carries download URLs in a `files[]` array per installer entry:

```text
installer entry = {
  id: "installer_windows_en", name, os: "windows", language: "en", language_full: "English",
  version: "2.2.2", total_size: 723517440,
  files: [ { id: "en1installer0", size: 723517440,
             downlink: "https://api.gog.com/products/{pid}/downlink/installer/en1installer0" } ]
}
```

(`downloads` also has `patches[]`, `bonus_content[]`, `language_packs[]` with the identical
shape — patches and extras ride the same flow. Multi-part installers = multiple `files[]`
entries: `en1installer0`, `en1installer1`, …)

Live-verified resolution chain:

```text
1. GET https://api.gog.com/products/{pid}/downlink/installer/{file_id}    (Bearer; 200 JSON)
   → { "downlink": "<pre-tokenized CDN url>", "checksum": "<xml url>" }
2. CDN url (example shape):
   https://gog-cdn-fastly.gog.com/token=…~dirs=6~token=…/secure/offline/{pid}/{pid}/{build}/{n}/setup_hot_brass_2.2.2_(85647).exe
   → Range-capable; NO Authorization header needed (URL is pre-tokenized)
3. GET {checksum} (no auth) → authoritative layout XML (see §1.2)
```

The legacy website flavor still exists in the *account* view (`manualUrl` = `/downlink/{slug}/…`
on www.gog.com, redirect-based). The lgogdownloader-era `galaxy_downlink_json_url` is gone from
the products response; the `files[].downlink` endpoint above is its replacement and returns the
same `{downlink, checksum}` JSON that lgogdownloader/minigalaxy/gogg consume. Bearer-only, no
cookies — confirmed live (doc 11).

### 1.2 Observed properties

- Multi-part installers = multiple entries in `files[]`; all parts required.
- Checksum XML is authoritative: root `<file name available notavailablemsg md5 chunks timestamp
  total_size>` + **10 MiB chunks** (`<chunk id from to method="md5">hex</chunk>`, last partial)
  → parallel chunked download with per-chunk verification is possible.
- Save filename = XML `name` attr: `setup_<slug>_<version>_(<buildid>).exe`.
- Caution: API `total_size`/`files[].size` can disagree with XML `total_size` (live: 723,517,440
  vs 724,243,232 — exactly the final chunk's span). Trust the XML; verify against real bytes.
- Languages/platforms multiply entries; filter (platform, language, skip extras/patches/DLCs,
  subdirectory templates).
- Some bonus content may 403 (bundled into the main installer) — skip, don't fail (gogg).
- Extras without a checksum: verify by size only.

## 2. Model B — Galaxy depots (content system)

### 2.1 Endpoint chain (verified in gog-warp, heroic-gogdl, lgogdownloader)

```
1. Builds:
   GET https://content-system.gog.com/products/{pid}/os/{os}/builds?generation=2[&password=...]
   Bearer auth. → { "total_count": n, "items": [ { "build_id", "generation", "version_name",
   "branch"?, "date_published", "public", "link"?, "platform_id" } ] }

2. Repository manifest (zlib-compressed JSON):
   GET {build["link"]}                             # v1: points at cdn.gog.com/content-system/v1/manifests/...
   GET {build["repository_manifest"]}              # v2: points at cdn.gog.com/content-system/v2/meta/{hash}
   → describes depots: [{ "dependencyId"?, "manifest", "size", "compressedSize",
      "executable": {"path" (starts with "__redist" for redist deps)} }]

3. Depot manifests (v2):
   GET https://cdn.gog.com/content-system/v2/meta/{hash}
   GET https://cdn.gog.com/content-system/v2/dependencies/meta/{hash}
   → { "depot": { "smallFilesContainer": { "chunks": [...] }, "items": [ { "chunks": [...],
        "path", "md5", "size", ... } ], "bigFiles": ... } }
   Each chunk: { "md5" (chunk id), "size", "compressedSize", "offset" }.

4. Secure link (Bearer):
   GET https://content-system.gog.com/products/{pid}/secure_link?_version=2&generation=2&path=/[&root=...]
   (v1 style: replace generation=2 with type=depot)
   → { "product_id", "urls": [ { "endpoint_name", "url", "url_format", "parameters",
        "priority", "max_fails", "supports_generation", "fallback_only" } ] }

5. Chunk downloads (no further auth):
   URL = assemble(endpoint.url_format, endpoint.parameters, galaxy_path(chunk.md5))
   Range: bytes={offset}-{offset+size-1}
   Content is zlib-compressed (v2); chunk files land under their md5 name.
```

### 2.2 URL assembly example (from gog-warp tests)

```
url_format: "{base_url}/token=nva={expires_at}~dirs={dirs}~token={token}{path}"
parameters: base_url=https://gog-cdn-fastly.gog.com, path=/content-system/v2/store/2034949552,
            dirs=4, token=0f76ef..., expires_at=1717011195
galaxy_path = md5[0:2] + "/" + md5[2:4] + "/" + md5
→ https://gog-cdn-fastly.gog.com/token=nva=1717011195~dirs=4~token=0f76ef.../content-system/v2/store/2034949552/f1/d4/f1d41c76...
```

Multiple endpoints (Fastly, Lumen, ...) come back ranked by `priority`; tools iterate with
fail counters (`max_fails`, `fallback_only`) — a built-in CDN failover protocol.

### 2.3 v1 vs v2 depots

| | v1 (Galaxy 1.0 era) | v2 (Galaxy 2.0 era) |
|---|---|---|
| Layout | one big `main.bin` per depot | chunk files by md5 |
| Compression | none | zlib per chunk |
| Manifest | `cdn.gog.com/content-system/v1/manifests/{pid}/{os}/{build}/{manifest}.json` | `cdn.gog.com/content-system/v2/meta/{hash}` (zlib JSON) |
| Updates | full re-download | **xdelta3 binary patches** between builds (fallback: chunk diff) |
| Shared deps | `content-system.gog.com/redists/repository` | `content-system.gog.com/dependencies/repository?generation=2` + `/open_link?...path=/dependencies/store/` (unauthenticated) |

Heroic picks the manager by `build["generation"]`; gogdl warns if a build's generation differs
from the requested one. Linux games go through a separate "Linux native installer" manager
(they download the mojosetup `.sh` and unpack it — same file as the Model A Linux installer).

### 2.4 Semantics worth noting

- **Download size vs disk size**: sum of `compressedSize` vs `size` over selected depots; redist
  depots (`__redist`) are usually excluded from game size; tools offer "download only deps this
  game needs".
- **Language selection**: depots are language-tagged; manifests list available languages.
- **Patching**: gogdl fetches *both* old and new manifests and computes a diff (`ManifestDiff`),
  requesting a server-provided patch first, else per-chunk updates; gog-warp implements the
  same (xdelta3 via C bindings, `bindgen`+`cc`).
- **Resume state**: gog-warp persists per-file chunk-bitmaps (state files next to downloads);
  gogdl caches manifests in `MANIFESTS_DIR` for verify/repair.
- **Verification**: chunk-level md5 on download; whole-file md5 for reassembly checks; final
  tree verification against manifest ("repair" command).

## 3. Which model for "offline installers"?

The user's requirement (archival offline installers) maps to **Model A**. Model B matters if we
later want Galaxy-style installs/updates (Heroic-style) or historical builds
(`builds` endpoint serves old versions — MattMills' tool bulk-archives depots). lgogdownloader
supports both, and its UX shows they can coexist behind one CLI
(`download` vs `galaxy-install`).

### 3.1 Is there an *official* way to get offline installers? (verified 2026-10-08)

Two-level answer:

- **Product level: yes.** Offline "backup installers" are a first-class, officially supported
  GOG feature and the core of their DRM-free promise. Official support article:
  <https://support.gog.com/hc/en-us/articles/360003934757> ("How to download the backup
  installers or bonus content for my GOG games") — users pick a game in the website library and
  download the installer parts (and extras) directly in the browser. GOG's 2026 independence
  announcement explicitly reaffirmed that offline installers remain available.
- **Program level: no.** There is no documented, supported API. GOG's own desktop downloader
  app ("GOG Downloader") was retired and its legacy download API decommissioned — the
  `gogdownloader://` URIs still present in gameDetails JSON are leftovers. GOG Galaxy does not
  download installers either (it installs via depots, Model B).

What remains for a CLI is exactly what §1.1 documents: **automating the website's own download
endpoints** (`manualUrl` / `galaxy_downlink_json_url` → CDN + checksum XML) with the user's own
OAuth credentials. That is the de-facto standard, used openly by lgogdownloader (12+ years,
discussed on GOG's own forums, especially valued on Linux where GOG ships no Galaxy client —
installers are GOG's *only* official Linux delivery channel).

Nuances confirmed during this check:

- Offline installers are **latest-version only**; older versions were removed from the installer
  flow (historical builds exist only via the content-system `builds` endpoint, Model B).
- Installers can be multi-part; all parts are required.

## 4. Primary source files

- gog-warp: `src/content_system/{secure_link.rs, types/v1.rs, types/v2.rs, downloader/*}`, `examples/builds.rs`
- heroic-gogdl: `gogdl/dl/dl_utils.py` (secure links, range headers, zlib decode), `dl/managers/{v1,v2,manager}.py`, `dl/objects/v2.py`
- lgogdownloader: `src/galaxyapi.cpp` (builds/manifests/secure link URLs), `src/downloader.cpp` (downlink JSON, resume, md5 XML), `src/gamefile.cpp`

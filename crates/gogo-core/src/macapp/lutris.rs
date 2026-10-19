//! Live Lutris client (design 11 §2): search games, enumerate a game's
//! community installer scripts, fetch script JSON. A **transient** client by
//! contract (design 11 §4): nothing is persisted — every conversion searches
//! live, the user picks, the per-run working set is discarded afterwards.
//!
//! Endpoint shapes are live-verified (docs/14 §2.1.1, 2026-10-19):
//! - `GET /api/games?search=<name>&with-installers=True` — filtered results;
//!   records carry `provider_games[]` with `service: "gog"` and the GOG
//!   product id as `slug`
//! - `GET /api/installers/<installer-slug>` — full record incl. `script` as a
//!   JSON object, `gogid`, `gogslug`, `runner`, `published`, `updated_at`
//! - game pages embed `Download script` anchors:
//!   `/api/installers/<installer-slug>?format=json`
//! - there is **no** server-side game→installers filter (all list params are
//!   ignored); the game page is the enumeration surface.

use serde::Deserialize;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

use super::error::MacAppError;

pub const DEFAULT_BASE: &str = "https://lutris.net";
const UA: &str = concat!("gogo/", env!("CARGO_PKG_VERSION"), " (recipe consumer)");

/// A Lutris game record (subset) with the GOG ids it is known by.
#[derive(Debug, Clone, PartialEq)]
pub struct LutrisGame {
    pub lutris_id: u64,
    pub slug: String,
    pub name: String,
    /// GOG product ids this Lutris game is linked to (`provider_games`).
    pub gog_ids: Vec<String>,
}

/// A community installer script (the `/api/installers/<slug>` record).
#[derive(Debug, Clone)]
pub struct LutrisInstaller {
    pub slug: String,
    pub game_slug: String,
    pub name: String,
    pub version: String,
    /// "wine" | "linux" | … — only `wine` scripts are applicable to `mac-app`.
    pub runner: String,
    pub author: String,
    pub updated_at: String,
    /// Cross-check against our GOG id (§2 selection rules).
    pub gogid: Option<String>,
    /// The script document itself (JSON object).
    pub script: Value,
}

#[derive(Deserialize)]
struct GamesPage {
    #[serde(default)]
    results: Vec<ApiGame>,
}

#[derive(Deserialize)]
struct ApiGame {
    id: u64,
    slug: String,
    name: String,
    #[serde(default)]
    provider_games: Vec<ProviderGame>,
}

#[derive(Deserialize)]
struct ProviderGame {
    #[serde(default)]
    slug: String,
    service: String,
}

pub struct LutrisClient {
    base: String,
    http: reqwest::Client,
}

impl LutrisClient {
    pub fn new(base: &str) -> Result<Self, MacAppError> {
        let http = reqwest::Client::builder()
            .user_agent(UA)
            .build()
            .map_err(|e| MacAppError::Recipe(format!("http client: {e}")))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// GET with 07 §retry semantics: 3 attempts with backoff on 5xx/network
    /// errors; 4xx passes through immediately (caller decides semantics).
    async fn get(&self, path: &str) -> Result<reqwest::Response, MacAppError> {
        let url = self.url(path);
        let mut last = String::new();
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * (1 << (attempt - 1)))).await;
            }
            match self.http.get(&url).send().await {
                Ok(resp) if resp.status().is_server_error() => {
                    last = format!("{} on {}", resp.status(), path);
                }
                Ok(resp) => return Ok(resp),
                Err(e) => last = format!("{e} on {path}"),
            }
        }
        Err(MacAppError::Recipe(format!("lutris unreachable: {last}")))
    }

    /// Search games with installers (`search_games` in the Lutris client).
    pub async fn search(&self, name: &str) -> Result<Vec<LutrisGame>, MacAppError> {
        let query: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("search", name)
            .append_pair("with-installers", "True")
            .finish();
        let page: GamesPage = self
            .get(&format!("/api/games?{query}"))
            .await?
            .json()
            .await
            .map_err(|e| MacAppError::Recipe(format!("search parse: {e}")))?;
        Ok(page
            .results
            .into_iter()
            .map(|g| LutrisGame {
                gog_ids: g
                    .provider_games
                    .iter()
                    .filter(|p| p.service == "gog")
                    .map(|p| p.slug.clone())
                    .collect(),
                lutris_id: g.id,
                slug: g.slug,
                name: g.name,
            })
            .collect())
    }

    /// The community installers of a game: enumerate via the game page's
    /// `Download script` anchors, then fetch each record.
    pub async fn installers_for(
        &self,
        game_slug: &str,
    ) -> Result<Vec<LutrisInstaller>, MacAppError> {
        let html = self
            .get(&format!("/games/{}", game_slug))
            .await?
            .text()
            .await
            .map_err(|e| MacAppError::Recipe(format!("game page: {e}")))?;
        let mut out = Vec::new();
        for slug in parse_script_slugs_from_page(&html) {
            if let Ok(inst) = self.installer(&slug).await {
                out.push(inst);
            }
        }
        Ok(out)
    }

    /// Fetch one installer record by slug (also accepts game slugs — the API
    /// returns their installer list, `count` may be 0).
    pub async fn installer(&self, slug: &str) -> Result<LutrisInstaller, MacAppError> {
        let mut v: Value = self
            .get(&format!("/api/installers/{}", slug))
            .await?
            .json()
            .await
            .map_err(|e| MacAppError::Recipe(format!("installer parse: {e}")))?;
        let rec = v
            .get_mut("results")
            .and_then(|r| r.as_array_mut())
            .and_then(|a| if a.len() == 1 { a.pop() } else { None })
            .ok_or_else(|| MacAppError::Recipe(format!("no installer record for '{slug}'")))?;
        let s = |k: &str| {
            rec.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        if rec.get("script").map(Value::is_null).unwrap_or(true) {
            return Err(MacAppError::Recipe(format!(
                "installer '{slug}' has no script"
            )));
        }
        Ok(LutrisInstaller {
            slug: s("slug"),
            game_slug: s("game_slug"),
            name: s("name"),
            version: s("version"),
            runner: s("runner"),
            author: s("user"),
            updated_at: s("updated_at"),
            gogid: rec
                .get("gogid")
                .and_then(Value::as_u64)
                .map(|id| id.to_string()),
            script: rec.get("script").cloned().unwrap_or(Value::Null),
        })
    }
}

impl LutrisClient {
    /// Stream a `files:` URL into `dst` (per-run working set; design 11 §4).
    pub async fn download(&self, url: &str, dst: &Path) -> Result<u64, MacAppError> {
        use tokio::io::AsyncWriteExt as _;
        let url = if url.starts_with("http") {
            url.to_string()
        } else {
            self.url(url)
        };
        let mut last = String::new();
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * (1 << (attempt - 1)))).await;
            }
            match self.http.get(&url).send().await {
                Ok(resp) if resp.status().is_server_error() => {
                    last = format!("{} on {url}", resp.status());
                }
                Ok(resp) if !resp.status().is_success() => {
                    return Err(MacAppError::Recipe(format!(
                        "download {url}: {}",
                        resp.status()
                    )));
                }
                Ok(resp) => {
                    if let Some(parent) = dst.parent() {
                        tokio::fs::create_dir_all(parent)
                            .await
                            .map_err(|e| MacAppError::Recipe(format!("mkdir: {e}")))?;
                    }
                    let mut file = tokio::fs::File::create(dst)
                        .await
                        .map_err(|e| MacAppError::Recipe(format!("create: {e}")))?;
                    let mut total = 0u64;
                    let mut stream = resp;
                    while let Some(chunk) = stream
                        .chunk()
                        .await
                        .map_err(|e| MacAppError::Recipe(format!("download: {e}")))?
                    {
                        file.write_all(&chunk)
                            .await
                            .map_err(|e| MacAppError::Recipe(format!("write: {e}")))?;
                        total += chunk.len() as u64;
                    }
                    file.flush()
                        .await
                        .map_err(|e| MacAppError::Recipe(format!("flush: {e}")))?;
                    return Ok(total);
                }
                Err(e) => last = format!("{e} on {url}"),
            }
        }
        Err(MacAppError::Recipe(format!("lutris unreachable: {last}")))
    }
}

/// Rank candidates (design 11 §2): scripts that drive a GOG installer first
/// (they encode the `/NOGUI`-class flags), then newest `updated_at`.
pub fn rank(candidates: &mut [LutrisInstaller]) {
    let gog_file = |i: &LutrisInstaller| {
        parse_files_value(&i.script)
            .iter()
            .any(|(_, url)| url.to_lowercase().contains("gog"))
    };
    candidates.sort_by(|a, b| {
        gog_file(b)
            .cmp(&gog_file(a))
            .then(b.updated_at.cmp(&a.updated_at))
    });
}

fn parse_files_value(script: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(list) = script.get("files").and_then(Value::as_array) {
        for entry in list {
            if let Some(o) = entry.as_object() {
                for (k, v) in o {
                    out.push((k.clone(), v.as_str().unwrap_or_default().to_string()));
                }
            }
        }
    }
    out
}

/// Match a Lutris game by any of our GOG ids (`provider_games[].slug`).
pub fn match_gog_game<'a>(games: &'a [LutrisGame], gog_ids: &[String]) -> Option<&'a LutrisGame> {
    games
        .iter()
        .find(|g| g.gog_ids.iter().any(|id| gog_ids.contains(id)))
}

/// The recipe's `wine.version` pin, if any (design 11 §2 `wine:` block) —
/// advisory on macOS (design 12 D6).
pub fn wine_version(script: &Value) -> Option<String> {
    script
        .get("wine")?
        .get("version")?
        .as_str()
        .map(str::to_string)
}

/// Best game for a pre-install search (design 12 §6 step 2): exact
/// case-insensitive name, then slug equality, then the first hit. The
/// authoritative gogid cross-check runs post-install and downgrades a wrong
/// pick to `RecipeMiss`, so a best-effort choice here is safe.
pub fn pick_game<'a>(games: &'a [LutrisGame], name: &str) -> Option<&'a LutrisGame> {
    let lower = name.to_lowercase();
    games
        .iter()
        .find(|g| g.name.to_lowercase() == lower)
        .or_else(|| {
            let slug = lower.replace([' ', '_'], "-");
            games.iter().find(|g| g.slug == slug)
        })
        .or_else(|| games.first())
}

/// Extract installer slugs from a game page: `Download script` anchors of the
/// form `/api/installers/<slug>?format=json`. Deliberately narrow — anything
/// else on the page is prose.
pub fn parse_script_slugs_from_page(html: &str) -> Vec<String> {
    const MARK: &str = "/api/installers/";
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(MARK) {
        rest = &rest[i + MARK.len()..];
        let end = rest.find(['"', '<', ' ']).unwrap_or(rest.len());
        let candidate = rest[..end].split('?').next().unwrap_or("");
        if !candidate.is_empty() && !candidate.contains('/') && !out.iter().any(|s| s == candidate)
        {
            out.push(candidate.to_string());
        }
    }
    out
}

/// Run an async future from the sync mac-app pipeline: join the ambient
/// multi-thread runtime when present (CLI), otherwise build a dedicated one
/// (tests, library callers).
pub fn block_on_detached<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("detached tokio runtime")
            .block_on(fut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE_HTML: &str = r#"
    <a href="lutris:pharaoh-cd-cnc-ddraw">Install</a>
    <a href="/games/pharaoh-cd-cnc-ddraw/installer/edit">Edit installer</a>
    <a href="/api/installers/pharaoh-cd-cnc-ddraw?format=json">Download script</a>
    <a href="/api/installers/other-script?format=json">Download script</a>
    <a href="/api/installers/">broken</a>
    "#;

    #[test]
    fn parses_download_script_anchors_only() {
        let slugs = parse_script_slugs_from_page(PAGE_HTML);
        assert_eq!(slugs, vec!["pharaoh-cd-cnc-ddraw", "other-script"]);
    }

    #[tokio::test]
    async fn search_parses_gog_provider_ids() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/games"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "count": 1,
                    "results": [{
                        "id": 13896, "slug": "pharaoh", "name": "Pharaoh",
                        "provider_games": [
                            {"name": "Pharaoh + Cleopatra", "slug": "1207659046", "service": "gog"},
                            {"name": "Pharaoh", "slug": "564530", "service": "steam"}
                        ]
                    }]
                })),
            )
            .mount(&server)
            .await;
        let client = LutrisClient::new(&server.uri()).unwrap();
        let games = client.search("pharaoh").await.unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].gog_ids, vec!["1207659046"]);
        let hit = match_gog_game(&games, &["1207659046".to_string()]).unwrap();
        assert_eq!(hit.slug, "pharaoh");
        assert!(match_gog_game(&games, &["999".to_string()]).is_none());
    }

    #[tokio::test]
    async fn installer_fetch_parses_script_object() {
        let server = wiremock::MockServer::start().await;
        let body = std::fs::read_to_string("tests/fixtures/lutris-cuphead-gog.json").unwrap();
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/installers/cuphead-gog"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let client = LutrisClient::new(&server.uri()).unwrap();
        let inst = client.installer("cuphead-gog").await.unwrap();
        assert_eq!(inst.runner, "wine");
        assert_eq!(inst.gogid.as_deref(), Some("1963513391"));
        // the knowledge the parity executor feeds on: installer flags + winetricks
        let script = inst.script;
        let steps = script["installer"].as_array().unwrap();
        assert!(steps.iter().any(|s| {
            s.get("task")
                .and_then(|t| t.get("name"))
                .and_then(Value::as_str)
                == Some("winetricks")
        }));
        assert!(
            steps
                .iter()
                .any(|s| serde_json::to_string(s).unwrap().contains("/NOGUI"))
        );
    }

    #[tokio::test]
    async fn retry_gives_up_after_backoff_on_5xx() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = LutrisClient::new(&server.uri()).unwrap();
        let err = client.search("x").await.unwrap_err();
        assert!(err.to_string().contains("lutris unreachable"), "{err}");
    }

    #[test]
    fn wine_version_extraction() {
        let script = serde_json::json!({"wine": {"version": "lutris-7.2-2", "esync": true}});
        assert_eq!(wine_version(&script).as_deref(), Some("lutris-7.2-2"));
        assert_eq!(wine_version(&serde_json::json!({})), None);
        assert_eq!(
            wine_version(&serde_json::json!({"wine": {"esync": true}})),
            None
        );
    }

    #[test]
    fn pick_game_prefers_exact_name_then_slug_then_first() {
        let mk = |slug: &str, name: &str| LutrisGame {
            gog_ids: vec![],
            lutris_id: 0,
            slug: slug.into(),
            name: name.into(),
        };
        let games = vec![mk("other", "Unrelated"), mk("test-game", "Test Game")];
        assert_eq!(pick_game(&games, "Test Game").unwrap().slug, "test-game");
        assert_eq!(pick_game(&games, "test GAME").unwrap().slug, "test-game");
        // No name hit: slug equality beats list order.
        assert_eq!(pick_game(&games, "Test-Game").unwrap().slug, "test-game");
        // Nothing matches: first result (post-install cross-check guards).
        assert_eq!(pick_game(&games, "zzz").unwrap().slug, "other");
        assert!(pick_game(&[], "anything").is_none());
    }
}

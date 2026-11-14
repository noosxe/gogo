//! Live, read-only check of the cloud-save auth (design 22 §10, design 08 §5):
//! credentials from a fetched manifest, the per-game token exchange, and one
//! listing GET on the storage host. Nothing is written to the cloud or the
//! listing cache. The session is persisted back to the token store when the
//! Galaxy token was refreshed, as every gogo command does (`CloudAuth`), so the
//! stored session stays valid.
//!
//! ```sh
//! GOGO_LIVE=1 GOGO_LIVE_CLOUD_GOGID=<owned gogid> \
//!   cargo nextest run -p gogo-core --test live_cloud --run-ignored only
//! ```
//!
//! Prints only the status, the object count and the redacted URL.

use gogo::cloud::{auth, http};

#[tokio::test]
#[ignore = "live: needs a session and GOGO_LIVE=1"]
async fn exchange_and_list() {
    if std::env::var("GOGO_LIVE").as_deref() != Ok("1") {
        eprintln!("GOGO_LIVE is not 1: skipped");
        return;
    }
    let gogid: u64 = std::env::var("GOGO_LIVE_CLOUD_GOGID")
        .expect("GOGO_LIVE_CLOUD_GOGID")
        .parse()
        .expect("a numeric gogid");

    let cfg = gogo::config::Config::load().expect("config").config;
    let store = gogo::auth::store::open(cfg.auth.token_store);
    let session = gogo::auth::Session::restore(store.clone())
        .await
        .expect("session");
    let cloud = auth::CloudAuth::new(&session, &*store);

    // An empty dir: no kept manifest, no info file, so the fetch path runs.
    let empty = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let game = gogo::cloud::locations::Game {
        gogid: gogid.to_string(),
        layout: gogo::cloud::locations::Layout {
            lane: gogo::cloud::locations::Lane::host(),
            prefix: empty.path().join("prefix"),
            game_files: empty.path().to_path_buf(),
        },
        root: empty.path().to_path_buf(),
    };
    let creds = cloud
        .credentials(&game, None)
        .await
        .unwrap_or_else(|e| panic!("credentials: {e}"));
    let token = cloud
        .token(gogid, &creds)
        .await
        .unwrap_or_else(|e| panic!("exchange: {e}"));

    let client = http::StorageClient::new().unwrap();
    let req = client
        .request(reqwest::Method::GET, &token, &creds.client_id, None)
        .unwrap()
        .header("Accept", "application/json");
    // `StorageUrl` displays redacted.
    let url = client
        .url(&token, &creds.client_id, None)
        .unwrap()
        .to_string();
    let resp = client
        .send(req)
        .await
        .unwrap_or_else(|e| panic!("list: {e}"));
    let status = resp.status().as_u16();
    let listing: Vec<serde_json::Value> = resp.json().await.expect("a JSON listing");
    eprintln!(
        "cloud list: HTTP {status}, {} objects, {url}",
        listing.len()
    );
    assert_eq!(status, 200);
}

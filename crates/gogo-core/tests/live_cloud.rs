//! Live, read-only check of the cloud-save auth (design 22 §10, design 08 §5):
//! credentials from a fetched manifest, the per-game token exchange, and one
//! listing GET on the storage host. Nothing is written: not the token store
//! (the session is never persisted), not the listing cache, not the cloud.
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
    let session = gogo::auth::Session::restore(store).await.expect("session");
    let core = gogo::auth::TokenProvider::gog_warp_core(&session).expect("live core");

    // An empty dir: no kept manifest, no info file, so the fetch path runs.
    let empty = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let creds = auth::credentials(core, gogid, empty.path(), None)
        .await
        .unwrap_or_else(|e| panic!("credentials: {e}"));
    let token = auth::per_game_token(&session, gogid, &creds)
        .await
        .unwrap_or_else(|e| panic!("exchange: {e}"));

    let client = http::StorageClient::new().unwrap();
    let req = client
        .request(reqwest::Method::GET, &token, &creds.client_id, None)
        .header("Accept", "application/json");
    let url = http::redact_url(&client.url(&token, &creds.client_id, None));
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

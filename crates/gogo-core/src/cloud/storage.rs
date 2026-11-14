//! The storage API's read side (design 22 §1.3): the listing, object
//! downloads checked against the listed MD5, and tombstones.
//!
//! Objects are stored gzip-compressed and the listed `hash` is the MD5 of the
//! stored gzip bytes, so a download is checked **before** it is gunzipped:
//! bytes whose MD5 isn't the listed one are never decoded, never written.
//!
//! Nothing here writes to the cloud.

use std::io::Write;

use md5::{Digest, Md5};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use super::auth::CloudToken;
use super::error::CloudError;
use super::http::StorageClient;

/// How much of a listing is read: Cyberpunk's 231 entries are about 50 KB.
pub const LISTING_CAP: usize = 32 * 1024 * 1024;

/// The MD5 of the empty gzip stream Galaxy writes as a deletion marker
/// (89 bytes, design 22 §1.3). Taken from a live listing; hashes are public.
pub const GALAXY_TOMBSTONE_MD5: &str = "aadd86936a80ee8a369579c3926f1b3c";

/// One listed object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// `<location name>/<path below the location>`, `/`-separated.
    pub name: String,
    /// Size of the stored (gzip) object.
    pub bytes: u64,
    /// MD5 of the stored gzip bytes, lowercase hex.
    pub hash: String,
    /// Server time of the upload.
    #[serde(default)]
    pub last_modified: Option<String>,
}

impl Entry {
    /// A deletion marker: an empty gzip stream (Galaxy's or gogo's own form),
    /// or an empty object. A real empty save file looks the same; Galaxy has
    /// that limit too.
    pub fn is_tombstone(&self) -> bool {
        self.bytes == 0 || self.hash == GALAXY_TOMBSTONE_MD5 || self.hash == empty_gzip_md5()
    }

    /// The location name and the path below it; `None` for a name with no
    /// `/` (it belongs to no location).
    pub fn split(&self) -> Option<(&str, &str)> {
        self.name
            .split_once('/')
            .filter(|(l, r)| !l.is_empty() && !r.is_empty())
    }
}

/// The container: every location's objects in one flat list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// `x-container-object-count`.
    pub object_count: Option<u64>,
    /// `x-container-bytes-used`.
    pub bytes_used: Option<u64>,
    /// `x-container-meta-quota-bytes`.
    pub quota_bytes: Option<u64>,
}

fn is_md5_hex(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn header_u64(h: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    h.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// Read at most `cap` bytes of `resp`; `None` when it has more.
async fn body_capped(
    mut resp: reqwest::Response,
    cap: usize,
) -> Result<Option<Vec<u8>>, CloudError> {
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| CloudError::Network(super::auth::reqwest_kind(&e).to_string()))?
    {
        if body.len() + chunk.len() > cap {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// Parse a JSON listing body and the container headers. A listing shorter
/// than the container's own object count is refused (design 22 as built):
/// the missing objects would look deleted.
pub fn parse_listing(
    body: &[u8],
    headers: &reqwest::header::HeaderMap,
) -> Result<Listing, CloudError> {
    let entries: Vec<Entry> =
        serde_json::from_slice(body).map_err(|e| CloudError::Listing(e.to_string()))?;
    if let Some(bad) = entries.iter().find(|e| !is_md5_hex(&e.hash)) {
        return Err(CloudError::Listing(format!(
            "object {:?} has no MD5 hash",
            bad.name
        )));
    }
    let listing = Listing {
        object_count: header_u64(headers, "x-container-object-count"),
        bytes_used: header_u64(headers, "x-container-bytes-used"),
        quota_bytes: header_u64(headers, "x-container-meta-quota-bytes"),
        entries,
    };
    if let Some(count) = listing.object_count
        && count > listing.entries.len() as u64
    {
        return Err(CloudError::ListingTruncated {
            listed: listing.entries.len(),
            count,
        });
    }
    Ok(listing)
}

/// `GET /v1/<userId>/<clientId>` as JSON: every location's objects.
pub async fn list(
    client: &StorageClient,
    token: &CloudToken,
    client_id: &str,
) -> Result<Listing, CloudError> {
    let req = client
        .request(Method::GET, token, client_id, None)?
        .header(reqwest::header::ACCEPT, "application/json");
    let resp = client.send(req).await?;
    let headers = resp.headers().clone();
    let body = body_capped(resp, LISTING_CAP)
        .await?
        .ok_or_else(|| CloudError::Listing(format!("larger than {LISTING_CAP} bytes")))?;
    parse_listing(&body, &headers)
}

/// The stored gzip bytes of `entry`, checked against its listed size and MD5.
pub async fn fetch_gzip(
    client: &StorageClient,
    token: &CloudToken,
    client_id: &str,
    entry: &Entry,
) -> Result<Vec<u8>, CloudError> {
    let req = client.request(Method::GET, token, client_id, Some(&entry.name))?;
    let resp = client.send(req).await?;
    let cap = usize::try_from(entry.bytes).unwrap_or(usize::MAX);
    let corrupt = |reason: String| CloudError::Corrupt {
        name: entry.name.clone(),
        reason,
    };
    let body = body_capped(resp, cap)
        .await?
        .ok_or_else(|| corrupt(format!("more than the listed {} bytes", entry.bytes)))?;
    verify(entry, &body)?;
    Ok(body)
}

/// Check stored bytes against the listing: the MD5 is the listed `hash`.
pub fn verify(entry: &Entry, gzip: &[u8]) -> Result<(), CloudError> {
    let got = md5_hex(gzip);
    if got != entry.hash {
        return Err(CloudError::Corrupt {
            name: entry.name.clone(),
            reason: format!("MD5 {got} isn't the listed {}", entry.hash),
        });
    }
    Ok(())
}

/// Gunzip already verified bytes into `out`; the plain size.
pub fn gunzip_into(name: &str, gzip: &[u8], out: &mut impl Write) -> Result<u64, CloudError> {
    let mut dec = flate2::read::MultiGzDecoder::new(gzip);
    std::io::copy(&mut dec, out).map_err(|e| CloudError::Corrupt {
        name: name.to_string(),
        reason: format!("not a gzip stream ({e})"),
    })
}

/// The plain file's MD5 and size of `entry`: downloaded into memory, checked,
/// gunzipped into a hasher. Nothing is written to disk.
pub async fn plain_md5(
    client: &StorageClient,
    token: &CloudToken,
    client_id: &str,
    entry: &Entry,
) -> Result<(String, u64), CloudError> {
    let gzip = fetch_gzip(client, token, client_id, entry).await?;
    let mut h = HashWriter(Md5::new());
    let size = gunzip_into(&entry.name, &gzip, &mut h)?;
    Ok((crate::fmt::md5_hex(h.0), size))
}

struct HashWriter(Md5);

impl Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn md5_hex(bytes: &[u8]) -> String {
    let mut h = Md5::new();
    h.update(bytes);
    crate::fmt::md5_hex(h)
}

/// gogo's gzip form: level 6, no name, mtime 0, so equal files compress to
/// equal bytes (design 22 §9). The upload side (#462) uses it; here it
/// defines gogo's own tombstone.
pub fn gzip(plain: &[u8]) -> Vec<u8> {
    let mut enc = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::new(6));
    enc.write_all(plain).expect("writing to a Vec");
    enc.finish().expect("writing to a Vec")
}

/// The MD5 of [`gzip`] of nothing.
pub fn empty_gzip_md5() -> &'static str {
    static MD5: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    MD5.get_or_init(|| md5_hex(&gzip(b"")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn token() -> CloudToken {
        CloudToken::new("made-up-access", "7", 3600)
    }

    async fn client(server: &MockServer) -> StorageClient {
        StorageClient::with_base(format!("{}/v1/", server.uri()).parse().unwrap()).unwrap()
    }

    fn entry(name: &str, gz: &[u8]) -> Entry {
        Entry {
            name: name.into(),
            bytes: gz.len() as u64,
            hash: md5_hex(gz),
            last_modified: Some("2024-02-11T03:49:20.911250+00:00".into()),
        }
    }

    #[test]
    fn tombstones() {
        let mut e = entry("saves/a", &gzip(b""));
        assert!(e.is_tombstone());
        e.hash = GALAXY_TOMBSTONE_MD5.into();
        e.bytes = 89;
        assert!(e.is_tombstone());
        assert!(!entry("saves/a", &gzip(b"x")).is_tombstone());
        let empty = Entry {
            bytes: 0,
            ..entry("saves/a", b"")
        };
        assert!(empty.is_tombstone());
    }

    #[test]
    fn names_split_into_location_and_path() {
        let e = |n: &str| entry(n, b"x");
        assert_eq!(e("saves/a/b.sav").split(), Some(("saves", "a/b.sav")));
        assert_eq!(e("loose").split(), None);
        assert_eq!(e("saves/").split(), None);
        assert_eq!(e("/x").split(), None);
    }

    #[test]
    fn gzip_is_deterministic_and_round_trips() {
        assert_eq!(gzip(b"save"), gzip(b"save"));
        let mut out = Vec::new();
        gunzip_into("n", &gzip(b"save"), &mut out).unwrap();
        assert_eq!(out, b"save");
        assert!(matches!(
            gunzip_into("n", b"plain", &mut Vec::new()),
            Err(CloudError::Corrupt { .. })
        ));
    }

    const LISTING: &str = r#"[
      {"name":"saves/s1/sav.dat","bytes":4,"hash":"0123456789abcdef0123456789abcdef",
       "last_modified":"2024-02-11T03:49:20.911250+00:00","content_type":"application/octet-stream"},
      {"name":"saves/old.dat","bytes":89,"hash":"aadd86936a80ee8a369579c3926f1b3c",
       "last_modified":"2024-02-11T03:49:20.911250+00:00","content_type":"application/octet-stream"}
    ]"#;

    #[tokio::test]
    async fn lists_as_json_with_the_container_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/7/c1"))
            .and(header("accept", "application/json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-container-object-count", "2")
                    .insert_header("x-container-bytes-used", "93")
                    .insert_header("x-container-meta-quota-bytes", "209715200")
                    .set_body_string(LISTING),
            )
            .expect(1)
            .mount(&server)
            .await;
        let l = list(&client(&server).await, &token(), "c1").await.unwrap();
        assert_eq!(l.entries.len(), 2);
        assert_eq!(l.object_count, Some(2));
        assert_eq!(l.bytes_used, Some(93));
        assert_eq!(l.quota_bytes, Some(209_715_200));
        assert!(!l.entries[0].is_tombstone());
        assert!(l.entries[1].is_tombstone());
    }

    #[tokio::test]
    async fn a_short_listing_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-container-object-count", "3")
                    .set_body_string(LISTING),
            )
            .mount(&server)
            .await;
        let e = list(&client(&server).await, &token(), "c1")
            .await
            .unwrap_err();
        assert!(matches!(
            e,
            CloudError::ListingTruncated {
                listed: 2,
                count: 3
            }
        ));
    }

    #[test]
    fn a_listing_that_isnt_the_expected_json_is_refused() {
        let h = reqwest::header::HeaderMap::new();
        assert!(matches!(
            parse_listing(b"saves/a\nsaves/b\n", &h),
            Err(CloudError::Listing(_))
        ));
        assert!(matches!(
            parse_listing(br#"[{"name":"a/b","bytes":1,"hash":"nope"}]"#, &h),
            Err(CloudError::Listing(_))
        ));
        assert_eq!(parse_listing(b"[]", &h).unwrap().entries, vec![]);
    }

    #[tokio::test]
    async fn invalid_version_is_a_storage_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                r#"{"error":"invalid_version","error_description":"not supported with this version"}"#,
            ))
            .mount(&server)
            .await;
        let e = list(&client(&server).await, &token(), "c1")
            .await
            .unwrap_err();
        assert!(matches!(
            &e,
            CloudError::Storage { status: 400, code: Some(c), .. } if c == "invalid_version"
        ));
        assert!(!e.to_string().contains("/7/"));
    }

    #[tokio::test]
    async fn downloads_are_checked_before_gunzip() {
        let server = MockServer::start().await;
        let gz = gzip(b"the save");
        Mock::given(method("GET"))
            .and(path("/v1/7/c1/saves/s%201/sav.dat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-encoding", "gzip")
                    .set_body_bytes(gz.clone()),
            )
            .mount(&server)
            .await;
        let c = client(&server).await;
        let good = entry("saves/s 1/sav.dat", &gz);
        let (md5, size) = plain_md5(&c, &token(), "c1", &good).await.unwrap();
        assert_eq!((md5.as_str(), size), (md5_hex(b"the save").as_str(), 8));

        let wrong = Entry {
            hash: "0".repeat(32),
            ..good.clone()
        };
        let e = plain_md5(&c, &token(), "c1", &wrong).await.unwrap_err();
        assert!(matches!(e, CloudError::Corrupt { .. }));
        assert_eq!(e.exit_code(), 5);

        let shorter = Entry {
            bytes: 3,
            ..good.clone()
        };
        let e = fetch_gzip(&c, &token(), "c1", &shorter).await.unwrap_err();
        assert!(e.to_string().contains("more than the listed 3 bytes"));
    }
}

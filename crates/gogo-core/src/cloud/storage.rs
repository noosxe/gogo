//! The storage API's read side (design 22 §1.3): the listing, object
//! downloads checked against the listed MD5, and tombstones.
//!
//! Objects are stored gzip-compressed and the listed `hash` is the MD5 of the
//! stored gzip bytes, so a download is checked **before** it is gunzipped:
//! bytes whose MD5 isn't the listed one are never decoded, never written.
//!
//! The write side ([`put_object`], [`delete_object`]) is only ever called by
//! `gogo saves push|sync`, behind design 22's D2 gate.

use std::io::Write;

use md5::{Digest, Md5};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use super::auth::CloudToken;
use super::error::CloudError;
use super::http::StorageClient;

/// How much of a listing is read: Cyberpunk's 231 entries are about 50 KB.
pub const LISTING_CAP: usize = 32 * 1024 * 1024;

/// The largest stored object gogo downloads: the smaller of the two quotas
/// seen live (design 22 §1.3, 200 MiB); no object of a container can be
/// larger. The listed `bytes` is untrusted, so it is clamped to this.
pub const MAX_OBJECT_GZIP: u64 = 209_715_200;

/// The largest plain file a download may gunzip to. Real saves are a few
/// MB; a gzip that expands past this is refused as corrupt (a bomb).
pub const MAX_PLAIN: u64 = 2 * 1024 * 1024 * 1024;

/// Swift's default listing limit: a listing of exactly this many entries
/// with no object count may be a first page.
pub const SWIFT_PAGE: usize = 10_000;

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
    /// The listing answered 404: the container doesn't exist yet (nothing was
    /// ever uploaded). Planned as empty; unverified live.
    pub missing: bool,
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
    let mut entries: Vec<Entry> =
        serde_json::from_slice(body).map_err(|e| CloudError::Listing(e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    for e in &mut entries {
        e.hash.make_ascii_lowercase();
        if !is_md5_hex(&e.hash) {
            return Err(CloudError::Listing(format!(
                "object {:?} has no MD5 hash",
                e.name
            )));
        }
        if !seen.insert(e.name.clone()) {
            return Err(CloudError::Listing(format!(
                "object {:?} is listed twice",
                e.name
            )));
        }
    }
    let listing = Listing {
        object_count: header_u64(headers, "x-container-object-count"),
        bytes_used: header_u64(headers, "x-container-bytes-used"),
        quota_bytes: header_u64(headers, "x-container-meta-quota-bytes"),
        entries,
        missing: false,
    };
    let listed = listing.entries.len();
    match listing.object_count {
        Some(count) if count > listed as u64 => {
            return Err(CloudError::ListingTruncated { listed, count });
        }
        // No count to check against and a full first page: maybe more.
        None if listed == SWIFT_PAGE => {
            return Err(CloudError::ListingTruncated {
                listed,
                count: listed as u64 + 1,
            });
        }
        _ => {}
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
    let resp = match client.send(req).await {
        Ok(r) => r,
        // The container is made by the first upload: none yet, nothing in it.
        Err(CloudError::Storage { status: 404, .. }) => {
            return Ok(Listing {
                entries: vec![],
                object_count: Some(0),
                bytes_used: None,
                quota_bytes: None,
                missing: true,
            });
        }
        Err(e) => return Err(e),
    };
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
    Ok(fetch_object(client, token, client_id, entry).await?.gzip)
}

/// A downloaded object: the stored bytes, checked, and the file's mtime as
/// the uploader saw it.
#[derive(Debug, Clone)]
pub struct Object {
    pub gzip: Vec<u8>,
    /// `X-Object-Meta-LocalLastModified`, when it parses and is plausible.
    pub local_last_modified: Option<std::time::SystemTime>,
    /// The header as sent, when it was there but unusable (not RFC 3339, or
    /// outside [`plausible_mtime`]'s window): for a warning.
    pub bad_local_last_modified: Option<String>,
}

/// An uploader's mtime gogo sets on a file: from 1980-01-01 (the FAT
/// epoch, before any save of a GOG game) to a day after now (clock skew).
pub fn plausible_mtime(t: std::time::SystemTime, now: std::time::SystemTime) -> bool {
    let floor = std::time::UNIX_EPOCH + std::time::Duration::from_secs(315_532_800);
    let ceiling = now + std::time::Duration::from_secs(86_400);
    t >= floor && t <= ceiling
}

/// The `X-Object-Meta-LocalLastModified` header: `Ok(None)` when absent,
/// `Ok(Some(t))` for an RFC 3339 time [`plausible_mtime`] accepts, `Err`
/// with the raw value (control characters dropped) for anything else.
pub fn local_last_modified(
    h: &reqwest::header::HeaderMap,
    now: std::time::SystemTime,
) -> Result<Option<std::time::SystemTime>, String> {
    let Some(v) = h.get("x-object-meta-locallastmodified") else {
        return Ok(None);
    };
    let raw = || {
        String::from_utf8_lossy(v.as_bytes())
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect::<String>()
    };
    let t: std::time::SystemTime = v
        .to_str()
        .ok()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s.trim()).ok())
        .map(|t| t.with_timezone(&chrono::Utc).into())
        .ok_or_else(raw)?;
    if plausible_mtime(t, now) {
        Ok(Some(t))
    } else {
        Err(raw())
    }
}

/// [`fetch_gzip`] with the object's `X-Object-Meta-LocalLastModified`.
pub async fn fetch_object(
    client: &StorageClient,
    token: &CloudToken,
    client_id: &str,
    entry: &Entry,
) -> Result<Object, CloudError> {
    let corrupt = |reason: String| CloudError::Corrupt {
        name: entry.name.clone(),
        reason,
    };
    if entry.bytes > MAX_OBJECT_GZIP {
        return Err(corrupt(format!(
            "listed as {} bytes, more than any container holds ({MAX_OBJECT_GZIP})",
            entry.bytes
        )));
    }
    let req = client.request(Method::GET, token, client_id, Some(&entry.name))?;
    let resp = client.send(req).await?;
    let mtime = local_last_modified(resp.headers(), std::time::SystemTime::now());
    let cap = usize::try_from(entry.bytes).unwrap_or(usize::MAX);
    let body = body_capped(resp, cap)
        .await?
        .ok_or_else(|| corrupt(format!("more than the listed {} bytes", entry.bytes)))?;
    verify(entry, &body)?;
    let (local_last_modified, bad_local_last_modified) = match mtime {
        Ok(t) => (t, None),
        Err(raw) => (None, Some(raw)),
    };
    Ok(Object {
        gzip: body,
        local_last_modified,
        bad_local_last_modified,
    })
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

/// Gunzip already verified bytes into `out`, at most `max_plain` bytes of
/// them ([`MAX_PLAIN`] outside tests); the plain size. More is
/// [`CloudError::Corrupt`] (a gzip bomb), and `out` may hold a prefix then.
pub fn gunzip_into(
    name: &str,
    gzip: &[u8],
    out: &mut impl Write,
    max_plain: u64,
) -> Result<u64, CloudError> {
    use std::io::Read;
    let corrupt = |reason: String| CloudError::Corrupt {
        name: name.to_string(),
        reason,
    };
    let mut dec = flate2::read::MultiGzDecoder::new(gzip).take(max_plain.saturating_add(1));
    let n =
        std::io::copy(&mut dec, out).map_err(|e| corrupt(format!("not a gzip stream ({e})")))?;
    if n > max_plain {
        return Err(corrupt(format!(
            "gunzips to more than {max_plain} bytes; not a save"
        )));
    }
    Ok(n)
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
    let size = gunzip_into(&entry.name, &gzip, &mut h, MAX_PLAIN)?;
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

/// A local file in gogo's gzip form, with what the upload and the record
/// need: the stored bytes' MD5 and the plain file's MD5 and size.
#[derive(Debug)]
pub struct Gzipped {
    pub gzip: Vec<u8>,
    /// MD5 of `gzip`: the `Etag` of the upload and the listing's `hash`.
    pub gzip_md5: String,
    pub plain_md5: String,
    pub plain_len: u64,
}

/// Read `r` to its end into [`gzip`]'s form (the same bytes [`gzip`] gives
/// for the same content). `Ok(None)` when the stored form would be larger
/// than `max_gzip`, or the plain file than [`MAX_PLAIN`]: nothing that large
/// is uploaded.
pub fn gzip_reader(r: &mut dyn std::io::Read, max_gzip: u64) -> std::io::Result<Option<Gzipped>> {
    let mut enc = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::new(6));
    let mut plain = Md5::new();
    let mut plain_len = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        plain.update(&buf[..n]);
        plain_len += n as u64;
        if plain_len > MAX_PLAIN {
            return Ok(None);
        }
        enc.write_all(&buf[..n])?;
        if enc.get_ref().len() as u64 > max_gzip {
            return Ok(None);
        }
    }
    let gzip = enc.finish()?;
    if gzip.len() as u64 > max_gzip {
        return Ok(None);
    }
    Ok(Some(Gzipped {
        gzip_md5: md5_hex(&gzip),
        plain_md5: crate::fmt::md5_hex(plain),
        plain_len,
        gzip,
    }))
}

/// `PUT` one object: the gzip body with `Content-Encoding: gzip`, its MD5 as
/// `Etag` and the file's mtime (whole seconds, UTC, RFC 3339) as
/// `X-Object-Meta-LocalLastModified` (design 22 §1.3; left out when the
/// mtime is unknown). `create_only` adds `If-None-Match: *`, so a name
/// someone else created since the listing isn't overwritten (a 412 is
/// [`CloudError::Storage`] with that status). **Unverified live (D2)**; the
/// `gate` can only come from the pull runner's gate check. An `Etag` in the
/// answer that isn't ours (a weak one compared by its value) means the cloud
/// stored something else: [`CloudError::Corrupt`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn put_object(
    _gate: &super::pull::CloudWrites,
    client: &StorageClient,
    token: &CloudToken,
    client_id: &str,
    name: &str,
    gzip: Vec<u8>,
    gzip_md5: &str,
    mtime: Option<std::time::SystemTime>,
    create_only: bool,
) -> Result<(), CloudError> {
    let mut req = client
        .request(Method::PUT, token, client_id, Some(name))?
        .header(reqwest::header::CONTENT_ENCODING, "gzip")
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .header(reqwest::header::ETAG, gzip_md5);
    if let Some(t) = mtime {
        let stamp = chrono::DateTime::<chrono::Utc>::from(t)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        req = req.header("X-Object-Meta-LocalLastModified", stamp);
    }
    if create_only {
        req = req.header(reqwest::header::IF_NONE_MATCH, "*");
    }
    let resp = client.send(req.body(gzip)).await?;
    if let Some(got) = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
    {
        let got = etag_value(got);
        if got != gzip_md5 {
            return Err(CloudError::Corrupt {
                name: name.to_string(),
                reason: format!("the cloud stored MD5 {got}, not the uploaded {gzip_md5}"),
            });
        }
    }
    Ok(())
}

/// An `ETag` header's value: no quotes, no weak marker, lowercase.
fn etag_value(h: &str) -> String {
    let h = h.trim();
    let h = h.strip_prefix("W/").unwrap_or(h);
    h.trim_matches('"').to_ascii_lowercase()
}

/// `DELETE` one object. **Unverified live (D2)**: whether the cloud then lists
/// a tombstone or nothing is what the live test shows; both read as deleted.
/// A 404 is success: it is gone.
pub(crate) async fn delete_object(
    _gate: &super::pull::CloudWrites,
    client: &StorageClient,
    token: &CloudToken,
    client_id: &str,
    name: &str,
) -> Result<(), CloudError> {
    let req = client.request(Method::DELETE, token, client_id, Some(name))?;
    match client.send(req).await {
        Ok(_) | Err(CloudError::Storage { status: 404, .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// What a `HEAD` of one object says.
#[derive(Debug, Clone, Default)]
pub struct Head {
    /// `ETag`, normalised ([`etag_value`]): the MD5 of the stored bytes.
    pub etag: Option<String>,
    /// `X-Object-Meta-LocalLastModified`, when usable.
    pub local_last_modified: Option<std::time::SystemTime>,
}

/// `HEAD` one object; `None` for a 404 (gone).
pub async fn head_object(
    client: &StorageClient,
    token: &CloudToken,
    client_id: &str,
    name: &str,
) -> Result<Option<Head>, CloudError> {
    let req = client.request(Method::HEAD, token, client_id, Some(name))?;
    let resp = match client.send(req).await {
        Ok(r) => r,
        Err(CloudError::Storage { status: 404, .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(Some(Head {
        etag: resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(etag_value),
        local_last_modified: local_last_modified(resp.headers(), std::time::SystemTime::now())
            .unwrap_or(None),
    }))
}

/// `HEAD` one object for its `X-Object-Meta-LocalLastModified` (`--prefer
/// newer`): `None` when it has none, an unusable one, or is gone.
pub async fn head_local_last_modified(
    client: &StorageClient,
    token: &CloudToken,
    client_id: &str,
    name: &str,
) -> Result<Option<std::time::SystemTime>, CloudError> {
    Ok(head_object(client, token, client_id, name)
        .await?
        .and_then(|h| h.local_last_modified))
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
    fn gzip_reader_gives_the_same_bytes_as_gzip_and_stops_at_the_cap() {
        let plain = b"some save data, some save data, some save data".to_vec();
        let g = gzip_reader(&mut plain.as_slice(), 1 << 20)
            .unwrap()
            .unwrap();
        assert_eq!(g.gzip, gzip(&plain));
        assert_eq!(g.gzip_md5, md5_hex(&g.gzip));
        assert_eq!(g.plain_md5, md5_hex(&plain));
        assert_eq!(g.plain_len, plain.len() as u64);
        assert!(gzip_reader(&mut plain.as_slice(), 4).unwrap().is_none());
    }

    #[tokio::test]
    async fn put_sends_the_headers_and_refuses_a_foreign_etag() {
        let server = MockServer::start().await;
        let gz = gzip(b"hello");
        let md5 = md5_hex(&gz);
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        Mock::given(method("PUT"))
            .and(path("/v1/7/cid/saves/a%20b.sav"))
            .and(header("content-encoding", "gzip"))
            .and(header("etag", md5.as_str()))
            .and(header(
                "x-object-meta-locallastmodified",
                "2023-11-14T22:13:20Z",
            ))
            .respond_with(ResponseTemplate::new(201).insert_header("etag", format!("\"{md5}\"")))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(&server).await;
        let gate = crate::cloud::pull::CloudWrites::for_tests();
        // A weak Etag is compared by its value.
        let weak = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(201).insert_header("etag", format!("W/\"{md5}\"")))
            .mount(&weak)
            .await;
        put_object(
            &gate,
            &client(&weak).await,
            &token(),
            "cid",
            "saves/w",
            gz.clone(),
            &md5,
            None,
            false,
        )
        .await
        .unwrap();
        let sent = &weak.received_requests().await.unwrap()[0];
        assert!(
            !sent.headers.contains_key("x-object-meta-locallastmodified"),
            "no mtime, no header"
        );
        assert!(!sent.headers.contains_key("if-none-match"));

        put_object(
            &gate,
            &c,
            &token(),
            "cid",
            "saves/a b.sav",
            gz.clone(),
            &md5,
            Some(at),
            false,
        )
        .await
        .unwrap();

        // A new name is created only: a 412 says someone made it meanwhile.
        let created = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(header("if-none-match", "*"))
            .respond_with(ResponseTemplate::new(412))
            .mount(&created)
            .await;
        let e = put_object(
            &gate,
            &client(&created).await,
            &token(),
            "cid",
            "saves/n",
            gz.clone(),
            &md5,
            None,
            true,
        )
        .await
        .unwrap_err();
        assert!(matches!(e, CloudError::Storage { status: 412, .. }), "{e}");

        let other = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(201).insert_header("etag", "0".repeat(32)))
            .mount(&other)
            .await;
        let e = put_object(
            &gate,
            &client(&other).await,
            &token(),
            "cid",
            "saves/x",
            gz,
            &md5,
            Some(at),
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(e, CloudError::Corrupt { .. }), "{e}");
    }

    #[tokio::test]
    async fn head_reads_the_etag_and_a_404_is_gone() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/v1/7/cid/saves/a"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("etag", "\"ABCDEF00ABCDEF00ABCDEF00ABCDEF00\"")
                    .insert_header("x-object-meta-locallastmodified", "2024-02-11T03:49:20Z"),
            )
            .mount(&server)
            .await;
        let c = client(&server).await;
        let h = head_object(&c, &token(), "cid", "saves/a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.etag.as_deref(), Some("abcdef00abcdef00abcdef00abcdef00"));
        assert!(h.local_last_modified.is_some());
        assert!(
            head_object(&c, &token(), "cid", "saves/none")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            head_local_last_modified(&c, &token(), "cid", "saves/none")
                .await
                .unwrap()
                .is_none(),
            "a 404 is no header, not an abort"
        );
    }

    #[tokio::test]
    async fn delete_treats_a_missing_object_as_gone() {
        let gate = crate::cloud::pull::CloudWrites::for_tests();
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/7/cid/saves/a"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/7/cid/saves/b"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let c = client(&server).await;
        delete_object(&gate, &c, &token(), "cid", "saves/a")
            .await
            .unwrap();
        let e = delete_object(&gate, &c, &token(), "cid", "saves/b")
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), 3);
    }

    #[test]
    fn the_uploaders_mtime_is_used_only_when_plausible() {
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
        let h = |v: &str| {
            let mut m = reqwest::header::HeaderMap::new();
            m.insert("x-object-meta-locallastmodified", v.parse().unwrap());
            m
        };
        assert_eq!(
            local_last_modified(&reqwest::header::HeaderMap::new(), now),
            Ok(None)
        );
        assert_eq!(
            local_last_modified(&h("2024-02-11T03:49:20Z"), now),
            Ok(Some(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_707_623_360)
            ))
        );
        for bad in ["1970-01-01T00:00:00Z", "2099-01-01T00:00:00Z", "yesterday"] {
            assert_eq!(
                local_last_modified(&h(bad), now),
                Err(bad.to_string()),
                "{bad}"
            );
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
        gunzip_into("n", &gzip(b"save"), &mut out, MAX_PLAIN).unwrap();
        assert_eq!(out, b"save");
        assert!(matches!(
            gunzip_into("n", b"plain", &mut Vec::new(), MAX_PLAIN),
            Err(CloudError::Corrupt { .. })
        ));
    }

    #[test]
    fn a_gzip_bomb_is_refused_at_the_plain_cap() {
        // 4 MiB of zeros compress to a few KiB.
        let bomb = gzip(&vec![0u8; 4 << 20]);
        assert!(bomb.len() < 64 << 10);
        let mut h = HashWriter(Md5::new());
        let e = gunzip_into("saves/bomb", &bomb, &mut h, 1 << 20).unwrap_err();
        assert!(e.to_string().contains("more than 1048576 bytes"), "{e}");
        assert_eq!(e.exit_code(), 5);
        // Exactly at the cap is fine.
        let at = gzip(&vec![0u8; 1 << 20]);
        assert_eq!(gunzip_into("n", &at, &mut h, 1 << 20).unwrap(), 1 << 20);
    }

    #[tokio::test]
    async fn an_absurd_listed_size_is_refused_before_any_download() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(gzip(b"x")))
            .expect(0)
            .mount(&server)
            .await;
        let c = client(&server).await;
        let huge = Entry {
            bytes: u64::MAX,
            ..entry("saves/a", &gzip(b"x"))
        };
        let e = fetch_gzip(&c, &token(), "c1", &huge).await.unwrap_err();
        assert!(matches!(e, CloudError::Corrupt { .. }), "{e}");
    }

    #[test]
    fn hashes_are_lowercased_and_duplicates_refused() {
        let h = reqwest::header::HeaderMap::new();
        let l = parse_listing(
            br#"[{"name":"a/b","bytes":1,"hash":"0123456789ABCDEF0123456789ABCDEF"}]"#,
            &h,
        )
        .unwrap();
        assert_eq!(l.entries[0].hash, "0123456789abcdef0123456789abcdef");
        let dup = br#"[{"name":"a/b","bytes":1,"hash":"0123456789abcdef0123456789abcdef"},
                      {"name":"a/b","bytes":1,"hash":"0123456789abcdef0123456789abcdef"}]"#;
        let e = parse_listing(dup, &h).unwrap_err();
        assert!(e.to_string().contains("listed twice"), "{e}");
    }

    #[test]
    fn a_full_page_without_a_count_may_be_truncated() {
        let one = |i: usize| {
            serde_json::json!({"name": format!("s/{i}"), "bytes": 1,
                               "hash": "0123456789abcdef0123456789abcdef"})
        };
        let full: Vec<_> = (0..SWIFT_PAGE).map(one).collect();
        let body = serde_json::to_vec(&full).unwrap();
        let none = reqwest::header::HeaderMap::new();
        assert!(matches!(
            parse_listing(&body, &none),
            Err(CloudError::ListingTruncated { .. })
        ));
        let mut counted = reqwest::header::HeaderMap::new();
        counted.insert("x-container-object-count", "10000".parse().unwrap());
        assert_eq!(
            parse_listing(&body, &counted).unwrap().entries.len(),
            SWIFT_PAGE
        );
        let short: Vec<_> = (0..SWIFT_PAGE - 1).map(one).collect();
        assert!(parse_listing(&serde_json::to_vec(&short).unwrap(), &none).is_ok());
    }

    #[tokio::test]
    async fn a_missing_container_is_an_empty_listing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/7/c1"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let l = list(&client(&server).await, &token(), "c1").await.unwrap();
        assert!(l.missing && l.entries.is_empty());
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

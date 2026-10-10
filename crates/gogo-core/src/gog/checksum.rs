//! Checksum XML → `ChecksumDoc` (`docs/design/04` §2–§3, review A2).
//!
//! The XML is **authoritative** (live-verified): `total_size` here can exceed the API's
//! `total_size` (724,243,232 vs 723,517,440 in our sample); the save filename comes from
//! the root element's `name` attribute. 10 MiB chunks, last one partial.
//!
//! The root also carries `available` / `notavailablemsg` (review A2):
//! `available="false"` means GOG itself says the file is not downloadable — the engine
//! skips such files with a `Warn` carrying the message; it is never treated as the
//! no-chunks case.

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use crate::gog::error::ApiError;

/// Live-verified chunk size.
pub const CHUNK_SIZE: u64 = 10 * 1024 * 1024;

/// Parsed checksum document.
///
/// Constructed only by [`parse`] / [`fetch`]; `warnings` holds non-fatal observations
/// (span/size mismatch, unknown chunk method) the engine surfaces as `Warn` events
/// (docs/design/08 §2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChecksumDoc {
    /// Save filename, e.g. `setup_<slug>_<version>_(<buildid>).exe`.
    pub name: String,
    /// `available="false"` → skip with `Warn(notavailablemsg)` (review A2).
    /// Absent attribute means available (live behavior).
    pub available: bool,
    pub notavailablemsg: Option<String>,
    /// Whole-file md5, when present.
    pub md5: Option<String>,
    /// Unix timestamp — used by `--strict` update detection (docs/design/06 §4).
    pub timestamp: Option<u64>,
    pub total_size: u64,
    pub chunks: Vec<Chunk>,
    /// Non-fatal parse observations.
    pub warnings: Vec<String>,
}

/// One `<chunk id from to method="md5">hex</chunk>`; `to` is the inclusive end offset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chunk {
    pub id: u32,
    pub from: u64,
    pub to: u64,
    /// Only `md5` exists live; anything else is recorded as a doc warning.
    pub method: String,
    pub md5: String,
}

/// Fetch + parse the checksum XML (public CDN, no auth) — tiny docs, buffered.
pub async fn fetch(http: &reqwest::Client, checksum_url: &str) -> Result<ChecksumDoc, ApiError> {
    let resp = ApiError::check(http.get(checksum_url).send().await?).await?;
    parse(&resp.text().await?)
}

/// Parse the checksum XML (BOM-tolerant, attribute-order agnostic).
///
/// Hard failures (malformed XML, missing root `name`) are errors; everything else that
/// looks off becomes a [`ChecksumDoc::warnings`] entry so a slightly-odd doc still
/// downloads (08 §2 test inventory).
pub fn parse(xml: &str) -> Result<ChecksumDoc, ApiError> {
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut doc = ChecksumDoc {
        available: true,
        ..ChecksumDoc::default()
    };
    let mut buf = Vec::new();
    let mut in_chunks = false;
    let mut cur: Option<Chunk> = None;

    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| ApiError::Other(format!("checksum XML: {e}")))?;
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                "file" => file_attrs(&e, &mut doc)?,
                "chunks" => in_chunks = true,
                "chunk" if in_chunks => cur = Some(chunk_attrs(&e)),
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                "file" => file_attrs(&e, &mut doc)?,
                "chunk" if in_chunks => doc.chunks.push(chunk_attrs(&e)),
                _ => {}
            },
            Event::Text(t) => {
                if let Some(chunk) = cur.as_mut() {
                    chunk.md5 = t.xml10_content().into_owned();
                }
            }
            Event::End(e) => match e.name().as_ref() {
                "chunk" => {
                    if let Some(chunk) = cur.take() {
                        doc.chunks.push(chunk);
                    }
                }
                "chunks" => in_chunks = false,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if doc.name.is_empty() {
        return Err(ApiError::Other("checksum XML: root has no name".into()));
    }
    validate(&mut doc);
    Ok(doc)
}

fn attr(e: &BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

fn file_attrs(e: &BytesStart<'_>, doc: &mut ChecksumDoc) -> Result<(), ApiError> {
    doc.name =
        attr(e, "name").ok_or_else(|| ApiError::Other("checksum XML: root has no name".into()))?;
    if let Some(v) = attr(e, "available") {
        doc.available = !v.eq_ignore_ascii_case("false");
    }
    doc.notavailablemsg = attr(e, "notavailablemsg");
    doc.md5 = attr(e, "md5");
    doc.timestamp = attr(e, "timestamp").and_then(|v| v.parse().ok());
    doc.total_size = attr(e, "total_size")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Ok(())
}

fn chunk_attrs(e: &BytesStart<'_>) -> Chunk {
    Chunk {
        id: attr(e, "id").and_then(|v| v.parse().ok()).unwrap_or(0),
        from: attr(e, "from").and_then(|v| v.parse().ok()).unwrap_or(0),
        to: attr(e, "to").and_then(|v| v.parse().ok()).unwrap_or(0),
        method: attr(e, "method").unwrap_or_else(|| "md5".into()),
        md5: String::new(),
    }
}

/// Non-fatal consistency checks → warnings (08 §2: count mismatch → warn field).
fn validate(doc: &mut ChecksumDoc) {
    for chunk in &doc.chunks {
        if !chunk.method.eq_ignore_ascii_case("md5") {
            doc.warnings.push(format!(
                "chunk {}: unknown method {}",
                chunk.id, chunk.method
            ));
        }
    }
    for pair in doc.chunks.windows(2) {
        if pair[1].from != pair[0].to + 1 {
            doc.warnings.push(format!(
                "chunk spans not contiguous: {}..{} then {}..{}",
                pair[0].from, pair[0].to, pair[1].from, pair[1].to
            ));
        }
    }
    if let Some(last) = doc.chunks.last() {
        let covered = last.to + 1;
        if doc.total_size != 0 && covered != doc.total_size {
            doc.warnings.push(format!(
                "chunk spans cover {covered} bytes, total_size says {}",
                doc.total_size
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<file name="setup_game_2.0.0.1_(12345).exe" available="true"
      md5="0f3e5a9b8c7d6e2f1a4b5c6d7e8f9012" timestamp="1760000000"
      total_size="20971520">
 <chunks>
  <chunk id="0" from="0" to="10485759" method="md5"> a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6 </chunk>
  <chunk id="1" from="10485760" to="20971519" method="md5">0f1e2d3c4b5a69788796a5b4c3d2e1f0</chunk>
 </chunks>
</file>"#;

    #[test]
    fn parses_basic_doc() {
        let doc = parse(SAMPLE).unwrap();
        assert_eq!(doc.name, "setup_game_2.0.0.1_(12345).exe");
        assert!(doc.available);
        assert_eq!(doc.notavailablemsg, None);
        assert_eq!(doc.timestamp, Some(1_760_000_000));
        assert_eq!(doc.total_size, 20_971_520);
        assert_eq!(doc.chunks.len(), 2);
        // Whitespace inside the text node is trimmed.
        assert_eq!(doc.chunks[0].md5, "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6");
        assert_eq!(doc.chunks[1].from, 10_485_760);
        assert_eq!(doc.chunks[1].to, 20_971_519);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    }

    #[test]
    fn attribute_order_is_irrelevant() {
        let doc = parse(
            r#"<file total_size="100" timestamp="99" md5="aa" name="x.bin">
 <chunks><chunk to="99" from="0" id="0" method="md5">bb</chunk></chunks>
</file>"#,
        )
        .unwrap();
        assert_eq!(doc.name, "x.bin");
        assert_eq!(doc.total_size, 100);
        assert_eq!(doc.chunks[0].md5, "bb");
    }

    #[test]
    fn bom_is_tolerated() {
        let doc = parse(&format!("\u{feff}{SAMPLE}")).unwrap();
        assert_eq!(doc.chunks.len(), 2);
    }

    #[test]
    fn missing_chunks_element_yields_empty() {
        let doc = parse(r#"<file name="whole.bin" total_size="42" available="true"/>"#).unwrap();
        assert!(doc.chunks.is_empty());
        assert!(doc.warnings.is_empty());
    }

    #[test]
    fn count_mismatch_becomes_warning() {
        let doc = parse(
            r#"<file name="x.bin" total_size="999">
 <chunks><chunk id="0" from="0" to="99" method="md5">aa</chunk></chunks>
</file>"#,
        )
        .unwrap();
        assert_eq!(doc.chunks.len(), 1);
        assert_eq!(doc.warnings.len(), 1);
        assert!(doc.warnings[0].contains("total_size"));
    }

    #[test]
    fn unavailable_doc_is_parsed_not_failed() {
        let doc = parse(
            r#"<file name="x.bin" available="false" notavailablemsg="File not available in your region"/>"#,
        )
        .unwrap();
        assert!(!doc.available);
        assert_eq!(
            doc.notavailablemsg.as_deref(),
            Some("File not available in your region")
        );
    }

    #[test]
    fn missing_root_name_is_fatal() {
        assert!(parse(r#"<file total_size="1"/>"#).is_err());
    }

    #[tokio::test]
    async fn fetch_parses_remote_doc() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(SAMPLE))
            .mount(&server)
            .await;
        let http = crate::gog::http().unwrap();
        let doc = fetch(&http, &server.uri()).await.unwrap();
        assert_eq!(doc.chunks.len(), 2);
    }

    #[tokio::test]
    async fn fetch_maps_404_to_not_found() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let http = crate::gog::http().unwrap();
        assert!(matches!(
            fetch(&http, &server.uri()).await,
            Err(ApiError::NotFound(_))
        ));
    }
}

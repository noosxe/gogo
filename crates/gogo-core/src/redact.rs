//! What gogo may show of a URL or of a library's error (design 07 §4).
//!
//! GOG puts secrets in URLs: the token endpoint takes the refresh token, the
//! client secret and the authorization code as query parameters, and CDN links
//! carry an access token in their first path segment (`/token=…~hmac=…/`).
//! reqwest's error text (and gog-warp's `Debug` and `source()`, which hand on
//! the reqwest error) includes the request URL. So:
//!
//! - **Construction time is the rule.** An error is turned into gogo's own text
//!   when it is made: [`describe_error`] for any error chain, [`redact_url`] for a
//!   URL, [`NetError`] in place of a stored `reqwest::Error`, [`SecretUrl`] for a
//!   URL that a struct must hold. Library error text is never formatted as-is.
//! - **[`scrub_text`] is a safety net** at the outermost sinks (the CLI's error
//!   printer, `--json` error fields, progress events, the log writer). It is not
//!   the mechanism: a pattern list is only as good as its patterns.

use std::error::Error as StdError;
use std::fmt;

use url::Url;

/// `u` as it may be shown or logged: scheme, host (and port), the first path
/// segment if it is a plain word, and how many segments follow. Userinfo, the
/// query and the fragment are dropped. A CDN link's first segment is its token,
/// so it is never kept: `https://cdn.example/[redacted 4 segments]`.
pub fn redact_url(u: &Url) -> String {
    let mut out = format!("{}://{}", u.scheme(), u.host_str().unwrap_or(""));
    if let Some(p) = u.port() {
        out.push_str(&format!(":{p}"));
    }
    let segs: Vec<&str> = u
        .path_segments()
        .map(|s| s.filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    let (kept, hidden) = match segs.split_first() {
        Some((first, rest)) if plain_segment(first) => (Some(*first), rest.len()),
        _ => (None, segs.len()),
    };
    if let Some(first) = kept {
        out.push('/');
        out.push_str(first);
    }
    if hidden > 0 {
        let noun = if hidden == 1 { "segment" } else { "segments" };
        out.push_str(&format!("/[redacted {hidden} {noun}]"));
    }
    out
}

/// A path segment that names a route (`v1`, `secure`, `token`), not a value.
fn plain_segment(s: &str) -> bool {
    (1..=16).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// What kind of failure a reqwest error is, in fixed words: its status, or
/// timeout, connect, redirect, body/decode. Never its text.
pub fn reqwest_kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "could not connect"
    } else if e.is_redirect() {
        "too many redirects"
    } else if e.is_decode() || e.is_body() {
        "the answer could not be read"
    } else {
        "request failed"
    }
}

/// One reqwest error, without its source: `HTTP 500 (https://host/[redacted …])`.
fn reqwest_head(e: &reqwest::Error) -> String {
    let mut out = match e.status() {
        Some(s) => format!("HTTP {}", s.as_u16()),
        None => reqwest_kind(e).to_string(),
    };
    if let Some(u) = e.url() {
        out.push_str(&format!(" ({})", redact_url(u)));
    }
    out
}

/// An error and its source chain in gogo's words (design 07 §4): each link is
/// shown only if its type is known to be safe, and then only in a safe form.
///
/// - `reqwest::Error`: kind or status, and the redacted URL.
/// - `gog_warp::Error`: its `Display`, a fixed phrase per kind.
/// - `std::io::Error`: an OS error's message; a custom one shows its kind and
///   goes on into the error it wraps.
/// - `serde_json::Error`: its category and position, never its message (which
///   can quote a value).
///
/// Any other link is skipped; if nothing known follows it, the text ends in
/// `[detail hidden]`.
pub fn describe_error(e: &(dyn StdError + 'static)) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut hidden = false;
    let mut link: Option<&(dyn StdError + 'static)> = Some(e);
    // A chain is a handful of links; the bound is only against a cycle.
    for _ in 0..16 {
        let Some(l) = link else { break };
        let (text, next) = describe_link(l);
        match text {
            Some(t) => {
                hidden = false;
                if parts.last() != Some(&t) {
                    parts.push(t);
                }
            }
            None => hidden = true,
        }
        link = next;
    }
    if hidden || parts.is_empty() {
        parts.push("[detail hidden]".into());
    }
    parts.join(": ")
}

type Link<'a> = Option<&'a (dyn StdError + 'static)>;

fn describe_link<'a>(l: &'a (dyn StdError + 'static)) -> (Option<String>, Link<'a>) {
    if let Some(r) = l.downcast_ref::<reqwest::Error>() {
        return (Some(reqwest_head(r)), r.source());
    }
    if let Some(w) = l.downcast_ref::<gog_warp::Error>() {
        return (Some(w.to_string()), w.source());
    }
    if let Some(io) = l.downcast_ref::<std::io::Error>() {
        return match io.get_ref() {
            // A custom io error's Display is its payload's: don't trust it.
            Some(inner) => (Some(io.kind().to_string()), Some(inner as _)),
            None => (Some(io.to_string()), None),
        };
    }
    if let Some(j) = l.downcast_ref::<serde_json::Error>() {
        return (Some(json_summary(j)), None);
    }
    (None, l.source())
}

/// A serde_json error without its message: `invalid data at line 1 column 7`.
pub fn json_summary(e: &serde_json::Error) -> String {
    use serde_json::error::Category;
    let what = match e.classify() {
        Category::Io => "io error",
        Category::Syntax => "invalid JSON",
        Category::Data => "invalid data",
        Category::Eof => "unexpected end of JSON",
    };
    if e.line() == 0 {
        what.to_string()
    } else {
        format!("{what} at line {} column {}", e.line(), e.column())
    }
}

/// A network error as gogo stores it: [`describe_error`]'s text and the facts
/// a caller branches on. It holds no `reqwest::Error`, so neither its `Display`
/// nor its `Debug` can show a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetError {
    what: String,
    status: Option<u16>,
    timeout: bool,
}

impl NetError {
    /// The HTTP status, when the server answered.
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    pub fn is_timeout(&self) -> bool {
        self.timeout
    }
}

impl From<reqwest::Error> for NetError {
    fn from(e: reqwest::Error) -> Self {
        NetError {
            what: describe_error(&e),
            status: e.status().map(|s| s.as_u16()),
            timeout: e.is_timeout(),
        }
    }
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.what)
    }
}

impl StdError for NetError {}

/// A URL that carries a secret (a CDN link with its token, a storage URL with
/// the user id). `Debug` and `Display` show only [`redact_url`]'s form.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretUrl(Url);

impl SecretUrl {
    pub fn new(u: Url) -> Self {
        SecretUrl(u)
    }

    /// The full URL, for building a request; never for output.
    pub fn expose(&self) -> &Url {
        &self.0
    }
}

impl fmt::Display for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redact_url(&self.0))
    }
}

impl fmt::Debug for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretUrl({self})")
    }
}

/// Query and form parameters whose values are secrets.
const SECRET_PARAMS: &[&str] = &[
    "refresh_token",
    "access_token",
    "id_token",
    "client_secret",
    "code",
    "token",
    "password",
    "session_id",
];

/// `text` with anything that looks like a credential removed: every `http(s)://`
/// URL replaced by its [`redact_url`] form, the values of [`SECRET_PARAMS`]
/// (`name=value`, `"name":"value"`) and of `Bearer …` replaced by `[redacted]`.
///
/// The safety net at output sinks (see the module doc); text built by gogo
/// should already be clean.
pub fn scrub_text(text: &str) -> String {
    scrub_params(&scrub_urls(text))
}

/// Where a URL (or a parameter value) ends in free text.
fn ends_token(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ']' | '}' | ',')
}

fn scrub_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = find_scheme(rest) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail.find(ends_token).unwrap_or(tail.len());
        let raw = &tail[..end];
        match Url::parse(raw) {
            Ok(u) => out.push_str(&redact_url(&u)),
            Err(_) => out.push_str("[redacted url]"),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// The byte offset of the next `http://` or `https://`, case-insensitively.
fn find_scheme(s: &str) -> Option<usize> {
    let lower = s.to_ascii_lowercase();
    [lower.find("https://"), lower.find("http://")]
        .into_iter()
        .flatten()
        .min()
}

fn scrub_params(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    // Value spans to replace, as (start, end) byte offsets.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let value_end = |from: usize, quoted: bool| -> usize {
        let t = &text[from..];
        let stop = if quoted {
            t.find(['"', '\''])
        } else {
            t.find(|c: char| ends_token(c) || matches!(c, '&' | '~' | '/' | ';'))
        };
        from + stop.unwrap_or(t.len())
    };
    for name in SECRET_PARAMS {
        let mut from = 0;
        while let Some(i) = lower[from..].find(name) {
            let start = from + i;
            let after = start + name.len();
            from = after;
            let boundary = start == 0
                || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            if !boundary {
                continue;
            }
            let t = &text[after..];
            if let Some(v) = t.strip_prefix('=') {
                // `name="value"` is how tracing's fmt shows a string field.
                let (v, quoted) = match v.strip_prefix(['"', '\'']) {
                    Some(q) => (q, true),
                    None => (v, false),
                };
                let vs = text.len() - v.len();
                spans.push((vs, value_end(vs, quoted)));
            } else if let Some(v) = strip_json_key(t) {
                let vs = text.len() - v.len();
                spans.push((vs, value_end(vs, true)));
            }
        }
    }
    let mut from = 0;
    while let Some(i) = lower[from..].find("bearer ") {
        let vs = from + i + "bearer ".len();
        spans.push((vs, value_end(vs, false)));
        from = vs;
    }
    spans.retain(|(s, e)| e > s);
    spans.sort();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (s, e) in spans {
        if s < pos {
            continue;
        }
        out.push_str(&text[pos..s]);
        out.push_str("[redacted]");
        pos = e;
    }
    out.push_str(&text[pos..]);
    out
}

/// After a key: `": "`, `":"` (JSON) or `: "` (Rust `Debug`), up to the
/// opening quote of a string value.
fn strip_json_key(t: &str) -> Option<&str> {
    let t = t.strip_prefix(['"', '\'']).unwrap_or(t);
    let t = t.trim_start().strip_prefix(':')?;
    let t = t.trim_start();
    t.strip_prefix(['"', '\''])
}

#[cfg(test)]
mod tests;

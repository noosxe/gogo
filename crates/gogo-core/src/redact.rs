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
/// segment if it is a plain route word (`[a-z0-9_-]`, at most 16 characters),
/// and how many segments follow. Userinfo, the
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

/// Query, form and CDN-token parameters whose values are secrets (GOG's CDN
/// token is `token=nva=…~dirs=…~hmac=…`, its Akamai form `acl=…~hmac=…`).
const SECRET_PARAMS: &[&str] = &[
    "refresh_token",
    "access_token",
    "id_token",
    "client_secret",
    "code",
    "token",
    "password",
    "session_id",
    "hmac",
    "nva",
    "acl",
];

/// Whether a log record from `target` at `level` may be written at all.
///
/// gog-warp logs the sign-in code and the start of an access token at debug
/// level, through the `log` bridge. Every log sink drops its records below warn,
/// whatever `-v`, `--log-file` or `RUST_LOG` ask for (design 07 §4).
pub fn log_record_allowed(target: &str, level: &tracing::Level) -> bool {
    let gog_warp = target == "gog_warp" || target.starts_with("gog_warp::");
    // A more verbose level compares greater.
    !gog_warp || *level <= tracing::Level::WARN
}

/// One log event's bytes, written out through [`scrub_text`] when dropped: what every
/// frontend's log writer hands each event to.
pub struct ScrubbedLine<W: std::io::Write> {
    buf: Vec<u8>,
    out: W,
}

impl<W: std::io::Write> ScrubbedLine<W> {
    pub fn new(out: W) -> Self {
        ScrubbedLine {
            buf: Vec::new(),
            out,
        }
    }
}

impl<W: std::io::Write> std::io::Write for ScrubbedLine<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<W: std::io::Write> Drop for ScrubbedLine<W> {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.buf);
        let _ = self.out.write_all(scrub_text(&text).as_bytes());
        let _ = self.out.flush();
    }
}

// The bare `code <value>` rule is a narrow backstop for gog-warp's one wording. It
// misses other forms (`code '…'`, a short value, a code with no digit or capital).
// What keeps gog-warp's secrets out of a log is the level filter,
// [`log_record_allowed`].

/// The shortest value taken as a sign-in code in `code <value>`.
const BARE_CODE_MIN: usize = 16;

/// The length of a sign-in code at the start of `t`, as in `code <value>`: a long
/// run of token characters with a digit or a capital in it, so that `exit code 1`
/// and `code signing` stay as they are.
fn bare_code_len(t: &str) -> Option<usize> {
    let end = t
        .find(|c: char| !(c.is_ascii_alphanumeric() || "-_.%".contains(c)))
        .unwrap_or(t.len());
    let value = t[..end].trim_end_matches('.');
    let looks_secret = value.len() >= BARE_CODE_MIN
        && value
            .bytes()
            .any(|b| b.is_ascii_digit() || b.is_ascii_uppercase());
    looks_secret.then_some(value.len())
}

/// `text` with anything that looks like a credential removed: every `http(s)://`
/// URL replaced by its [`redact_url`] form, and the values of [`SECRET_PARAMS`]
/// and of `Bearer …` replaced by `[redacted]`. A parameter matches as
/// `name=value`, `name%3Dvalue`, `"name": value` / `"name":"value"` (JSON, also
/// with escaped quotes) and `name: "value"` (Rust `Debug`). A sign-in code also
/// matches as `code <value>`, gog-warp's wording. ANSI escapes are
/// looked through; text that needed scrubbing comes back without them.
///
/// The safety net at output sinks (see the module doc); text built by gogo
/// should already be clean.
pub fn scrub_text(text: &str) -> String {
    let plain = strip_ansi(text);
    let out = scrub_params(&scrub_urls(&plain));
    if out == plain { text.to_string() } else { out }
}

/// `text` without ANSI CSI sequences (`ESC [ … final`), as styled logs carry.
fn strip_ansi(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for f in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&f) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Where a URL ends in free text.
fn ends_url(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '\\' | '<' | '>' | '`' | ')' | ']' | '}' | ','
        )
}

/// Where an unquoted secret value ends: never at `/` or `~`, which tokens and
/// CDN paths contain.
fn ends_value(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '\\' | '&' | ';' | '#' | ',' | ')' | ']' | '}' | '<' | '>' | '`'
        )
}

fn scrub_urls(text: &str) -> String {
    // Lowercased once: offsets match, as ASCII lowercasing keeps byte lengths.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    loop {
        let rest = &lower[pos..];
        let Some(at) = [rest.find("https://"), rest.find("http://")]
            .into_iter()
            .flatten()
            .min()
        else {
            break;
        };
        let start = pos + at;
        out.push_str(&text[pos..start]);
        let tail = &text[start..];
        let end = start + tail.find(ends_url).unwrap_or(tail.len());
        match Url::parse(&text[start..end]) {
            Ok(u) => out.push_str(&redact_url(&u)),
            Err(_) => out.push_str("[redacted url]"),
        }
        pos = end;
    }
    out.push_str(&text[pos..]);
    out
}

/// `spans` sorted, overlapping or touching ones merged into one.
fn merge_spans(mut spans: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    spans.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in spans {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// A quote as text may carry it: `"`, `'`, or JSON-escaped `\"`.
fn strip_quote(t: &str) -> Option<&str> {
    t.strip_prefix("\\\"")
        .or_else(|| t.strip_prefix(['"', '\'']))
}

/// The value span after a parameter name ends at `after`, if a separator and a
/// value follow.
fn value_span(text: &str, after: usize) -> Option<(usize, usize)> {
    let t = &text[after..];
    let (key_quoted, t) = match strip_quote(t) {
        Some(r) => (true, r),
        None => (false, t),
    };
    let t = if let Some(r) = t.strip_prefix('=') {
        r
    } else if t.len() >= 3 && t[..3].eq_ignore_ascii_case("%3d") {
        &t[3..]
    } else {
        let r = t.trim_start().strip_prefix(':')?.trim_start();
        // `name: 3` is Debug of an ordinary field; only a quoted key or a
        // quoted value says this is a parameter.
        if !key_quoted && strip_quote(r).is_none() {
            return None;
        }
        r
    };
    let (quoted, v) = match strip_quote(t) {
        Some(r) => (true, r),
        None => (false, t),
    };
    let start = text.len() - v.len();
    let len = if quoted {
        v.find(['"', '\'', '\\'])
    } else {
        v.find(ends_value)
    }
    .unwrap_or(v.len());
    (len > 0).then_some((start, start + len))
}

fn scrub_params(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for name in SECRET_PARAMS {
        let mut from = 0;
        while let Some(i) = lower[from..].find(name) {
            let start = from + i;
            let after = start + name.len();
            from = after;
            let prev = start.checked_sub(1).map(|p| bytes[p]);
            if prev.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_') {
                continue;
            }
            spans.extend(value_span(text, after));
        }
    }
    let mut from = 0;
    while let Some(i) = lower[from..].find("bearer ") {
        let vs = from + i + "bearer ".len();
        let len = text[vs..].find(ends_value).unwrap_or(text.len() - vs);
        if len > 0 {
            spans.push((vs, vs + len));
        }
        from = vs;
    }
    let mut from = 0;
    while let Some(i) = lower[from..].find("code ") {
        let start = from + i;
        let vs = start + "code ".len();
        from = vs;
        let prev = start.checked_sub(1).map(|p| bytes[p]);
        if prev.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        if let Some(len) = bare_code_len(&text[vs..]) {
            spans.push((vs, vs + len));
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (s, e) in merge_spans(spans) {
        out.push_str(&text[pos..s]);
        out.push_str("[redacted]");
        pos = e;
    }
    out.push_str(&text[pos..]);
    out
}

#[cfg(test)]
mod tests;

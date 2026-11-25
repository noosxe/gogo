//! KDL v2 text → our own node tree, with spans (design 27 §9 step 1 and 2).
//!
//! The `kdl` crate's types stop here: nothing public mentions them. A [`RawNode`] is a
//! name, arguments, properties and children, each with the file, byte offset and length
//! it came from. The loader reads the manifest and the tokens from it directly; styles,
//! components and routes go on to the typed tree in [`crate::ast`].

use kdl::{KdlDocument, KdlNode, KdlValue};

use crate::diag::{Code, Collector};
use crate::limits::{MAX_DEPTH, MAX_NODES, PARSER_GUARD_DEPTH};
use crate::source::Span;

/// A literal from the file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Lit {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
}

impl Lit {
    pub fn kind(&self) -> &'static str {
        match self {
            Lit::Str(_) => "a string",
            Lit::Num(_) => "a number",
            Lit::Bool(_) => "a boolean",
            Lit::Null => "null",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RawArg {
    pub lit: Lit,
    /// Where the value is written (for `key=value`, the value alone).
    pub span: Span,
    /// The string's content starts this many bytes into `span`, when the text of `span`
    /// is the string as written (no escapes): lets an expression error point inside it.
    pub inner: Option<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawProp {
    pub name: String,
    pub name_span: Span,
    pub value: RawArg,
}

#[derive(Debug, Clone)]
pub(crate) struct RawNode {
    pub name: String,
    pub name_span: Span,
    pub args: Vec<RawArg>,
    pub props: Vec<RawProp>,
    pub children: Vec<RawNode>,
    pub span: Span,
}

impl RawNode {
    pub fn str_arg(&self, i: usize) -> Option<&str> {
        match self.args.get(i).map(|a| &a.lit) {
            Some(Lit::Str(s)) => Some(s),
            _ => None,
        }
    }

    pub fn prop(&self, name: &str) -> Option<&RawProp> {
        self.props.iter().rev().find(|p| p.name == name)
    }
}

/// The running totals of one load: S12 counts nodes over the whole skin.
#[derive(Debug, Default)]
pub(crate) struct Counter {
    pub nodes: usize,
    pub tripped: bool,
}

/// The deepest `{` nesting of a file, read naively: strings and comments are skipped, and
/// a raw or multi-line string is skipped as well as it can be. Only a guard for the
/// parser's stack; the real limit is counted on the parsed tree.
fn naive_depth(text: &str) -> usize {
    let b = text.as_bytes();
    let (mut i, mut depth, mut max) = (0, 0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut nest = 1;
                i += 2;
                while i < b.len() && nest > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        nest += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        nest -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            b'#' | b'"' => {
                let hashes = b[i..].iter().take_while(|c| **c == b'#').count();
                let q = i + hashes;
                if b.get(q) == Some(&b'"') {
                    let multi = b.get(q + 1) == Some(&b'"') && b.get(q + 2) == Some(&b'"');
                    let quotes = if multi { 3 } else { 1 };
                    i = q + quotes;
                    while i < b.len() {
                        if !multi && hashes == 0 && b[i] == b'\\' {
                            i += 2;
                            continue;
                        }
                        if b[i] == b'"'
                            && b[i..].iter().take_while(|c| **c == b'"').count() >= quotes
                            && b[i + quotes..].iter().take_while(|c| **c == b'#').count() >= hashes
                        {
                            i += quotes + hashes;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                i += hashes.max(1);
                continue;
            }
            b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    max
}

/// Parse file `file` of the collector's sources. Reports the KDL syntax error (or the
/// limit that was hit) and returns `None`.
pub(crate) fn parse_file(
    col: &mut Collector,
    file: usize,
    counter: &mut Counter,
) -> Option<Vec<RawNode>> {
    let text = col.sources.get(file)?.text.clone();
    if naive_depth(&text) > PARSER_GUARD_DEPTH {
        col.skin_error(
            Code::LimitDepth,
            Span::new(file, 0, 0),
            format!("nested deeper than {MAX_DEPTH} levels (S12)"),
        )
        .help("flatten the tree: move parts into components");
        return None;
    }
    let doc = match KdlDocument::parse_v2(&text) {
        Ok(d) => d,
        Err(e) => {
            for d in e.diagnostics.iter().take(3) {
                let at = clamp(&text, d.span.offset(), d.span.len());
                let msg = d
                    .message
                    .clone()
                    .unwrap_or_else(|| "invalid KDL (v2)".to_owned());
                let f = col.error(Code::KdlSyntax, Span::new(file, at.0, at.1), msg);
                let f = match &d.label {
                    Some(l) => f.label(l.clone()),
                    None => f,
                };
                if let Some(h) = &d.help {
                    f.help(h.clone());
                }
            }
            return None;
        }
    };
    let mut out = Vec::new();
    for n in doc.nodes() {
        out.push(convert(col, n, file, &text, 1, counter)?);
    }
    Some(out)
}

/// Offset and length made safe for slicing `text`.
fn clamp(text: &str, offset: usize, len: usize) -> (usize, usize) {
    let mut s = offset.min(text.len());
    while !text.is_char_boundary(s) {
        s -= 1;
    }
    let mut e = offset.saturating_add(len).min(text.len());
    while !text.is_char_boundary(e) {
        e -= 1;
    }
    (s, e.saturating_sub(s))
}

fn span_of(text: &str, file: usize, s: miette::SourceSpan) -> Span {
    let (o, l) = clamp(text, s.offset(), s.len());
    Span::new(file, o, l)
}

fn convert(
    col: &mut Collector,
    n: &KdlNode,
    file: usize,
    text: &str,
    depth: usize,
    counter: &mut Counter,
) -> Option<RawNode> {
    let span = span_of(text, file, n.span());
    counter.nodes += 1;
    if counter.nodes > MAX_NODES {
        if !counter.tripped {
            counter.tripped = true;
            col.skin_error(
                Code::LimitNodes,
                span,
                format!("more than {MAX_NODES} nodes in the skin (S12)"),
            )
            .help("split the skin's content: components don't count their uses");
        }
        return None;
    }
    if depth > MAX_DEPTH {
        col.skin_error(
            Code::LimitDepth,
            span,
            format!("nested deeper than {MAX_DEPTH} levels (S12)"),
        )
        .help("flatten the tree: move parts into components");
        return None;
    }
    if n.ty().is_some() {
        col.error(
            Code::KdlSyntax,
            span,
            "type annotations are not used in skins",
        );
    }
    let mut node = RawNode {
        name: n.name().value().to_owned(),
        name_span: span_of(text, file, n.name().span()),
        args: Vec::new(),
        props: Vec::new(),
        children: Vec::new(),
        span,
    };
    for e in n.entries() {
        let espan = span_of(text, file, e.span());
        if e.ty().is_some() {
            col.error(
                Code::KdlSyntax,
                espan,
                "type annotations are not used in skins",
            );
        }
        let lit = match e.value() {
            KdlValue::String(s) => Lit::Str(s.clone()),
            KdlValue::Integer(i) => Lit::Num(*i as f64),
            KdlValue::Float(f) => Lit::Num(*f),
            KdlValue::Bool(b) => Lit::Bool(*b),
            KdlValue::Null => Lit::Null,
        };
        match e.name() {
            Some(key) => {
                let name_span = span_of(text, file, key.span());
                let vspan = value_span(text, espan, name_span);
                let inner = inner_offset(text, vspan, &lit);
                node.props.push(RawProp {
                    name: key.value().to_owned(),
                    name_span,
                    value: RawArg {
                        lit,
                        span: vspan,
                        inner,
                    },
                });
            }
            None => {
                let inner = inner_offset(text, espan, &lit);
                node.args.push(RawArg {
                    lit,
                    span: espan,
                    inner,
                });
            }
        }
    }
    if let Some(doc) = n.children() {
        for c in doc.nodes() {
            node.children
                .push(convert(col, c, file, text, depth + 1, counter)?);
        }
    }
    Some(node)
}

/// The value of `key=value`: what follows the `=` after the key.
fn value_span(text: &str, entry: Span, name: Span) -> Span {
    let end = entry.offset + entry.len;
    let key_end = (name.offset + name.len).min(end);
    let Some(rest) = text.get(key_end..end) else {
        return entry;
    };
    let Some(eq) = rest.find('=') else {
        return entry;
    };
    let after = &rest[eq + 1..];
    let ws = after.len() - after.trim_start().len();
    let start = key_end + eq + 1 + ws;
    Span::new(entry.file, start, end - start)
}

/// How many bytes into `span` the content of a string literal starts, if the text of the
/// span is exactly the string with its delimiters: `"x"` → 1, `x` → 0, `#"x"#` → 2.
fn inner_offset(text: &str, span: Span, lit: &Lit) -> Option<usize> {
    let Lit::Str(value) = lit else {
        return None;
    };
    let raw = text.get(span.offset..span.offset + span.len)?;
    // The opening and closing delimiters are the same length: `"`, `#"`, `##"`, or none.
    for lead in [1usize, 0, 2, 3] {
        let want = 2 * lead + value.len();
        if raw.len() == want && raw.get(lead..lead + value.len()) == Some(value.as_str()) {
            let delim_ok = match lead {
                0 => true,
                1 => raw.starts_with('"') && raw.ends_with('"'),
                n => {
                    let hashes = n - 1;
                    raw.starts_with(&"#".repeat(hashes)) && raw.ends_with(&"#".repeat(hashes))
                }
            };
            if delim_ok {
                return Some(lead);
            }
        }
    }
    None
}

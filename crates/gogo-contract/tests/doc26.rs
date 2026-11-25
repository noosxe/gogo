//! Every row of design 26 exists in `gogo-contract`, and the code has no row the doc
//! lacks: sources (§3), actions (§4), app-owned widgets (§5), tokens (§6.1), routes
//! (§6.2), the value types (§2), the statuses and the invariants (§7).
//!
//! The doc's Markdown tables are parsed here. The parser is deliberately small: a row is
//! a line starting with `|`, cells split on `|` not preceded by `\`, and names are read
//! from backtick spans.

use gogo_contract::actions::{ACTIONS, ParamType};
use gogo_contract::routes::ROUTES;
use gogo_contract::sources::SOURCES;
use gogo_contract::tokens::{COLOURS, FONT_ROLES, MOTION, SHAPE};
use gogo_contract::widgets::{Depth, Placement, WIDGETS};
use gogo_contract::{Invariant, Status, ValueType};
use std::collections::{BTreeMap, BTreeSet};

fn doc() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/design/26-skin-contract.md"
    );
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The lines of the section whose heading starts with `heading`, up to the next heading
/// of the same or a higher level.
fn section<'a>(doc: &'a str, heading: &str) -> Vec<&'a str> {
    let level = heading.chars().take_while(|c| *c == '#').count();
    let mut out = Vec::new();
    let mut inside = false;
    for line in doc.lines() {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && line[hashes..].starts_with(' ') {
            if inside && hashes <= level {
                break;
            }
            if line.starts_with(heading) {
                inside = true;
                continue;
            }
        }
        if inside {
            out.push(line);
        }
    }
    assert!(!out.is_empty(), "no section {heading}");
    out
}

/// The body rows of every table in `lines`, as cells (header and separator skipped).
fn rows(lines: &[&str]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in lines {
        if !line.starts_with('|') {
            in_table = false;
            continue;
        }
        if !in_table {
            in_table = true; // the header row
            continue;
        }
        if line.starts_with("|---") {
            continue;
        }
        out.push(cells(line));
    }
    out
}

fn cells(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cur).trim().to_owned()),
            _ => cur.push(c),
        }
    }
    cells.push(cur.trim().to_owned());
    cells
}

/// The backtick spans in `s`.
fn spans(s: &str) -> Vec<String> {
    s.split('`').skip(1).step_by(2).map(str::to_owned).collect()
}

/// `s` without `**` and backticks.
fn plain(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

/// Splits a parameter list on commas outside brackets.
fn split_top(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in s.chars() {
        match c {
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur).trim().to_owned());
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out
}

/// A parameter as the doc writes it.
#[derive(Debug, PartialEq, Eq)]
enum DocParam {
    /// `name`, `name?`, `name: type`, `name{…}`: the name, whether it is optional, and
    /// the alternatives when the type is a list of plain words (`which: game|prefix`).
    Named(String, bool, Option<Vec<String>>),
    /// Written by its values alone (`on|off|default`).
    Positional(String),
}

fn doc_params(code: &str) -> Vec<DocParam> {
    split_top(code)
        .into_iter()
        .map(|piece| {
            if let Some((name, ty)) = piece.split_once(':') {
                let ty = ty.trim();
                let optional = ty.ends_with('?');
                let words: Vec<String> = ty
                    .trim_end_matches('?')
                    .split('|')
                    .map(str::to_owned)
                    .collect();
                let plain_words = words.len() > 1
                    && words
                        .iter()
                        .all(|w| w.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
                DocParam::Named(
                    name.trim().to_owned(),
                    optional,
                    plain_words.then_some(words),
                )
            } else if let Some((name, _)) = piece.split_once('{') {
                DocParam::Named(name.trim().to_owned(), false, None)
            } else if piece.contains('|') {
                DocParam::Positional(piece)
            } else {
                let optional = piece.ends_with('?');
                DocParam::Named(piece.trim_end_matches('?').to_owned(), optional, None)
            }
        })
        .collect()
}

/// A name with its parameters: `library(filter?, sort?)` → (`library`, the params).
fn call(span: &str) -> (String, Vec<DocParam>) {
    match span.split_once('(') {
        Some((name, rest)) => (name.to_owned(), doc_params(rest.trim_end_matches(')'))),
        None => (span.to_owned(), Vec::new()),
    }
}

fn names_differ(what: &str, doc: BTreeSet<String>, code: BTreeSet<String>) {
    let missing: Vec<_> = doc.difference(&code).collect();
    let extra: Vec<_> = code.difference(&doc).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{what}: in the doc, not in code: {missing:?}; in code, not in the doc: {extra:?}"
    );
}

#[test]
fn sources_match_section_3() {
    let doc = doc();
    let mut seen = BTreeSet::new();
    let mut prev_from: Vec<String> = Vec::new();
    for row in rows(&section(&doc, "## 3. Data sources")) {
        let names = spans(&row[0]);
        let live = plain(&row[2]);
        let from = if row[3] == "same" {
            prev_from.clone()
        } else {
            spans(&row[3])
        };
        prev_from = from.clone();
        let mut code_from = Vec::new();
        for span in &names {
            let (name, params) = call(span);
            let spec = SOURCES
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("source {name} is in the doc, not in code"));
            let code_params: Vec<DocParam> = spec
                .params
                .iter()
                .map(|p| DocParam::Named(p.name.to_owned(), p.optional, None))
                .collect();
            assert_eq!(params, code_params, "source {name}: params");
            let doc_live = spec.live.doc();
            assert!(
                live == doc_live || live.starts_with(&format!("{doc_live} (")),
                "source {name}: doc says `{live}`, code says `{doc_live}`"
            );
            code_from.extend(spec.from.iter().map(|s| (*s).to_owned()));
            seen.insert(name);
        }
        assert_eq!(from, code_from, "sources {names:?}: From column");
    }
    names_differ(
        "sources",
        seen,
        SOURCES.iter().map(|s| s.name.to_owned()).collect(),
    );
}

#[test]
fn actions_match_section_4() {
    let doc = doc();
    let mut seen = BTreeSet::new();
    for row in rows(&section(&doc, "## 4. Actions")) {
        let names = spans(&row[0]);
        let params = if row[1] == "—" {
            Some(Vec::new())
        } else {
            spans(&row[1]).first().map(|s| doc_params(s))
        };
        let kind_tier = plain(&row[2]);
        for name in names {
            let spec = ACTIONS
                .iter()
                .find(|a| a.name == name)
                .unwrap_or_else(|| panic!("action {name} is in the doc, not in code"));
            // `clipboard.copy` takes "a bound non-secret field", in words.
            match &params {
                Some(params) => {
                    let code: Vec<DocParam> = spec
                        .params
                        .iter()
                        .map(|p| match (p.positional, p.ty) {
                            (true, ParamType::OneOf(values)) => {
                                DocParam::Positional(values.join("|"))
                            }
                            (true, ParamType::UnusedOrCatalogId) => {
                                DocParam::Positional("unused|id".into())
                            }
                            (true, other) => panic!("{name}: positional {other:?} has no doc form"),
                            (false, ty) => {
                                let words = match ty {
                                    ParamType::OneOf(values) => {
                                        Some(values.iter().map(|v| (*v).to_owned()).collect())
                                    }
                                    _ => None,
                                };
                                DocParam::Named(p.name.to_owned(), p.optional, words)
                            }
                        })
                        .collect();
                    // The doc names only the alternatives it spells out as words.
                    let code: Vec<DocParam> = code
                        .into_iter()
                        .zip(params.iter())
                        .map(|(c, d)| match (c, d) {
                            (DocParam::Named(n, o, _), DocParam::Named(_, _, None)) => {
                                DocParam::Named(n, o, None)
                            }
                            (c, _) => c,
                        })
                        .chain(
                            spec.params
                                .iter()
                                .skip(params.len())
                                .map(|p| DocParam::Named(p.name.into(), p.optional, None)),
                        )
                        .collect();
                    assert_eq!(params, &code, "action {name}: params");
                }
                None => assert_eq!(name, "clipboard.copy", "action {name}: params in words"),
            }
            let code = spec.doc_kind_tier();
            let rest = kind_tier.strip_prefix(&code).unwrap_or_else(|| {
                panic!("action {name}: doc says `{kind_tier}`, code says `{code}`")
            });
            // After the kind and tier the doc may add a note, never another rule.
            assert!(
                rest.is_empty()
                    || rest.starts_with(" (")
                    || rest.starts_with(": ")
                    || rest.starts_with(", only"),
                "action {name}: unexpected `{rest}` after `{code}`"
            );
            seen.insert(name);
        }
    }
    names_differ(
        "actions",
        seen,
        ACTIONS.iter().map(|a| a.name.to_owned()).collect(),
    );
}

#[test]
fn skin_settable_keys_match_section_4_9() {
    let doc = doc();
    let lines = section(&doc, "### 4.9 Queue and machine");
    let start = lines
        .iter()
        .position(|l| l.starts_with("**Skin-settable settings.**"))
        .expect("allow-list");
    let end = lines[start..]
        .iter()
        .position(|l| l.starts_with("A setting is **app-only**"))
        .expect("end")
        + start;
    let keys: BTreeSet<String> = lines[start + 1..end]
        .iter()
        .filter(|l| l.starts_with("- ") || l.starts_with("  "))
        .flat_map(|l| spans(l))
        // Keys, not the action the doc mentions beside one.
        .filter(|s| s.contains('.') && gogo_contract::actions::action(s).is_none())
        .collect();
    names_differ(
        "skin-settable keys",
        keys,
        gogo_contract::settings::SKIN_SETTABLE
            .iter()
            .map(|k| (*k).to_owned())
            .collect(),
    );
}

#[test]
fn widgets_match_section_5() {
    let doc = doc();
    let lines = section(&doc, "## 5. App-owned widgets");
    let mut seen = BTreeSet::new();
    for row in rows(&lines).into_iter().filter(|r| r.len() == 4) {
        let depth = match row[1].trim_start_matches("**").chars().next() {
            Some('A') => Depth::A,
            Some('B') => Depth::B,
            Some('C') => Depth::C,
            _ => continue, // the depth table
        };
        let mut parts: Vec<(String, bool)> = Vec::new();
        let mut variants: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut placements = Vec::new();
        for clause in row[2].split("; ") {
            let clause = clause.trim();
            if let Some(rest) = clause.strip_prefix("parts ") {
                for span in spans(rest) {
                    for part in span.split(", ") {
                        let optional = part.ends_with('*');
                        parts.push((part.trim_end_matches('*').to_owned(), optional));
                    }
                }
            } else if clause.starts_with("variant") {
                for span in spans(clause) {
                    let (name, values) = span
                        .split_once(": ")
                        .unwrap_or_else(|| panic!("unnamed variant {span}"));
                    variants.insert(
                        name.to_owned(),
                        values.split('|').map(str::to_owned).collect(),
                    );
                }
            } else if clause.starts_with("placement") {
                for span in spans(clause) {
                    placements.push(match span.split('(').next() {
                        Some("inline") => Placement::Inline,
                        Some("modal") => Placement::Modal,
                        other => panic!("placement {other:?}"),
                    });
                }
            }
        }
        for name in spans(&row[0]) {
            let w = WIDGETS
                .iter()
                .find(|w| w.name == name)
                .unwrap_or_else(|| panic!("widget {name} is in the doc, not in code"));
            assert_eq!(w.depth, depth, "widget {name}: depth");
            let code_parts: Vec<(String, bool)> = w
                .parts
                .iter()
                .map(|p| (p.name.to_owned(), p.optional))
                .collect();
            assert_eq!(parts, code_parts, "widget {name}: parts");
            let code_variants: BTreeMap<String, Vec<String>> = w
                .variants
                .iter()
                .map(|v| {
                    (
                        v.name.to_owned(),
                        v.values.iter().map(|s| (*s).to_owned()).collect(),
                    )
                })
                .collect();
            assert_eq!(variants, code_variants, "widget {name}: variants");
            assert_eq!(placements, w.placements, "widget {name}: placements");
            seen.insert(name);
        }
    }
    names_differ(
        "widgets",
        seen,
        WIDGETS.iter().map(|w| w.name.to_owned()).collect(),
    );
}

/// Expands the doc's shorthand: `radius.sm/md/lg`, `space.1…8`, `motion.fast/normal/slow`.
fn expand(token: &str) -> Vec<String> {
    let token = token.trim_end_matches(',').trim_end_matches('.');
    let Some((stem, tail)) = token.rsplit_once('.') else {
        return vec![token.to_owned()];
    };
    if let Some((a, b)) = tail.split_once('…') {
        let (a, b): (u32, u32) = (
            a.parse().expect("range start"),
            b.parse().expect("range end"),
        );
        return (a..=b).map(|n| format!("{stem}.{n}")).collect();
    }
    tail.split('/').map(|t| format!("{stem}.{t}")).collect()
}

#[test]
fn tokens_match_section_6_1() {
    let doc = doc();
    let lines = section(&doc, "### 6.1 Semantic tokens");
    let mut bullets: BTreeMap<String, String> = BTreeMap::new();
    let mut current = None;
    for line in lines {
        if let Some(rest) = line.strip_prefix("- **") {
            let (label, body) = rest.split_once(":**").expect("bullet label");
            current = Some(label.to_owned());
            bullets.insert(label.to_owned(), body.to_owned());
        } else if let (Some(label), true) = (&current, line.starts_with("  ")) {
            if let Some(body) = bullets.get_mut(label) {
                body.push(' ');
                body.push_str(line.trim());
            }
        } else {
            current = None;
        }
    }
    let words = |label: &str| -> BTreeSet<String> {
        let body = bullets
            .get(label)
            .unwrap_or_else(|| panic!("no {label} bullet"));
        spans(body)
            .iter()
            .flat_map(|s| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
            .filter(|t| t != "easing.*")
            .flat_map(|t| expand(&t))
            .collect()
    };
    let set = |xs: &[&str]| xs.iter().map(|x| (*x).to_owned()).collect::<BTreeSet<_>>();
    names_differ("colour tokens", words("Colours"), set(COLOURS));
    names_differ("type roles", words("Type roles"), set(FONT_ROLES));
    names_differ("shape tokens", words("Shape"), set(SHAPE));
    names_differ("motion tokens", words("Motion"), set(MOTION));
    assert!(
        bullets["Motion"].contains("`easing.*`"),
        "easing.* is open-ended in code"
    );
}

#[test]
fn routes_match_section_6_2() {
    let doc = doc();
    let mut seen = BTreeSet::new();
    for row in rows(&section(&doc, "### 6.2 Routes")) {
        let name = spans(&row[0]).remove(0);
        let r = ROUTES
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("route {name} is in the doc, not in code"));
        let params = if row[1] == "—" {
            Vec::new()
        } else {
            doc_params(&spans(&row[1])[0])
        };
        let code: Vec<DocParam> = r
            .params
            .iter()
            .map(|p| DocParam::Named(p.name.into(), p.optional, None))
            .collect();
        assert_eq!(params, code, "route {name}: params");
        // Only a cell made of code spans is a list of sections; prose describes the route.
        let sections: Vec<String> = if row[2].starts_with('`') {
            spans(&row[2])
                .iter()
                .flat_map(|s| s.split(", ").map(str::to_owned).collect::<Vec<_>>())
                .collect()
        } else {
            Vec::new()
        };
        assert_eq!(sections, r.sections, "route {name}: sections");
        seen.insert(name);
    }
    names_differ(
        "routes",
        seen,
        ROUTES.iter().map(|r| r.name.to_owned()).collect(),
    );
}

#[test]
fn types_and_statuses_match_section_2() {
    let doc = doc();
    let mut seen = BTreeSet::new();
    for row in rows(&section(&doc, "## 2. Types")) {
        let name = spans(&row[0]).remove(0);
        let t = ValueType::ALL
            .iter()
            .find(|t| t.name() == name)
            .unwrap_or_else(|| panic!("type {name} is in the doc, not in code"));
        let helpers: Vec<String> = spans(&row[2])
            .into_iter()
            .map(|h| h.split(" →").next().unwrap_or(&h).to_owned())
            .collect();
        // Art's cell names the elements that use it, not helpers.
        if name != "Art" {
            assert_eq!(helpers, t.helpers(), "type {name}: helpers");
        }
        if name == "Status" {
            let words: Vec<String> = spans(&row[1])[0].split(" · ").map(str::to_owned).collect();
            let code: Vec<String> = Status::ALL.iter().map(|s| s.name().to_owned()).collect();
            assert_eq!(words, code, "Status values");
        }
        seen.insert(name);
    }
    names_differ(
        "types",
        seen,
        ValueType::ALL.iter().map(|t| t.name().to_owned()).collect(),
    );
}

#[test]
fn invariants_match_section_7() {
    let doc = doc();
    let mut found = Vec::new();
    for line in section(&doc, "## 7. Safety invariants") {
        let Some(rest) = line.strip_prefix("- **S") else {
            continue;
        };
        let (num, rest) = rest.split_once(". ").expect("S<n>. title");
        let title = rest
            .split_once("**")
            .expect("closing **")
            .0
            .trim_end_matches('.');
        found.push((format!("S{num}"), title.to_owned()));
    }
    let code: Vec<(String, String)> = Invariant::ALL
        .iter()
        .map(|i| (i.id().to_owned(), i.title().to_owned()))
        .collect();
    assert_eq!(found, code);
}

#[test]
fn version_matches_the_status_line() {
    let doc = doc();
    let v = gogo_contract::ContractVersion::CURRENT;
    let label = if gogo_contract::ContractVersion::DRAFT {
        format!("**Contract version {v} (draft)**")
    } else {
        format!("**Contract version {v}**")
    };
    assert!(doc.contains(&label), "doc lacks {label}");
}

//! Every row of design 26 exists in `gogo-contract`, in the doc's order, and the code has
//! no row the doc lacks: the value types, statuses and OSes (§2), sources and the fields
//! of their shapes (§3), actions and their parameter types (§4), the settings lists
//! (§4.9), app-owned widgets (§5), tokens (§6.1), routes (§6.2) and the invariants (§7).
//!
//! The doc's Markdown is parsed here. The parser is deliberately small, and strict: a
//! table is a header line followed by a `|---` separator, every row has the header's
//! number of cells, and anything it doesn't recognise fails the test instead of being
//! skipped.

use gogo_contract::actions::{ACTIONS, ParamType};
use gogo_contract::routes::ROUTES;
use gogo_contract::shapes::{self, Field, FieldType};
use gogo_contract::sources::SOURCES;
use gogo_contract::tokens::{COLOURS, FONT_FIELDS, FONT_ROLES, MOTION, SHAPE};
use gogo_contract::widgets::{Depth, Placement, WIDGETS};
use gogo_contract::{Invariant, Os, Status, ValueType};
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

struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

/// The tables in `lines`. A header is a `|` line whose next line is a `|---` separator;
/// any other `|` line must belong to a table, and have the header's number of cells.
fn tables(lines: &[&str]) -> Vec<Table> {
    let mut out: Vec<Table> = Vec::new();
    let mut in_table = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if !line.starts_with('|') {
            in_table = false;
            i += 1;
            continue;
        }
        if !in_table {
            let sep = lines.get(i + 1).copied().unwrap_or("");
            assert!(
                sep.starts_with("|---"),
                "a table row without a header: {line}"
            );
            let header = cells(line);
            assert_eq!(cells(sep).len(), header.len(), "separator of {line}");
            out.push(Table {
                header,
                rows: Vec::new(),
            });
            in_table = true;
            i += 2;
            continue;
        }
        let row = cells(line);
        let table = out.last_mut().expect("a table");
        assert_eq!(row.len(), table.header.len(), "cell count of {line}");
        table.rows.push(row);
        i += 1;
    }
    out
}

/// The body rows of every table in `lines`.
fn rows(lines: &[&str]) -> Vec<Vec<String>> {
    tables(lines).into_iter().flat_map(|t| t.rows).collect()
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

/// A parameter as the doc writes it: `name`, `name?`, `name: type`, `name{…}`, or by its
/// values alone (`on|off|default`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DocParam {
    name: Option<String>,
    optional: bool,
    /// The written type: after `:`, the `{…}` after a name, or the positional values.
    ty: Option<String>,
}

fn doc_params(code: &str) -> Vec<DocParam> {
    split_top(code)
        .into_iter()
        .map(|piece| {
            if let Some((name, ty)) = piece.split_once(':') {
                let ty = ty.trim();
                DocParam {
                    name: Some(name.trim().to_owned()),
                    optional: ty.ends_with('?'),
                    ty: Some(ty.trim_end_matches('?').to_owned()),
                }
            } else if let Some((name, rest)) = piece.split_once('{') {
                DocParam {
                    name: Some(name.trim().to_owned()),
                    optional: false,
                    ty: Some(format!("{{{rest}")),
                }
            } else if piece.contains('|') {
                DocParam {
                    name: None,
                    optional: false,
                    ty: Some(piece),
                }
            } else {
                DocParam {
                    name: Some(piece.trim_end_matches('?').to_owned()),
                    optional: piece.ends_with('?'),
                    ty: None,
                }
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

/// `(name, optional)` of a source's or route's params, from the doc.
fn names_only(params: &[DocParam]) -> Vec<(String, bool)> {
    params
        .iter()
        .map(|p| {
            assert!(p.ty.is_none(), "a typed source or route param: {p:?}");
            (p.name.clone().expect("named param"), p.optional)
        })
        .collect()
}

/// The doc's names, in order, equal the code's: same set, then same order.
fn same_list(what: &str, doc: &[String], code: &[&str]) {
    let doc_set: BTreeSet<&str> = doc.iter().map(String::as_str).collect();
    let code_set: BTreeSet<&str> = code.iter().copied().collect();
    let missing: Vec<_> = doc_set.difference(&code_set).collect();
    let extra: Vec<_> = code_set.difference(&doc_set).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{what}: in the doc, not in code: {missing:?}; in code, not in the doc: {extra:?}"
    );
    assert_eq!(doc.len(), doc_set.len(), "{what}: the doc names one twice");
    let code_order: Vec<&str> = code.to_vec();
    let doc_order: Vec<&str> = doc.iter().map(String::as_str).collect();
    assert_eq!(doc_order, code_order, "{what}: order");
}

fn same_set(what: &str, doc: BTreeSet<String>, code: &[&str]) {
    let doc: Vec<String> = doc.into_iter().collect();
    let mut code: Vec<&str> = code.to_vec();
    code.sort_unstable();
    same_list(what, &doc, &code);
}

#[test]
fn sources_match_section_3() {
    let doc = doc();
    let mut order = Vec::new();
    let mut prev_from: Vec<String> = Vec::new();
    for row in rows(&section(&doc, "## 3. Data sources")) {
        let names = spans(&row[0]);
        assert!(!names.is_empty(), "a source row without a name: {row:?}");
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
            let code_params: Vec<(String, bool)> = spec
                .params
                .iter()
                .map(|p| (p.name.to_owned(), p.optional))
                .collect();
            assert_eq!(names_only(&params), code_params, "source {name}: params");
            let doc_live = spec.live.doc();
            assert!(
                live == doc_live || live.starts_with(&format!("{doc_live} (")),
                "source {name}: doc says `{live}`, code says `{doc_live}`"
            );
            code_from.extend(spec.from.iter().map(|s| (*s).to_owned()));
            order.push(name);
        }
        assert_eq!(from, code_from, "sources {names:?}: From column");
    }
    let code: Vec<&str> = SOURCES.iter().map(|s| s.name).collect();
    same_list("sources", &order, &code);
}

/// What a parameter's type should be, from how the doc writes it. A param the doc leaves
/// untyped gets the type its name has everywhere in §4 (checked list below).
#[derive(Debug, PartialEq, Eq)]
enum Expect {
    Exact(ParamType),
    Words(Vec<String>),
}

fn words(s: &str) -> Option<Vec<String>> {
    let w: Vec<String> = s.split('|').map(str::to_owned).collect();
    (w.len() > 1
        && w.iter()
            .all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c == '_')))
    .then_some(w)
}

fn expected(action: &str, p: &DocParam) -> Expect {
    use ParamType as T;
    if let Some(ty) = &p.ty {
        return match ty.as_str() {
            "Bytes" => Expect::Exact(T::Bytes),
            "[id]|all" => Expect::Exact(T::DlcIdsOrAll),
            "[id]" => Expect::Exact(T::DlcIds),
            "[n]" => Expect::Exact(T::Indices),
            "{os, lang, kinds, dlc}" => Expect::Exact(T::Selection),
            "unused|id" => Expect::Exact(T::UnusedOrCatalogId),
            other => Expect::Words(
                words(other).unwrap_or_else(|| panic!("{action}: unknown doc type `{other}`")),
            ),
        };
    }
    let name = p.name.as_deref().expect("named");
    let w = |xs: &[&str]| Expect::Words(xs.iter().map(|x| (*x).to_owned()).collect());
    Expect::Exact(match (action, name) {
        ("runtimes.install" | "renderers.install", "id") => T::CatalogId,
        (_, "id") => T::GameId,
        (_, "ids") => T::GameIds,
        (_, "job") => T::JobId,
        (_, "route") => T::Route,
        (_, "params") => T::RouteParams,
        (_, "scope") => T::PaletteScope,
        (_, "var") => T::UiVar,
        ("ui.set", "value") => T::UiValue,
        ("settings.set", "value") => T::SettingValue,
        (_, "key") => T::SettingKey,
        (_, "purge_state" | "keep_override" | "drop_saves" | "on" | "hdr" | "no_delete") => T::Bool,
        (_, "exe" | "n") => T::Index,
        (_, "build") => T::BuildId,
        (_, "runtime" | "renderer") => T::CatalogId,
        (_, "lang") => T::Lang,
        (_, "version") => T::Version,
        (_, "name") => T::EnvName,
        (_, "monitor") => T::Monitor,
        (_, "dlc") => T::DlcIdsOrAll,
        // The values `game.runner` reports (§3.3) and the CLI's `--prefer` (design 22).
        (_, "platform") => return w(&["linux", "mac"]),
        (_, "mode") => return w(&["wayland", "x11"]),
        (_, "prefer") => return w(&["local", "cloud", "newer"]),
        _ => panic!("{action}: no expected type for untyped param `{name}`"),
    })
}

fn matches(ty: ParamType, e: &Expect) -> bool {
    match (ty, e) {
        (ParamType::OneOf(values), Expect::Words(w)) => {
            values.iter().copied().eq(w.iter().map(String::as_str))
        }
        (ty, Expect::Exact(t)) => ty == *t,
        _ => false,
    }
}

#[test]
fn actions_match_section_4() {
    let doc = doc();
    let mut order = Vec::new();
    for row in rows(&section(&doc, "## 4. Actions")) {
        let names = spans(&row[0]);
        assert!(!names.is_empty(), "an action row without a name: {row:?}");
        let kind_tier = plain(&row[2]);
        for name in names {
            let spec = ACTIONS
                .iter()
                .find(|a| a.name == name)
                .unwrap_or_else(|| panic!("action {name} is in the doc, not in code"));
            if row[1] == "—" {
                assert!(
                    spec.params.is_empty(),
                    "action {name}: the doc has no params"
                );
            } else if let Some(code) = spans(&row[1]).first() {
                let params = doc_params(code);
                assert_eq!(
                    params.len(),
                    spec.params.len(),
                    "action {name}: param count"
                );
                for (d, c) in params.iter().zip(spec.params) {
                    match &d.name {
                        Some(n) => {
                            assert!(
                                !c.positional,
                                "action {name}: {} is positional in code",
                                c.name
                            );
                            assert_eq!(n, c.name, "action {name}: param name");
                        }
                        None => assert!(
                            c.positional,
                            "action {name}: {} is positional in the doc",
                            c.name
                        ),
                    }
                    assert_eq!(d.optional, c.optional, "action {name}: {} optional", c.name);
                    let e = expected(&name, d);
                    assert!(
                        matches(c.ty, &e),
                        "action {name}: {} is {:?}, the doc says {e:?}",
                        c.name,
                        c.ty
                    );
                }
            } else {
                // Only clipboard.copy describes its param in words.
                assert_eq!(
                    row[1], "a bound non-secret field",
                    "action {name}: params in words"
                );
                assert_eq!(spec.params.len(), 1);
                assert_eq!(spec.params[0].ty, ParamType::BoundField);
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
            order.push(name);
        }
    }
    let code: Vec<&str> = ACTIONS.iter().map(|a| a.name).collect();
    same_list("actions", &order, &code);
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
    let mut keys = Vec::new();
    for line in &lines[start + 1..end] {
        if !(line.starts_with("- ") || line.starts_with("  ")) {
            continue;
        }
        for span in spans(line) {
            // The doc mentions the action that also sets `download.limit`.
            if gogo_contract::actions::action(&span).is_some() {
                continue;
            }
            assert!(
                span.contains('.'),
                "`{span}` in the allow-list is not a key"
            );
            keys.push(span);
        }
    }
    same_list(
        "skin-settable keys",
        &keys,
        gogo_contract::settings::SKIN_SETTABLE,
    );
}

#[test]
fn app_only_keys_are_named_in_section_4_9() {
    let doc = doc();
    let text = section(&doc, "### 4.9 Queue and machine").join(" ");
    let app_only = text
        .split("A setting is **app-only**")
        .nth(1)
        .expect("app-only paragraph");
    for key in gogo_contract::settings::APP_ONLY {
        assert!(
            app_only.contains(&format!("`{key}`")),
            "{key} is not in the doc's app-only paragraph"
        );
    }
}

#[test]
fn every_status_tone_is_a_colour_token_in_section_6_1() {
    let doc = doc();
    let text = section(&doc, "### 6.1 Semantic tokens").join(" ");
    for s in Status::ALL {
        assert!(text.contains(s.tone()), "{} is not in §6.1", s.tone());
        if let Some(f) = s.tone_fallback() {
            let sentence = format!("`{f}` for {}", s.name());
            assert!(
                text.contains(&sentence),
                "§6.1 lacks the fallback `{sentence}`"
            );
        }
    }
}

#[test]
fn widgets_match_section_5() {
    let doc = doc();
    let lines = section(&doc, "## 5. App-owned widgets");
    let all = tables(&lines);
    let table = all
        .iter()
        .find(|t| t.header[0] == "Widget")
        .expect("the widget table");
    let mut order = Vec::new();
    for row in &table.rows {
        let depth = match row[1].trim_start_matches("**").chars().next() {
            Some('A') => Depth::A,
            Some('B') => Depth::B,
            Some('C') => Depth::C,
            _ => panic!("unknown depth `{}`", row[1]),
        };
        let mut parts: Vec<(String, bool)> = Vec::new();
        let mut variants: Vec<(String, Vec<String>)> = Vec::new();
        let mut placements = Vec::new();
        for clause in row[2].split("; ") {
            let clause = clause.trim();
            if clause.is_empty() || clause == "—" {
                continue;
            } else if let Some(rest) = clause.strip_prefix("parts ") {
                let list = spans(rest);
                assert_eq!(list.len(), 1, "parts clause `{clause}`");
                for part in list[0].split(", ") {
                    parts.push((part.trim_end_matches('*').to_owned(), part.ends_with('*')));
                }
            } else if clause.starts_with("variants ") || clause.starts_with("variant ") {
                let list = spans(clause);
                assert!(!list.is_empty(), "variant clause `{clause}`");
                for span in list {
                    let (name, values) = span
                        .split_once(": ")
                        .unwrap_or_else(|| panic!("unnamed variant {span}"));
                    variants.push((
                        name.to_owned(),
                        values.split('|').map(str::to_owned).collect(),
                    ));
                }
            } else if clause.starts_with("placement ") {
                for span in spans(clause) {
                    placements.push(match span.split('(').next() {
                        Some("inline") => Placement::Inline,
                        Some("modal") => Placement::Modal,
                        other => panic!("placement {other:?}"),
                    });
                }
            } else {
                panic!("unrecognised clause `{clause}` in the row of {}", row[0]);
            }
        }
        let names = spans(&row[0]);
        assert!(!names.is_empty(), "a widget row without a name: {row:?}");
        for name in names {
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
            let code_variants: Vec<(String, Vec<String>)> = w
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
            order.push(name);
        }
    }
    let code: Vec<&str> = WIDGETS.iter().map(|w| w.name).collect();
    same_list("widgets", &order, &code);
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
    let labels: Vec<&str> = bullets.keys().map(String::as_str).collect();
    assert_eq!(
        labels,
        ["Colours", "Motion", "Shape", "Type roles"],
        "token bullets"
    );
    let words = |label: &str| -> BTreeSet<String> {
        spans(&bullets[label])
            .iter()
            .flat_map(|s| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
            .filter(|t| t != "easing.*")
            .flat_map(|t| expand(&t))
            .collect()
    };
    same_set("colour tokens", words("Colours"), COLOURS);
    same_set("type roles", words("Type roles"), FONT_ROLES);
    same_set("shape tokens", words("Shape"), SHAPE);
    same_set("motion tokens", words("Motion"), MOTION);
    assert!(
        bullets["Motion"].contains("`easing.*`"),
        "easing.* is open-ended in code"
    );
    assert!(
        bullets["Type roles"].contains("each with family, size and weight"),
        "type-role fields"
    );
    assert_eq!(FONT_FIELDS, ["family", "size", "weight"]);
}

#[test]
fn routes_match_section_6_2() {
    let doc = doc();
    let mut order = Vec::new();
    for row in rows(&section(&doc, "### 6.2 Routes")) {
        let name = spans(&row[0]).remove(0);
        let r = ROUTES
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("route {name} is in the doc, not in code"));
        let params = if row[1] == "—" {
            Vec::new()
        } else {
            names_only(&doc_params(&spans(&row[1])[0]))
        };
        let code: Vec<(String, bool)> = r
            .params
            .iter()
            .map(|p| (p.name.to_owned(), p.optional))
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
        order.push(name);
    }
    let code: Vec<&str> = ROUTES.iter().map(|r| r.name).collect();
    same_list("routes", &order, &code);
}

#[test]
fn types_statuses_and_oses_match_section_2() {
    let doc = doc();
    let mut order = Vec::new();
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
        let values =
            || -> Vec<String> { spans(&row[1])[0].split(" · ").map(str::to_owned).collect() };
        if name == "Status" {
            let code: Vec<&str> = Status::ALL.iter().map(|s| s.name()).collect();
            same_list("Status values", &values(), &code);
        }
        if name == "Os" {
            let code: Vec<&str> = Os::ALL.iter().map(|o| o.name()).collect();
            same_list("Os values", &values(), &code);
        }
        order.push(name);
    }
    let code: Vec<&str> = ValueType::ALL.iter().map(|t| t.name()).collect();
    same_list("types", &order, &code);
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

/// A type as §3 writes it.
#[derive(Debug, Clone)]
enum DocType {
    /// No type written: `Text`.
    Untyped,
    /// `Text`, `GameSummary`, or `source` (the type of the sibling field `source`).
    Name(String),
    /// `a|b|c`; digits only (`1|2`) is a `Number`.
    Enum(Vec<String>),
    List(Box<DocType>),
    Masked(Box<DocType>),
    Opt(Box<DocType>),
    /// `Name{ … }` or `{ … }`.
    Rec(Option<String>, Vec<(String, DocType)>),
    /// `GameSummary + { … }`.
    Plus(String, Vec<(String, DocType)>),
}

struct ShapeParser {
    toks: Vec<String>,
    i: usize,
}

impl ShapeParser {
    fn new(src: &str) -> Self {
        let mut toks = Vec::new();
        let mut word = String::new();
        for c in src.chars() {
            if c.is_ascii_alphanumeric() || c == '_' {
                word.push(c);
                continue;
            }
            if !word.is_empty() {
                toks.push(std::mem::take(&mut word));
            }
            if !c.is_whitespace() {
                toks.push(c.to_string());
            }
        }
        if !word.is_empty() {
            toks.push(word);
        }
        Self { toks, i: 0 }
    }

    fn peek(&self) -> &str {
        self.toks.get(self.i).map_or("", String::as_str)
    }

    fn eat(&mut self, t: &str) -> bool {
        let hit = self.peek() == t;
        if hit {
            self.i += 1;
        }
        hit
    }

    fn expect(&mut self, t: &str) {
        assert!(
            self.eat(t),
            "expected `{t}` at token {} of {:?}",
            self.i,
            self.toks
        );
    }

    fn word(&mut self) -> String {
        let w = self.peek().to_owned();
        assert!(
            w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !w.is_empty(),
            "expected a name at token {} of {:?}",
            self.i,
            self.toks
        );
        self.i += 1;
        w
    }

    fn done(&self) -> bool {
        self.i == self.toks.len()
    }

    fn ty(&mut self) -> DocType {
        let t = self.atom();
        if self.eat("+") {
            let DocType::Name(base) = t else {
                panic!("`+` after a non-name in {:?}", self.toks)
            };
            self.expect("{");
            return DocType::Plus(base, self.fields());
        }
        t
    }

    fn atom(&mut self) -> DocType {
        let t = if self.eat("{") {
            DocType::Rec(None, self.fields())
        } else if self.eat("[") {
            let t = self.ty();
            self.expect("]");
            DocType::List(Box::new(t))
        } else {
            let w = self.word();
            if (w == "List" || w == "Masked") && self.eat("<") {
                let t = self.ty();
                self.expect(">");
                if w == "List" {
                    DocType::List(Box::new(t))
                } else {
                    DocType::Masked(Box::new(t))
                }
            } else if self.eat("{") {
                DocType::Rec(Some(w), self.fields())
            } else if self.peek() == "|" {
                let mut words = vec![w];
                while self.eat("|") {
                    words.push(self.word());
                }
                DocType::Enum(words)
            } else {
                DocType::Name(w)
            }
        };
        if self.eat("?") {
            DocType::Opt(Box::new(t))
        } else {
            t
        }
    }

    /// The fields up to and including the closing `}`.
    fn fields(&mut self) -> Vec<(String, DocType)> {
        let mut out = Vec::new();
        loop {
            if self.eat("}") {
                return out;
            }
            let name = self.word();
            let optional = self.eat("?");
            let mut ty = if self.eat(":") {
                self.ty()
            } else if self.eat("{") {
                DocType::Rec(None, self.fields())
            } else {
                DocType::Untyped
            };
            // A note in brackets: `progress: Fraction? (while a job runs)`.
            if self.eat("(") {
                while !self.eat(")") {
                    assert!(!self.done(), "unclosed `(` in {:?}", self.toks);
                    self.i += 1;
                }
            }
            if optional {
                ty = DocType::Opt(Box::new(ty));
            }
            out.push((name, ty));
            if !self.eat(",") {
                self.expect("}");
                return out;
            }
        }
    }
}

fn parse_shape(src: &str) -> DocType {
    let mut p = ShapeParser::new(src);
    let t = p.ty();
    assert!(p.done(), "trailing tokens in `{src}`: {:?}", &p.toks[p.i..]);
    t
}

struct ShapeCheck {
    /// Named records the doc defines apart from where they are used (`Job` = `{ … }`).
    defs: BTreeMap<String, DocType>,
    /// Named records whose fields were compared.
    checked: BTreeSet<String>,
    /// Named records used by name only.
    named: BTreeSet<String>,
}

impl ShapeCheck {
    fn scalar(name: &str) -> Option<FieldType> {
        Some(match name {
            "Text" => FieldType::Text,
            "Number" => FieldType::Number,
            "Bool" => FieldType::Bool,
            "Bytes" => FieldType::Bytes,
            "Time" => FieldType::Time,
            "Duration" => FieldType::Duration,
            "Fraction" => FieldType::Fraction,
            "GameId" => FieldType::GameId,
            "JobId" => FieldType::JobId,
            "Status" => FieldType::Status,
            "Os" => FieldType::Os,
            "RichText" => FieldType::RichText,
            "ActionRef" => FieldType::ActionRef,
            _ => return None,
        })
    }

    fn check(&mut self, at: &str, doc: &DocType, code: &FieldType, siblings: &[&Field]) {
        match (doc, code) {
            (DocType::Untyped, FieldType::Text) => {}
            (DocType::Opt(d), FieldType::Option(c)) => self.check(at, d, c, siblings),
            (DocType::List(d), FieldType::List(c)) => self.check(at, d, c, siblings),
            (DocType::Masked(d), FieldType::Masked(c)) => self.check(at, d, c, siblings),
            (DocType::Enum(words), c) => {
                if words.iter().all(|w| w.chars().all(|c| c.is_ascii_digit())) {
                    assert_eq!(*c, FieldType::Number, "{at}: a numeric enum");
                } else {
                    assert_eq!(*c, FieldType::OneOf(leak(words)), "{at}: enum words");
                }
            }
            (DocType::Name(n), c) if Self::scalar(n).is_some() => {
                assert_eq!(Self::scalar(n).as_ref(), Some(c), "{at}: doc says {n}");
            }
            (DocType::Name(n), c) if n.starts_with(char::is_lowercase) => {
                let sib = siblings
                    .iter()
                    .find(|f| f.name == n)
                    .unwrap_or_else(|| panic!("{at}: `[{n}]` names no sibling field"));
                assert_eq!(sib.ty, *c, "{at}: the type of `{n}`");
            }
            (DocType::Name(n), FieldType::Record(s)) => {
                assert_eq!(s.name, n, "{at}: record");
                if let Some(def) = self.defs.get(n).cloned()
                    && self.checked.insert(n.clone())
                {
                    self.check(n, &def, code, &[]);
                }
                self.named.insert(n.clone());
            }
            (DocType::Rec(name, fields), FieldType::Record(s)) => {
                if let Some(n) = name {
                    assert_eq!(s.name, n, "{at}: record name");
                    self.checked.insert(n.clone());
                }
                assert!(
                    s.extends.is_none(),
                    "{at}: the code extends {:?}",
                    s.extends
                );
                // A named record's fields are reported under its name: `Setting.value`.
                self.fields(name.as_deref().unwrap_or(at), fields, s.fields);
            }
            (DocType::Plus(base, fields), FieldType::Record(s)) => {
                let ext = s.extends.unwrap_or_else(|| panic!("{at}: extends nothing"));
                assert_eq!(ext.name, base, "{at}: extends");
                self.named.insert(base.clone());
                self.fields(at, fields, s.fields);
            }
            (d, c) => panic!("{at}: doc {d:?}, code {c:?}"),
        }
    }

    fn fields(&mut self, at: &str, doc: &[(String, DocType)], code: &'static [Field]) {
        let names: Vec<&str> = doc.iter().map(|(n, _)| n.as_str()).collect();
        let code_names: Vec<&str> = code.iter().map(|f| f.name).collect();
        assert_eq!(names, code_names, "{at}: fields");
        let siblings: Vec<&Field> = code.iter().collect();
        for ((name, d), f) in doc.iter().zip(code) {
            let at = format!("{at}.{name}");
            // A setting's value and default are typed by its `type` field.
            if at == "Setting.value" || at == "Setting.default" {
                assert!(matches!(d, DocType::Untyped), "{at}");
                assert_eq!(f.ty, FieldType::Scalar, "{at}");
                continue;
            }
            self.check(&at, d, &f.ty, &siblings);
        }
    }
}

fn leak(words: &[String]) -> &'static [&'static str] {
    Box::leak(
        words
            .iter()
            .map(|w| &*Box::leak(w.clone().into_boxed_str()))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    )
}

#[test]
fn shapes_match_section_3() {
    let doc = doc();
    let lines = section(&doc, "## 3. Data sources");
    let text = lines.join(" ");
    // `Name` = `{ … }`, and standalone `Name{ … }` spans (ErrorView in §3.7).
    let mut defs = BTreeMap::new();
    let parts: Vec<&str> = text.split('`').collect();
    for (i, part) in parts.iter().enumerate().skip(1).step_by(2) {
        let after = parts.get(i + 1).copied().unwrap_or("");
        if part.chars().all(|c| c.is_ascii_alphanumeric()) && after.trim() == "=" {
            defs.insert((*part).to_owned(), parse_shape(parts[i + 2]));
        } else if part.starts_with(char::is_uppercase)
            && part.ends_with('}')
            && !part.contains('<')
            && let DocType::Rec(Some(n), f) = parse_shape(part)
        {
            defs.insert(n.clone(), DocType::Rec(Some(n), f));
        }
    }
    let mut ck = ShapeCheck {
        defs,
        checked: BTreeSet::new(),
        named: BTreeSet::new(),
    };
    for row in rows(&lines) {
        let shape_cell = &row[1];
        let s = spans(shape_cell);
        for span in spans(&row[0]) {
            let (name, _) = call(&span);
            let code = shapes::source_shape(&name).unwrap_or_else(|| panic!("{name}: no shape"));
            match code.inner() {
                // The skin's own state, and what only an app widget shows.
                FieldType::Ui | FieldType::Opaque(_) => continue,
                _ => {}
            }
            let src = if shape_cell.contains("` + `") {
                format!("{} + {}", s[0], s[1])
            } else {
                s[0].clone()
            };
            ck.check(&name, &parse_shape(&src), &code, &[]);
        }
    }
    // §2 lists Art's fields in prose: `cover`, `background`, `logo`, plus `colors: …`.
    let art = rows(&section(&doc, "## 2. Types"))
        .into_iter()
        .find(|r| r[0] == "`Art`")
        .expect("an Art row");
    let art_fields: Vec<String> = spans(&art[1])
        .iter()
        .map(|s| s.split(':').next().unwrap_or(s).to_owned())
        .collect();
    let code: Vec<&str> = shapes::ART.fields.iter().map(|f| f.name).collect();
    assert_eq!(art_fields, code, "Art fields (§2)");
    assert_eq!(
        shapes::ART.field("colors").map(|f| f.ty),
        Some(FieldType::List(&FieldType::Color))
    );
    ck.checked.insert("Art".to_owned());
    let unchecked: Vec<_> = ck.named.difference(&ck.checked).collect();
    assert!(
        unchecked.is_empty(),
        "named in §3, fields never compared: {unchecked:?}"
    );
    // Plan reaches a skin only through `plan-card`, so it has no shape in code.
    let spare: Vec<_> = ck
        .defs
        .keys()
        .filter(|k| !ck.checked.contains(*k) && *k != "Plan")
        .collect();
    assert!(spare.is_empty(), "defined in §3, used nowhere: {spare:?}");
}

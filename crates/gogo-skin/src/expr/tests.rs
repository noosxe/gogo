use std::collections::BTreeMap;

use gogo_contract::{Status, Value};

use super::*;

pub(super) struct S(pub BTreeMap<String, Value>);

impl Scope for S {
    fn var(&self, n: &str) -> Option<Value> {
        self.0.get(n).cloned()
    }
    fn param(&self, n: &str) -> Option<Value> {
        (n == "id").then_some(Value::Num(7.0))
    }
    fn token(&self, n: &str) -> Option<Value> {
        match n {
            "warn" => Some(Value::str("#e0a040")),
            "accent" => Some(Value::Num(1.0)),
            _ => None,
        }
    }
    fn call(&self, n: &str, a: &[Value]) -> Option<Result<Value, CallError>> {
        match n {
            "game.builds" => {
                assert_eq!(a, &[Value::Num(7.0)]);
                Some(Ok(Value::List(
                    [3.0, 1.0, 2.0]
                        .iter()
                        .map(|d| {
                            let mut m = BTreeMap::new();
                            m.insert("date".into(), Value::Num(*d));
                            m.insert("installed".into(), Value::Bool(*d > 1.5));
                            Value::Map(m)
                        })
                        .collect(),
                )))
            }
            "numbers" => Some(Ok(Value::List(
                (1..=2500).map(|i| Value::Num(f64::from(i))).collect(),
            ))),
            "tabs" => Some(Ok(Value::List(
                ["a", "b", "c"].iter().map(|s| Value::str(*s)).collect(),
            ))),
            "none" => Some(Ok(Value::List(vec![]))),
            "boom" => Some(Err(CallError::new("it broke"))),
            _ => None,
        }
    }
    fn now(&self) -> f64 {
        1_000_000.0
    }
}

pub(super) fn scope() -> S {
    let mut g = BTreeMap::new();
    g.insert("title".into(), Value::str("Hollow Lantern"));
    g.insert("status".into(), Value::str("ready"));
    g.insert("update".into(), Value::Null);
    let mut m = BTreeMap::new();
    m.insert("g".into(), Value::Map(g));
    m.insert("tab".into(), Value::str("b"));
    m.insert("count".into(), Value::Num(3.0));
    S(m)
}

fn ev(src: &str) -> Result<Value, EvalError> {
    eval(&parse(src).unwrap(), &scope())
}

fn parse_kind(src: &str) -> ParseErrorKind {
    parse(src).unwrap_err().kind
}

#[test]
fn paths_enums_and_logic() {
    assert_eq!(ev("g.title").unwrap(), Value::str("Hollow Lantern"));
    assert_eq!(
        ev("g.status == ready and not false").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(ev("g.update?.to else '—'").unwrap(), Value::str("—"));
    assert_eq!(
        ev("g.status == ready ? 'a' : 'b'").unwrap(),
        Value::str("a")
    );
    assert_eq!(ev("count > 2 and count <= 3").unwrap(), Value::Bool(true));
    assert_eq!(ev("count != 3 or -count < 0").unwrap(), Value::Bool(true));
}

#[test]
fn typos_are_errors() {
    let e = ev("g.titel").unwrap_err();
    assert!(e.to_string().contains("no field `titel`"), "{e}");
    assert!(ev("g.update.to").is_err());
    assert!(matches!(ev("nosuch.fn(1)"), Err(EvalError::UnknownCall(_))));
    assert!(matches!(ev("$nope"), Err(EvalError::UnknownParam(_))));
    assert!(matches!(ev("@nope"), Err(EvalError::UnknownToken(_))));
    assert!(matches!(ev("boom()"), Err(EvalError::Call { .. })));
    assert!(matches!(ev("-g.title"), Err(EvalError::NegNeedsNumber(_))));
    assert!(matches!(ev("g.title < 3"), Err(EvalError::Compare { .. })));
    assert!(parse("a |").is_err());
}

#[test]
fn parse_errors_carry_spans() {
    let e = parse("a | frob").unwrap_err();
    assert_eq!(e.kind, ParseErrorKind::UnknownStage("frob".into()));
    assert_eq!(e.span, Span { offset: 4, len: 4 });
    let e = parse("a +").unwrap_err();
    assert_eq!(e.kind, ParseErrorKind::UnexpectedChar('+'));
    assert_eq!(e.span, Span { offset: 2, len: 1 });
    // Multi-byte characters: offsets are bytes.
    let e = parse("é + 1").unwrap_err();
    assert_eq!(e.span, Span { offset: 3, len: 1 });
    let e = parse("(a").unwrap_err();
    assert_eq!(e.kind, ParseErrorKind::Expected("`)`"));
    assert_eq!(e.span, Span { offset: 2, len: 0 });
    let e = parse("a b").unwrap_err();
    assert!(matches!(e.kind, ParseErrorKind::TrailingInput(_)));
    assert_eq!(e.span, Span { offset: 2, len: 1 });
    assert_eq!(parse_kind("'abc"), ParseErrorKind::UnterminatedString);
    assert_eq!(parse_kind("@"), ParseErrorKind::MissingName('@'));
    assert!(matches!(parse_kind("1.2.3"), ParseErrorKind::BadNumber(_)));
    assert!(matches!(
        parse_kind(&"9".repeat(400)),
        ParseErrorKind::BadNumber(_)
    ));
    // Diagnostics expose the span as a label.
    use miette::Diagnostic;
    let labels: Vec<_> = e.labels().unwrap().collect();
    assert_eq!((labels[0].offset(), labels[0].len()), (2, 1));
}

#[test]
fn pipes() {
    assert_eq!(ev("game.builds($id) | len").unwrap(), Value::Num(3.0));
    assert_eq!(
        ev("game.builds($id) | where installed | len").unwrap(),
        Value::Num(2.0)
    );
    assert_eq!(
        ev("game.builds($id) | sort -date | first.date").unwrap(),
        Value::Num(3.0)
    );
    assert_eq!(
        ev("game.builds($id) | sort date | take 2 | len").unwrap(),
        Value::Num(2.0)
    );
    assert_eq!(ev("g.title | upper").unwrap(), Value::str("HOLLOW LANTERN"));
    assert_eq!(ev("g.title | lower").unwrap(), Value::str("hollow lantern"));
    assert_eq!(ev("none() | first").unwrap(), Value::Null);
    assert_eq!(ev("g.update | len").unwrap(), Value::Num(0.0));
    assert!(matches!(
        ev("g.title | len"),
        Err(EvalError::NeedsList { .. })
    ));
}

#[test]
fn sort_mixed_values_is_total() {
    // Items without the key sort as null, first; no panic on odd data.
    assert_eq!(
        ev("tabs() | sort missing | first").unwrap(),
        Value::str("a")
    );
    assert_eq!(ev("tabs() | sort -missing | len").unwrap(), Value::Num(3.0));
}

#[test]
fn before_and_after() {
    assert_eq!(ev("tabs() | after tab").unwrap(), Value::str("c"));
    assert_eq!(ev("tabs() | before tab").unwrap(), Value::str("a"));
    // Wrap around the ends.
    assert_eq!(ev("tabs() | after 'c'").unwrap(), Value::str("a"));
    assert_eq!(ev("tabs() | before 'a'").unwrap(), Value::str("c"));
    // Not in the list: the first item. Empty list: null.
    assert_eq!(ev("tabs() | after 'zzz'").unwrap(), Value::str("a"));
    assert_eq!(ev("tabs() | before 'zzz'").unwrap(), Value::str("a"));
    assert_eq!(ev("none() | after 'x'").unwrap(), Value::Null);
    assert_eq!(ev("none() | before 'x'").unwrap(), Value::Null);
    assert_eq!(ev("g.update | before 'x'").unwrap(), Value::Null);
}

#[test]
fn lists_are_capped_at_a_thousand() {
    assert_eq!(ev("numbers() | len").unwrap(), Value::Num(2500.0));
    assert_eq!(
        ev("numbers() | take 5000 | len").unwrap(),
        Value::Num(1000.0)
    );
    assert_eq!(ev("numbers() | take 999 | len").unwrap(), Value::Num(999.0));
    assert_eq!(
        ev("numbers() | take 1000 | len").unwrap(),
        Value::Num(1000.0)
    );
    assert_eq!(ev("numbers() | take (-4) | len").unwrap(), Value::Num(0.0));
    assert!(matches!(
        ev("numbers() | take 'x'"),
        Err(EvalError::StageNeedsNumber { stage: "take", .. })
    ));
}

#[test]
fn tokens_and_formatters() {
    assert_eq!(ev("@warn alpha=.55").unwrap(), Value::str("#e0a0408c"));
    assert_eq!(ev("@warn alpha=1").unwrap(), Value::str("#e0a040ff"));
    assert_eq!(ev("@warn").unwrap(), Value::str("#e0a040"));
    assert!(matches!(
        ev("@accent alpha=.5"),
        Err(EvalError::NotAColour(_))
    ));
    assert!(matches!(
        ev("@warn alpha='x'"),
        Err(EvalError::AlphaNotNumber)
    ));
    assert_eq!(ev("fmt.bytes(18600000000)").unwrap(), Value::str("18.6 GB"));
    assert_eq!(ev("fmt.bytes(512)").unwrap(), Value::str("512 B"));
    assert_eq!(ev("fmt.rate(48200000)").unwrap(), Value::str("48.2 MB/s"));
    assert_eq!(
        ev("fmt.date(1763424000)").unwrap(),
        Value::str("2025-11-18")
    );
    assert_eq!(ev("fmt.ago(827200)").unwrap(), Value::str("2 days ago"));
    assert_eq!(ev("fmt.ago(999990)").unwrap(), Value::str("just now"));
    assert_eq!(ev("fmt.ago(996400)").unwrap(), Value::str("1 h ago"));
    assert_eq!(ev("fmt.hours(147600)").unwrap(), Value::str("41 h"));
    assert_eq!(ev("fmt.eta(720)").unwrap(), Value::str("12 min left"));
    assert_eq!(ev("fmt.pct(0.62)").unwrap(), Value::str("62%"));
    assert_eq!(ev("fmt.pct(g.update)").unwrap(), Value::Null);
    assert!(matches!(
        ev("fmt.pct('x')"),
        Err(EvalError::FmtNeedsNumber { .. })
    ));
    assert!(matches!(ev("fmt.bytes()"), Err(EvalError::Arity { .. })));
    assert!(matches!(
        ev("fmt.bytes(1, 2)"),
        Err(EvalError::Arity { .. })
    ));
    // Extremes don't panic.
    let _ = ev("fmt.date(99999999999999999999999999)");
    let _ = ev("fmt.date(-99999999999999999999999999)");
}

#[test]
fn status_helpers_use_the_contract() {
    assert_eq!(ev("status.word(g.status)").unwrap(), Value::str("Ready"));
    for st in Status::ALL {
        let mut sc = scope();
        sc.0.insert("s".into(), Value::str(st.name()));
        let got = eval(&parse("status.tone(s)").unwrap(), &sc).unwrap();
        assert_eq!(got, Value::str(st.tone()));
        let word = eval(&parse("status.word(s)").unwrap(), &sc).unwrap();
        assert!(!word.display().is_empty());
    }
    // An enum word as a bare name works too.
    assert_eq!(
        ev("status.tone(update_available)").unwrap(),
        Value::str("tone.update")
    );
    assert!(matches!(
        ev("status.tone('installed')"),
        Err(EvalError::UnknownStatus(_))
    ));
    assert!(matches!(
        ev("status.word(3)"),
        Err(EvalError::UnknownStatus(_))
    ));
}

#[test]
fn templates() {
    let t = parse_template("Update {g.update?.to} available").unwrap();
    assert!(matches!(t, Template::Interp(ref s) if s.len() == 3));
    assert!(matches!(
        parse_template("{g.title}").unwrap(),
        Template::Expr(_)
    ));
    assert!(matches!(
        parse_template("plain").unwrap(),
        Template::Text(_)
    ));
    assert_eq!(parse_template("").unwrap(), Template::Text(String::new()));
    assert!(parse_template("{oops").is_err());
    assert!(parse(&"a".repeat(MAX_EXPR_LEN + 1)).is_err());
}

#[test]
fn templates_evaluate() {
    let sc = scope();
    let t = parse_template("{g.title} is {g.status | upper}!").unwrap();
    assert_eq!(t.eval(&sc).unwrap(), Value::str("Hollow Lantern is READY!"));
    let t = parse_template("{count}").unwrap();
    assert_eq!(t.eval(&sc).unwrap(), Value::Num(3.0));
    let t = parse_template("{'}'} and {\"{\"}").unwrap();
    assert_eq!(t.eval(&sc).unwrap(), Value::str("} and {"));
    let t = parse_template("n = {count}").unwrap();
    assert!(t.mentions("count") && !t.mentions("g"));
}

#[test]
fn doubled_braces_are_literal() {
    let sc = scope();
    let text = |s: &str| match parse_template(s).unwrap() {
        Template::Text(t) => t,
        other => panic!("{other:?}"),
    };
    assert_eq!(text("{{"), "{");
    assert_eq!(text("}}"), "}");
    assert_eq!(text("{{x}}"), "{x}");
    assert_eq!(text("a {{ b }} c"), "a { b } c");
    assert_eq!(text("{{{{"), "{{");
    let t = parse_template("{{{count}}}").unwrap();
    assert_eq!(t.eval(&sc).unwrap(), Value::str("{3}"));
    let t = parse_template("{{ {count} }}").unwrap();
    assert_eq!(t.eval(&sc).unwrap(), Value::str("{ 3 }"));
}

#[test]
fn stray_and_unclosed_braces_are_errors_with_spans() {
    let e = parse_template("ab }").unwrap_err();
    assert_eq!(e.kind, TemplateErrorKind::StrayCloseBrace);
    assert_eq!(e.span, Span { offset: 3, len: 1 });
    let e = parse_template("ab {count").unwrap_err();
    assert_eq!(e.kind, TemplateErrorKind::UnclosedBrace);
    assert_eq!(e.span, Span { offset: 3, len: 1 });
    assert_eq!(
        parse_template("{count}}").unwrap_err().kind,
        TemplateErrorKind::StrayCloseBrace
    );
    // An error inside a hole points into the template text.
    let e = parse_template("héllo {count +}").unwrap_err();
    assert_eq!(
        e.kind,
        TemplateErrorKind::Expr(ParseErrorKind::UnexpectedChar('+'))
    );
    assert_eq!(e.span, Span { offset: 14, len: 1 });
    assert!(e.expr_kind().is_some());
    let e = parse_template("{}").unwrap_err();
    assert_eq!(e.span, Span { offset: 1, len: 0 });
}

#[test]
fn length_limit() {
    let at_limit = format!("'{}'", "x".repeat(MAX_EXPR_LEN - 2));
    assert_eq!(at_limit.chars().count(), MAX_EXPR_LEN);
    assert!(parse(&at_limit).is_ok());
    let over = format!("'{}'", "x".repeat(MAX_EXPR_LEN - 1));
    assert!(matches!(
        parse_kind(&over),
        ParseErrorKind::TooLong { got: 513, max: 512 }
    ));
    // Characters, not bytes: 512 multi-byte characters pass.
    let wide = format!("'{}'", "é".repeat(MAX_EXPR_LEN - 2));
    assert!(wide.len() > MAX_EXPR_LEN);
    assert!(parse(&wide).is_ok());
    // The limit applies to each expression of a template.
    let t = format!("{{{over}}}");
    assert!(matches!(
        parse_template(&t).unwrap_err().kind,
        TemplateErrorKind::Expr(ParseErrorKind::TooLong { .. })
    ));
}

#[test]
fn depth_limit() {
    let parens = |n: usize| format!("{}1{}", "(".repeat(n), ")".repeat(n));
    assert!(parse(&parens(MAX_EXPR_DEPTH - 1)).is_ok());
    assert!(matches!(
        parse_kind(&parens(MAX_EXPR_DEPTH)),
        ParseErrorKind::TooDeep { .. }
    ));
    assert!(parse(&format!("{}a", "not ".repeat(MAX_EXPR_DEPTH - 1))).is_ok());
    assert!(matches!(
        parse_kind(&format!("{}a", "not ".repeat(MAX_EXPR_DEPTH))),
        ParseErrorKind::TooDeep { .. }
    ));
    assert!(matches!(
        parse_kind(&format!("{}1", "-".repeat(MAX_EXPR_DEPTH))),
        ParseErrorKind::TooDeep { .. }
    ));
    assert!(matches!(
        parse_kind(&format!("{}a", "a ? a : ".repeat(MAX_EXPR_DEPTH))),
        ParseErrorKind::TooDeep { .. }
    ));
    assert!(matches!(
        parse_kind(&format!(
            "{}1{}",
            "f(".repeat(MAX_EXPR_DEPTH),
            ")".repeat(MAX_EXPR_DEPTH)
        )),
        ParseErrorKind::TooDeep { .. }
    ));
    // Realistic expressions are nowhere near it.
    assert!(parse("a ? (b ? c : d) : (e and not (f or g))").is_ok());
}

#[test]
fn mentions_finds_variables() {
    let e = parse("value == 'x' ? 1 : 2").unwrap();
    assert!(e.mentions("value") && !e.mentions("on"));
    let e = parse("tabs() | where (value == 1) | take on").unwrap();
    assert!(e.mentions("value") && e.mentions("on"));
    assert!(!parse("$id").unwrap().mentions("id"));
}

#[test]
fn eval_never_reparses() {
    // A string value that looks like an expression stays a string.
    let mut sc = scope();
    sc.0.insert("x".into(), Value::str("1 +"));
    assert_eq!(eval(&parse("x").unwrap(), &sc).unwrap(), Value::str("1 +"));
}

#[test]
fn trim_cuts_text_with_an_ellipsis() {
    assert_eq!(
        ev("g.title | trim 20").unwrap(),
        Value::str("Hollow Lantern")
    );
    assert_eq!(
        ev("g.title | trim 14").unwrap(),
        Value::str("Hollow Lantern")
    );
    assert_eq!(ev("g.title | trim 7").unwrap(), Value::str("Hollow…"));
    assert_eq!(ev("g.title | trim 1").unwrap(), Value::str("…"));
    assert_eq!(ev("g.title | trim 0").unwrap(), Value::str(""));
    assert_eq!(ev("g.title | trim (-3)").unwrap(), Value::str(""));
    // Characters, not bytes.
    assert_eq!(ev("'ÄÖÜäöü' | trim 4").unwrap(), Value::str("ÄÖÜ…"));
    assert!(matches!(
        ev("g.title | trim 'x'"),
        Err(EvalError::StageNeedsNumber { stage: "trim", .. })
    ));
}

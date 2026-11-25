//! Property tests: the lexer, parser and evaluator never panic and never overflow the
//! stack, whatever the input.

use proptest::prelude::*;

use super::tests::scope;
use super::*;

fn cfg() -> ProptestConfig {
    ProptestConfig::with_cases(96)
}

/// Source text built from the language's own pieces, to get past the lexer often.
fn fragments() -> impl Strategy<Value = String> {
    let frag = prop::sample::select(vec![
        "g",
        "g.title",
        "g.update",
        "?.",
        ".",
        "1",
        ".5",
        "'x'",
        "\"y\"",
        "(",
        ")",
        "|",
        "sort",
        "-",
        "date",
        "take",
        "trim",
        "3",
        "where",
        "installed",
        "len",
        "first",
        "before",
        "after",
        "else",
        "?",
        ":",
        "not",
        "and",
        "or",
        "==",
        "!=",
        "<",
        ">=",
        "fmt.bytes(",
        "fmt.ago(",
        "status.tone(",
        "status.word(",
        "@warn",
        "alpha=",
        "$id",
        "game.builds($id)",
        "tabs()",
        ",",
        "true",
        "null",
        "upper",
        "lower",
        "tab",
        "count",
    ]);
    prop::collection::vec(frag, 0..14).prop_map(|v| v.join(" "))
}

/// Source text that mostly parses, built by nesting valid forms.
fn expressions() -> impl Strategy<Value = String> {
    let leaf = prop::sample::select(vec![
        "g.title",
        "7",
        "'abc'",
        "null",
        "g.update?.to",
        "game.builds($id)",
        "tabs()",
        "$id",
        "@warn",
        "true",
        "count",
        "tab",
    ])
    .prop_map(String::from);
    leaf.prop_recursive(5, 48, 3, |inner| {
        prop_oneof![
            inner.clone().prop_map(|a| format!("({a})")),
            inner.clone().prop_map(|a| format!("not {a}")),
            inner.clone().prop_map(|a| format!("-{a}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} and {b}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} == {b}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} < {b}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} else {b}")),
            (inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(a, b, c)| format!("{a} ? {b} : {c}")),
            inner.clone().prop_map(|a| format!("{a} | len")),
            inner.clone().prop_map(|a| format!("{a} | first")),
            inner.clone().prop_map(|a| format!("{a} | upper")),
            inner.clone().prop_map(|a| format!("{a} | take 2")),
            inner.clone().prop_map(|a| format!("{a} | trim 3")),
            inner.clone().prop_map(|a| format!("{a} | sort -date")),
            inner.clone().prop_map(|a| format!("{a} | where installed")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} | before ({b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} | after ({b})")),
            inner.clone().prop_map(|a| format!("fmt.bytes({a})")),
            inner.clone().prop_map(|a| format!("fmt.ago({a})")),
            inner.clone().prop_map(|a| format!("status.tone({a})")),
            inner.clone().prop_map(|a| format!("status.word({a})")),
            inner.prop_map(|a| format!("{a}.len")),
        ]
    })
}

proptest! {
    #![proptest_config(cfg())]

    #[test]
    fn parsers_never_panic_on_any_text(s in any::<String>()) {
        let _ = parse(&s);
        let _ = parse_template(&s);
    }

    #[test]
    fn parsers_never_panic_on_brace_soup(s in "[{}()'\" a-z0-9$@.?:|,=!<>\\-é]{0,700}") {
        let _ = parse(&s);
        let _ = parse_template(&s);
    }

    #[test]
    fn parsers_never_panic_on_language_soup(s in fragments()) {
        let _ = parse(&s);
        let _ = parse_template(&s);
        let _ = parse_template(&format!("x {{{s}}} y"));
    }

    #[test]
    fn too_long_is_rejected(extra in 1usize..2000, c in prop::sample::select(vec!['a', '1', ' ', 'é', '\u{1F600}'])) {
        let s: String = std::iter::repeat_n(c, MAX_EXPR_LEN + extra).collect();
        let e = parse(&s).unwrap_err();
        let too_long = matches!(e.kind, ParseErrorKind::TooLong { .. });
        prop_assert!(too_long);
        prop_assert_eq!(e.span.offset, 0);
        prop_assert_eq!(e.span.len, s.len());
    }

    #[test]
    fn deep_nesting_is_rejected_not_overflowed(
        n in MAX_EXPR_DEPTH..(MAX_EXPR_LEN / 2 - 2),
        shape in 0usize..4,
    ) {
        let src = match shape {
            0 => format!("{}1{}", "(".repeat(n), ")".repeat(n)),
            1 => format!("{}a", "not ".repeat(n.min(120))),
            2 => format!("{}1", "-".repeat(n)),
            _ => format!("{}a", "a ? a : ".repeat(n.min(60))),
        };
        prop_assume!(src.chars().count() <= MAX_EXPR_LEN);
        let e = parse(&src).unwrap_err();
        let too_deep = matches!(e.kind, ParseErrorKind::TooDeep { .. });
        prop_assert!(too_deep);
    }

    #[test]
    fn eval_of_parsed_soup_never_panics(s in fragments()) {
        if let Ok(e) = parse(&s) {
            let _ = eval(&e, &scope());
        }
    }

    #[test]
    fn eval_of_parsed_expressions_never_panics(s in expressions()) {
        if let Ok(e) = parse(&s) {
            let _ = eval(&e, &scope());
        }
        if let Ok(t) = parse_template(&format!("a {{{s}}} b")) {
            let _ = t.eval(&scope());
        }
    }
}

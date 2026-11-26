//! Shaping, breaking, hit-testing and glyph runs on the bundled fonts only (design 24
//! decision 10): the same numbers on every machine.

use std::sync::Arc;

use gogo_ui_ir::{TextAlign, TextStyle};
use gogo_ui_text::{FontSet, TextLayout};

fn style(size: f32) -> TextStyle {
    TextStyle {
        family: Arc::from("Noto Sans"),
        size,
        ..TextStyle::default()
    }
}

fn layout(text: &str, st: &TextStyle) -> TextLayout {
    FontSet::bundled().layout(text, st)
}

#[test]
fn a_run_measures_wider_with_more_text_and_a_bigger_size() {
    let (w1, h1) = layout("Play", &style(14.0)).size();
    let (w2, _) = layout("Play game", &style(14.0)).size();
    let (w3, h3) = layout("Play", &style(28.0)).size();
    assert!(w1 > 10.0 && h1 > 10.0, "{w1} {h1}");
    assert!(w2 > w1);
    assert!(w3 > w1 * 1.8 && h3 > h1 * 1.8);
}

#[test]
fn measuring_is_deterministic_and_does_not_depend_on_the_machine() {
    // Bundled Noto Sans 2.015, 14 px: pinned so a font swap shows up.
    let (w, h) = layout("Hello, world", &style(14.0)).size();
    assert!((w - 78.83).abs() < 0.1, "width {w}");
    assert!((h - 19.0).abs() < 2.0, "height {h}");
    let again = layout("Hello, world", &style(14.0)).size();
    assert_eq!((w, h), again);
}

#[test]
fn text_without_wrap_stays_one_line() {
    let mut t = layout("one two three four five six seven", &style(14.0));
    t.break_lines(Some(40.0));
    assert_eq!(t.line_count(), 1);
}

#[test]
fn wrapped_text_breaks_at_words_within_the_width() {
    let st = TextStyle {
        wrap: true,
        ..style(14.0)
    };
    let mut t = layout("one two three four five six seven eight", &st);
    t.break_lines(Some(90.0));
    assert!(t.line_count() >= 3, "{}", t.line_count());
    assert!(t.width() <= 90.0 + 0.01, "{}", t.width());
    for l in t.lines() {
        let s = &t.text()[l.text.clone()];
        assert!(!s.trim_start().starts_with(char::is_whitespace));
    }
    let h3 = t.height();
    t.break_lines(Some(1000.0));
    assert_eq!(t.line_count(), 1);
    assert!(t.height() < h3);
}

#[test]
fn a_width_of_zero_gives_one_word_per_line() {
    let st = TextStyle {
        wrap: true,
        ..style(14.0)
    };
    let mut t = layout("aa bb cc", &st);
    t.break_lines(Some(0.0));
    assert_eq!(t.line_count(), 3);
}

#[test]
fn max_lines_keeps_that_many_lines() {
    let st = TextStyle {
        wrap: true,
        max_lines: 2,
        ..style(14.0)
    };
    let text = "one two three four five six seven eight nine ten eleven twelve";
    let mut fonts = FontSet::bundled();
    let mut t = fonts.layout(text, &st);
    t.break_lines(Some(90.0));
    assert_eq!(t.line_count(), 2);
    assert!(t.is_truncated());
    assert_eq!(t.lines().len(), 2);
    let two = t.height();
    let mut free = fonts.layout(
        text,
        &TextStyle {
            max_lines: 0,
            ..st.clone()
        },
    );
    free.break_lines(Some(90.0));
    assert!(free.height() > two);
    // Only the glyphs of the kept lines are drawn.
    let drawn: usize = t.runs().iter().map(|r| r.glyphs.len()).sum();
    let all: usize = free.runs().iter().map(|r| r.glyphs.len()).sum();
    assert!(drawn < all);
}

#[test]
fn ellipsize_ends_the_last_kept_line_in_an_ellipsis_within_the_width() {
    let st = TextStyle {
        wrap: true,
        max_lines: 2,
        ..style(14.0)
    };
    let text = "one two three four five six seven eight nine ten eleven twelve";
    let mut fonts = FontSet::bundled();
    let mut t = fonts.layout(text, &st);
    t.ellipsize(&mut fonts, Some(90.0));
    assert!(t.is_truncated());
    assert!(t.text().ends_with('…'), "{:?}", t.text());
    assert!(t.text().len() < text.len() + 3);
    assert_eq!(t.line_count(), 2);
    assert!(t.width() <= 90.0 + 0.01);
    // A wider box needs no ellipsis, and the original text comes back.
    t.ellipsize(&mut fonts, Some(2000.0));
    assert!(!t.is_truncated());
    assert_eq!(t.text(), text);
}

#[test]
fn ellipsize_leaves_text_that_fits_alone() {
    let st = TextStyle {
        wrap: true,
        max_lines: 2,
        ..style(14.0)
    };
    let mut fonts = FontSet::bundled();
    let mut t = fonts.layout("short", &st);
    t.ellipsize(&mut fonts, Some(200.0));
    assert!(!t.is_truncated());
    assert_eq!(t.text(), "short");
}

#[test]
fn alignment_moves_the_line_inside_the_width() {
    let mut left = layout("Hi", &style(14.0));
    left.break_lines(Some(200.0));
    let mut centre = layout(
        "Hi",
        &TextStyle {
            align: TextAlign::Center,
            ..style(14.0)
        },
    );
    centre.break_lines(Some(200.0));
    let mut right = layout(
        "Hi",
        &TextStyle {
            align: TextAlign::End,
            ..style(14.0)
        },
    );
    right.break_lines(Some(200.0));
    let x = |t: &TextLayout| t.lines()[0].left;
    assert!(x(&left) < 1.0);
    assert!(x(&centre) > 80.0 && x(&centre) < 120.0, "{}", x(&centre));
    assert!(x(&right) > 160.0, "{}", x(&right));
}

#[test]
fn hit_testing_maps_points_to_byte_offsets() {
    let mut t = layout("abc def", &style(20.0));
    t.break_lines(None);
    let (w, h) = t.size();
    assert_eq!(t.hit(-5.0, h / 2.0), 0);
    assert_eq!(t.hit(w + 50.0, h / 2.0), 7);
    let mid = t.hit(w / 2.0, h / 2.0);
    assert!((2..=5).contains(&mid), "{mid}");
    // Offsets always sit on a char boundary, even with multi-byte text.
    let t = layout("héllo wörld", &style(20.0));
    for i in 0..40 {
        let o = t.hit(i as f32 * 5.0, 5.0);
        assert!(t.text().is_char_boundary(o), "{o}");
    }
}

#[test]
fn hit_testing_picks_the_line_by_y() {
    let st = TextStyle {
        wrap: true,
        ..style(14.0)
    };
    let mut t = layout("aaaa bbbb cccc", &st);
    t.break_lines(Some(40.0));
    assert!(t.line_count() >= 2);
    let lines = t.lines();
    let last = lines.last().unwrap();
    let o = t.hit(1.0, (last.top + last.bottom) / 2.0);
    assert_eq!(o, last.text.start);
}

#[test]
fn glyph_runs_carry_the_font_size_and_variation_coords() {
    let t = layout(
        "Hello",
        &TextStyle {
            weight: 700.0,
            ..style(16.0)
        },
    );
    let runs = t.runs();
    assert_eq!(runs.len(), 1);
    let r = &runs[0];
    assert_eq!(r.size, 16.0);
    assert_eq!(r.glyphs.len(), 5);
    assert!(!r.coords.is_empty(), "a variable font has coords");
    assert!(r.glyphs.windows(2).all(|g| g[0].x < g[1].x));
    // The baseline is the same for every glyph of a line.
    assert!(r.glyphs.iter().all(|g| g.y == r.glyphs[0].y));
    assert!(r.glyphs[0].y > 8.0 && r.glyphs[0].y < 24.0);
}

#[test]
fn weight_and_width_change_the_shapes() {
    let w = |weight: f32, width: f32| {
        layout(
            "Library",
            &TextStyle {
                weight,
                width,
                ..style(16.0)
            },
        )
        .width()
    };
    assert!(w(800.0, 100.0) > w(400.0, 100.0), "bold is wider");
    assert!(w(400.0, 75.0) < w(400.0, 100.0), "condensed is narrower");
}

#[test]
fn tracking_and_line_height_apply() {
    let base = layout("Library", &style(16.0));
    let tracked = layout(
        "Library",
        &TextStyle {
            tracking: 0.1,
            ..style(16.0)
        },
    );
    assert!(tracked.width() > base.width() + 7.0 * 1.0);
    let tall = layout(
        "Library",
        &TextStyle {
            line_height: Some(2.0),
            ..style(16.0)
        },
    );
    assert!((tall.height() - 32.0).abs() < 0.5, "{}", tall.height());
}

#[test]
fn the_mono_family_is_monospaced_and_unknown_families_get_the_default() {
    let mono = |s: &str| {
        layout(
            s,
            &TextStyle {
                family: Arc::from("JetBrains Mono"),
                ..style(14.0)
            },
        )
        .width()
    };
    assert!((mono("iiii") - mono("MMMM")).abs() < 0.01);
    let unknown = layout(
        "Hello",
        &TextStyle {
            family: Arc::from("Nope"),
            ..style(14.0)
        },
    )
    .width();
    let noto = layout("Hello", &style(14.0)).width();
    assert_eq!(unknown, noto);
    let empty = layout(
        "Hello",
        &TextStyle {
            family: Arc::from(""),
            ..style(14.0)
        },
    )
    .width();
    assert_eq!(empty, noto);
}

#[test]
fn a_glyph_the_run_family_lacks_falls_back_to_another_bundled_family() {
    // Arrows and `·` come from whichever bundled family has them: no missing-glyph id (0).
    let t = layout(
        "a → b · c",
        &TextStyle {
            family: Arc::from("JetBrains Mono"),
            ..style(14.0)
        },
    );
    for r in t.runs() {
        assert!(r.glyphs.iter().all(|g| g.id != 0), "{r:?}");
    }
}

#[test]
fn empty_text_has_no_runs_and_no_width() {
    let t = layout("", &style(14.0));
    assert!(t.runs().is_empty());
    assert_eq!(t.width(), 0.0);
    assert!(
        t.height() > 0.0,
        "an empty line still has the font's height"
    );
    assert_eq!(t.hit(5.0, 5.0), 0);
}

#[test]
fn explicit_newlines_break_even_without_wrap() {
    let t = layout("a\nb", &style(14.0));
    assert_eq!(t.line_count(), 2);
}

#[test]
fn the_font_set_is_reusable_across_layouts() {
    let mut fonts = FontSet::bundled();
    let a = fonts.layout("Play", &style(14.0)).width();
    let _ = fonts.layout("Something else entirely", &style(30.0));
    let b = fonts.layout("Play", &style(14.0)).width();
    assert_eq!(a, b);
}

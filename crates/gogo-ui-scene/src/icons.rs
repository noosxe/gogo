//! The icons the engine ships (design 27 §5.2: `play`, `square`, `circle`, `chevron-down`,
//! `dot`). This crate owns the list; the skin loader keeps its own copy of the names, and
//! the testkit checks the two agree.
//!
//! Each icon is a path in a 24 × 24 box, drawn scaled to fit the icon node's box (centred
//! when the box is not square). It is either filled or stroked with its own width, in the
//! node's solid fill colour. An unknown name has no path: [`path`] returns `None` and the
//! scene draws nothing.

use kurbo::{BezPath, Stroke, StrokeOpts};

/// The side of the box every icon path is drawn in.
pub const VIEWBOX: f64 = 24.0;

/// A path segment, absolute, in viewbox units.
#[derive(Debug, Clone, Copy)]
enum Seg {
    M(f64, f64),
    L(f64, f64),
    C(f64, f64, f64, f64, f64, f64),
    Z,
}

struct Def {
    name: &'static str,
    segs: &'static [Seg],
    /// Stroke width in viewbox units; `None` fills the path.
    stroke: Option<f64>,
}

use Seg::{C, L, M, Z};

const DEFS: &[Def] = &[
    Def {
        name: "square",
        segs: &[M(5.0, 5.0), L(19.0, 5.0), L(19.0, 19.0), L(5.0, 19.0), Z],
        stroke: Some(2.0),
    },
    Def {
        // Four cubic quarter-arcs, radius 7.
        name: "circle",
        segs: &[
            M(12.0, 5.0),
            C(15.866, 5.0, 19.0, 8.134, 19.0, 12.0),
            C(19.0, 15.866, 15.866, 19.0, 12.0, 19.0),
            C(8.134, 19.0, 5.0, 15.866, 5.0, 12.0),
            C(5.0, 8.134, 8.134, 5.0, 12.0, 5.0),
            Z,
        ],
        stroke: Some(2.0),
    },
    Def {
        name: "play",
        segs: &[M(8.0, 5.0), L(19.0, 12.0), L(8.0, 19.0), Z],
        stroke: None,
    },
    Def {
        name: "chevron-down",
        segs: &[M(6.0, 9.0), L(12.0, 15.0), L(18.0, 9.0)],
        stroke: Some(2.0),
    },
    Def {
        // A filled disc, radius 4.
        name: "dot",
        segs: &[
            M(16.0, 12.0),
            C(16.0, 14.209, 14.209, 16.0, 12.0, 16.0),
            C(9.791, 16.0, 8.0, 14.209, 8.0, 12.0),
            C(8.0, 9.791, 9.791, 8.0, 12.0, 8.0),
            C(14.209, 8.0, 16.0, 9.791, 16.0, 12.0),
            Z,
        ],
        stroke: None,
    },
];

/// The icon names, in the order of design 27 §5.2.
pub const NAMES: &[&str] = &["play", "square", "circle", "chevron-down", "dot"];

/// Whether the engine has an icon called `name` (exact, case-sensitive).
pub fn exists(name: &str) -> bool {
    DEFS.iter().any(|d| d.name == name)
}

/// The outline of icon `name` drawn into the box `(x, y, w, h)` in logical px, ready to
/// fill, or `None` for an unknown name or an empty box. Stroked icons are already expanded
/// to filled outlines.
pub fn path(name: &str, x: f64, y: f64, w: f64, h: f64) -> Option<BezPath> {
    let def = DEFS.iter().find(|d| d.name == name)?;
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let k = w.min(h) / VIEWBOX;
    let (ox, oy) = (x + (w - VIEWBOX * k) / 2.0, y + (h - VIEWBOX * k) / 2.0);
    let pt = |px: f64, py: f64| (ox + px * k, oy + py * k);
    let mut p = BezPath::new();
    for s in def.segs {
        match *s {
            M(a, b) => p.move_to(pt(a, b)),
            L(a, b) => p.line_to(pt(a, b)),
            C(a, b, c, d, e, f) => p.curve_to(pt(a, b), pt(c, d), pt(e, f)),
            Z => p.close_path(),
        }
    }
    Some(match def.stroke {
        Some(width) => kurbo::stroke(
            p.iter(),
            &Stroke::new(width * k),
            &StrokeOpts::default(),
            0.05,
        ),
        None => p,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape;

    #[test]
    fn the_name_list_matches_the_definitions() {
        let defs: Vec<&str> = DEFS.iter().map(|d| d.name).collect();
        let mut a = defs.clone();
        let mut b = NAMES.to_vec();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
        for n in NAMES {
            assert!(exists(n), "{n}");
        }
    }

    #[test]
    fn every_icon_has_a_path_inside_its_box() {
        for n in NAMES {
            let p = path(n, 10.0, 20.0, 24.0, 24.0).expect(n);
            let b = p.bounding_box();
            assert!(b.width() > 3.0 && b.height() > 3.0, "{n}: {b:?}");
            assert!(
                b.x0 >= 9.0 && b.y0 >= 19.0 && b.x1 <= 35.0 && b.y1 <= 45.0,
                "{n}: {b:?}"
            );
        }
    }

    #[test]
    fn an_icon_scales_with_its_box_and_centres_in_a_wider_one() {
        let small = path("play", 0.0, 0.0, 12.0, 12.0)
            .expect("play")
            .bounding_box();
        let big = path("play", 0.0, 0.0, 24.0, 24.0)
            .expect("play")
            .bounding_box();
        assert!((big.width() - 2.0 * small.width()).abs() < 1e-9);
        let wide = path("play", 0.0, 0.0, 100.0, 24.0)
            .expect("play")
            .bounding_box();
        assert!((wide.x0 - (38.0 + 8.0)).abs() < 1e-9, "{wide:?}");
    }

    #[test]
    fn an_unknown_name_or_an_empty_box_has_no_path() {
        assert!(path("nope", 0.0, 0.0, 24.0, 24.0).is_none());
        assert!(path("", 0.0, 0.0, 24.0, 24.0).is_none());
        assert!(
            path("Play", 0.0, 0.0, 24.0, 24.0).is_none(),
            "names are case-sensitive"
        );
        assert!(path("play", 0.0, 0.0, 0.0, 24.0).is_none());
        assert!(path("play", 0.0, 0.0, f64::NAN, 24.0).is_none());
        assert!(!exists("nope"));
    }

    #[test]
    fn a_stroked_icon_is_an_outline_and_a_filled_one_is_the_path_itself() {
        let sq = path("square", 0.0, 0.0, 24.0, 24.0).expect("square");
        // Stroke expansion makes an outer and an inner loop: area of the ring, not the box.
        assert!(sq.area().abs() < 14.0 * 14.0, "{}", sq.area());
        let play = path("play", 0.0, 0.0, 24.0, 24.0).expect("play");
        assert!((play.area().abs() - 0.5 * 14.0 * 11.0).abs() < 1e-9);
    }
}

//! The geometry every renderer needs the same way (design 28 §2), so they cannot drift.

use kurbo::{Affine, Rect, RoundedRectRadii};

use crate::Fit;

/// The largest blur sigma, in device px, a filter layer is given.
const MAX_DEVICE_SIGMA: f64 = 256.0;

/// The sigma, in logical px, to give a filter layer at `scale`: at most 256 device px.
///
/// A filter layer is padded by 3 sigma on each side, and the skin format allows radii in the
/// thousands, which would ask for gigabytes. A blur this wide is a flat wash either way.
pub fn filter_sigma(sigma: f64, scale: f64) -> f64 {
    sigma.min(MAX_DEVICE_SIGMA / scale).max(0.0)
}

/// The one corner radius of a blurred rounded rect: the largest of the four, since vello's
/// blurred rect takes only one (design 28 §5).
pub fn blur_radius(radii: RoundedRectRadii) -> f64 {
    radii
        .top_left
        .max(radii.top_right)
        .max(radii.bottom_right)
        .max(radii.bottom_left)
}

/// Where an image of `width` by `height` px goes in the box `rect` (design 27 §5.2): the
/// transform from image px to logical px, centred, and the part of the box it paints. The
/// overflow of `Cover` is cut. `None` when the box or the image has no area.
pub fn image_placement(rect: Rect, fit: Fit, width: u32, height: u32) -> Option<(Affine, Rect)> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || width == 0 || height == 0 {
        return None;
    }
    let (w, h) = (f64::from(width), f64::from(height));
    let (sx, sy) = (rect.width() / w, rect.height() / h);
    let (sx, sy) = match fit {
        Fit::Contain => (sx.min(sy), sx.min(sy)),
        Fit::Fill => (sx, sy),
        // `Fit` is non-exhaustive; cover is its default.
        _ => (sx.max(sy), sx.max(sy)),
    };
    let (dw, dh) = (w * sx, h * sy);
    let x = rect.x0 + (rect.width() - dw) / 2.0;
    let y = rect.y0 + (rect.height() - dh) / 2.0;
    Some((
        Affine::translate((x, y)) * Affine::scale_non_uniform(sx, sy),
        Rect::new(x, y, x + dw, y + dh).intersect(rect),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: Rect = Rect::new(10.0, 20.0, 110.0, 70.0);

    fn place(fit: Fit) -> (Affine, Rect) {
        // A square image in a box twice as wide as high.
        image_placement(BOX, fit, 10, 10).unwrap_or_else(|| unreachable!())
    }

    #[test]
    fn cover_fills_the_box_and_centres_the_overflow() {
        let (t, r) = place(Fit::Cover);
        assert_eq!(r, BOX);
        assert_eq!(t.as_coeffs(), [10.0, 0.0, 0.0, 10.0, 10.0, -5.0]);
    }

    #[test]
    fn contain_stays_inside_and_fill_stretches() {
        assert_eq!(place(Fit::Contain).1, Rect::new(35.0, 20.0, 85.0, 70.0));
        let (t, r) = place(Fit::Fill);
        assert_eq!(r, BOX);
        assert_eq!(t.as_coeffs(), [10.0, 0.0, 0.0, 5.0, 10.0, 20.0]);
    }

    #[test]
    fn a_filter_sigma_is_capped_in_device_px() {
        assert_eq!(filter_sigma(6.0, 2.0), 6.0);
        assert_eq!(filter_sigma(5000.0, 2.0), 128.0);
        assert_eq!(filter_sigma(f64::NAN, 1.0), 256.0);
    }

    #[test]
    fn nothing_for_no_area_and_the_largest_corner_for_a_blur() {
        assert_eq!(image_placement(Rect::ZERO, Fit::Cover, 1, 1), None);
        assert_eq!(image_placement(BOX, Fit::Cover, 0, 1), None);
        assert_eq!(blur_radius(RoundedRectRadii::new(2.0, 9.0, 1.0, 0.0)), 9.0);
    }
}

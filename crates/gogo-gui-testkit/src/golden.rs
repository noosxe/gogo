//! Comparing renders: against a committed PNG, or two renderers against each other
//! (design 08 §7).
//!
//! Pixels differ a little between architectures, SIMD levels and renderers, so a comparison
//! has a [`Tolerance`]. A pixel's difference is the **largest difference of its four
//! straight RGBA channels**.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use gogo_ui_scene::Image;

/// How far two renders may differ.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tolerance {
    /// A pixel differing by more than this many levels counts as differing.
    pub level: u8,
    /// The share of pixels (0 to 1) that may differ by more than `level`.
    pub share: f64,
    /// No pixel may differ by more than this. It catches a small local change that a share
    /// of the whole image would hide.
    pub max: u8,
    /// The mean pixel difference may be at most this.
    pub mean: f64,
}

/// How far two renders of one size are apart under a [`Tolerance`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Diff {
    /// The share of pixels differing by more than the tolerance's level.
    pub share: f64,
    /// The largest pixel difference.
    pub max: u8,
    /// The mean pixel difference.
    pub mean: f64,
}

impl Diff {
    /// Whether the difference is within `t`.
    pub fn within(&self, t: &Tolerance) -> bool {
        self.share <= t.share && self.max <= t.max && self.mean <= t.mean
    }
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:.4}% of pixels over the level, largest difference {}, mean {:.4}",
            self.share * 100.0,
            self.max,
            self.mean
        )
    }
}

fn pixel_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0)
}

/// The difference of two images, or `None` when their sizes differ.
pub fn diff(a: &Image, b: &Image, level: u8) -> Option<Diff> {
    if (a.width, a.height) != (b.width, b.height) {
        return None;
    }
    let (mut over, mut max, mut sum, mut n) = (0u64, 0u8, 0u64, 0u64);
    for (a, b) in a
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.rgba.as_chunks::<4>().0.iter())
    {
        let d = pixel_diff(a, b);
        over += u64::from(d > level);
        max = max.max(d);
        sum += u64::from(d);
        n += 1;
    }
    let n = n.max(1) as f64;
    Some(Diff {
        share: over as f64 / n,
        max,
        mean: sum as f64 / n,
    })
}

/// `actual` dimmed, with the pixels that differ from `want` by more than `level` in red.
fn diff_picture(want: &Image, actual: &Image, level: u8) -> Option<Image> {
    let rgba = want
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(actual.rgba.as_chunks::<4>().0.iter())
        .flat_map(|(w, a)| {
            if pixel_diff(w, a) > level {
                [255, 0, 0, 255]
            } else {
                [a[0] / 4, a[1] / 4, a[2] / 4, 255]
            }
        })
        .collect();
    Image::new(actual.width, actual.height, rgba)
}

/// Why a render could not be compared or does not match.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GoldenError {
    /// A file could not be read or written.
    #[error("{path}: {message}")]
    File {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// The sizes differ.
    #[error("{name}: the size is {actual:?}, the golden's {want:?}")]
    Size {
        /// The golden's name.
        name: String,
        /// The render's size.
        actual: (u32, u32),
        /// The golden's size.
        want: (u32, u32),
    },
    /// The pixels differ by more than the tolerance.
    #[error(
        "{name}: {diff}; the render and a diff picture are in {out}. \
         Rerun with GOGO_BLESS=1 if the change is intended"
    )]
    Mismatch {
        /// The golden's name.
        name: String,
        /// How far apart they are.
        diff: Diff,
        /// Where the render and the diff picture were written.
        out: PathBuf,
    },
}

fn file_error(path: &Path, e: impl std::fmt::Display) -> GoldenError {
    GoldenError::File {
        path: path.to_owned(),
        message: e.to_string(),
    }
}

/// Writes `image` as a PNG, without the alpha channel when every pixel is opaque.
///
/// # Errors
/// [`GoldenError::File`].
pub fn save_png(path: &Path, image: &Image) -> Result<(), GoldenError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| file_error(dir, e))?;
    }
    let file = File::create(path).map_err(|e| file_error(path, e))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), image.width, image.height);
    let opaque = image.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
    enc.set_color(if opaque {
        png::ColorType::Rgb
    } else {
        png::ColorType::Rgba
    });
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    let rgb: Vec<u8>;
    let data: &[u8] = if opaque {
        rgb = image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        &rgb
    } else {
        &image.rgba
    };
    let mut writer = enc.write_header().map_err(|e| file_error(path, e))?;
    writer
        .write_image_data(data)
        .map_err(|e| file_error(path, e))?;
    writer.finish().map_err(|e| file_error(path, e))
}

/// Reads an 8-bit RGB or RGBA PNG.
///
/// # Errors
/// [`GoldenError::File`].
pub fn load_png(path: &Path) -> Result<Image, GoldenError> {
    let file = File::open(path).map_err(|e| file_error(path, e))?;
    let mut reader = png::Decoder::new(BufReader::new(file))
        .read_info()
        .map_err(|e| file_error(path, e))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| file_error(path, "too large"))?;
    let mut buf = vec![0; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| file_error(path, e))?;
    buf.truncate(info.buffer_size());
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf,
        (png::ColorType::Rgb, png::BitDepth::Eight) => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        _ => return Err(file_error(path, "not an 8-bit RGB or RGBA PNG")),
    };
    Image::new(info.width, info.height, rgba).ok_or_else(|| file_error(path, "empty image"))
}

/// Compares `actual` with the golden `<goldens>/<name>.png`. With `GOGO_BLESS=1`, writes it
/// as the golden instead.
///
/// On a mismatch the render and a diff picture are written to `out`.
///
/// # Errors
/// [`GoldenError`] when the golden is missing or the render differs by more than `t`.
pub fn check(
    goldens: &Path,
    out: &Path,
    name: &str,
    actual: &Image,
    t: &Tolerance,
) -> Result<Diff, GoldenError> {
    let path = goldens.join(format!("{name}.png"));
    if crate::env_flag("GOGO_BLESS") {
        save_png(&path, actual)?;
        return Ok(Diff {
            share: 0.0,
            max: 0,
            mean: 0.0,
        });
    }
    let want = load_png(&path)?;
    compare(out, name, &want, actual, t)
}

/// Compares `actual` with `want`, writing both and a diff picture to `out` on a mismatch.
///
/// # Errors
/// [`GoldenError::Size`] or [`GoldenError::Mismatch`].
pub fn compare(
    out: &Path,
    name: &str,
    want: &Image,
    actual: &Image,
    t: &Tolerance,
) -> Result<Diff, GoldenError> {
    let Some(d) = diff(want, actual, t.level) else {
        save_png(&out.join(format!("{name}.actual.png")), actual)?;
        return Err(GoldenError::Size {
            name: name.to_owned(),
            actual: (actual.width, actual.height),
            want: (want.width, want.height),
        });
    };
    if d.within(t) {
        return Ok(d);
    }
    save_png(&out.join(format!("{name}.actual.png")), actual)?;
    save_png(&out.join(format!("{name}.want.png")), want)?;
    if let Some(picture) = diff_picture(want, actual, t.level) {
        save_png(&out.join(format!("{name}.diff.png")), &picture)?;
    }
    Err(GoldenError::Mismatch {
        name: name.to_owned(),
        diff: d,
        out: out.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Tolerance = Tolerance {
        level: 2,
        share: 0.01,
        max: 8,
        mean: 1.0,
    };

    fn flat(v: u8) -> Vec<u8> {
        vec![v; 100 * 100 * 4]
    }

    fn img(rgba: Vec<u8>) -> Image {
        Image::new(100, 100, rgba).unwrap_or_else(|| unreachable!())
    }

    fn within(change: impl FnOnce(&mut Vec<u8>)) -> bool {
        let mut b = flat(100);
        change(&mut b);
        diff(&img(flat(100)), &img(b), T.level).is_some_and(|d| d.within(&T))
    }

    #[test]
    fn small_differences_everywhere_pass_up_to_the_mean() {
        assert!(within(|_| {}));
        assert!(within(|b| b.fill(101)));
        assert!(!within(|b| b.fill(102)), "mean 2 is over 1");
    }

    #[test]
    fn few_pixels_may_differ_more_but_not_many() {
        // 100 pixels are 1% of the image.
        assert!(within(|b| b[..100 * 4].fill(106)));
        assert!(!within(|b| b[..101 * 4].fill(106)));
    }

    #[test]
    fn one_pixel_over_the_maximum_fails() {
        assert!(within(|b| b[2] = 108));
        assert!(!within(|b| b[2] = 109));
        assert!(!within(|b| b[3] = 91), "alpha counts too");
    }

    #[test]
    fn sizes_must_match() {
        let small = Image::new(1, 1, vec![0; 4]).unwrap_or_else(|| unreachable!());
        assert_eq!(diff(&img(flat(0)), &small, 0), None);
    }
}

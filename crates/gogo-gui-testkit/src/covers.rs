//! Procedural covers: fictional art made from an image's key, so no picture of a real game
//! is ever committed (design 08 §7).
//!
//! The pixels use integer arithmetic only, so every machine makes the same ones.

use gogo_ui_scene::{DisplayList, Image, ImageKey, MemoryStore};

/// The handle of [`checker`], a tiny image for showing how a renderer scales up.
pub const CHECKER: &str = "art:checker";

/// A store holding a picture for every handle and asset the list names. Noise tiles come
/// from the store itself.
pub fn store_for(list: &DisplayList) -> MemoryStore {
    let mut store = MemoryStore::new();
    for key in &list.images {
        let name = match key {
            ImageKey::Handle(h) => h,
            ImageKey::Asset(a) => a,
            _ => continue,
        };
        let image = if &**name == CHECKER {
            checker()
        } else {
            cover(name)
        };
        store.insert(key.clone(), image);
    }
    store
}

/// An 8 by 8 black and orange checkerboard.
pub fn checker() -> Image {
    image(8, 8, |x, y| {
        if (x + y) % 2 == 0 {
            [20, 20, 28, 255]
        } else {
            [240, 150, 40, 255]
        }
    })
}

/// A cover for `name`: a diagonal two-colour gradient with a disc and a band, picked by a
/// hash of the name. A name ending in `background` is wide, one ending in `logo` is wide
/// with a transparent surround, and any other is a 3:4 portrait.
pub fn cover(name: &str) -> Image {
    // FNV-1a.
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let byte = |i: u32| ((hash >> (i * 8)) & 0xff) as u32;
    let top = [60 + byte(0) / 2, 40 + byte(1) / 2, 80 + byte(2) / 2];
    let bottom = [byte(3) / 3, byte(4) / 3, 30 + byte(5) / 3];
    let (w, h) = if name.ends_with("background") {
        (320, 180)
    } else if name.ends_with("logo") {
        (200, 80)
    } else {
        (180, 240)
    };
    let logo = name.ends_with("logo");
    let (cx, cy) = (w / 2 + byte(6) % (w / 4), h / 3);
    let r = w.min(h) / 4;
    let band = h * 2 / 3;
    image(w, h, move |x, y| {
        let (dx, dy) = (x.abs_diff(cx), y.abs_diff(cy));
        let disc = dx * dx + dy * dy <= r * r;
        let stripe = y >= band && y < band + h / 10;
        if logo && !disc && !stripe {
            return [0, 0, 0, 0];
        }
        let t = (x + y) * 255 / (w + h - 2);
        let mix = |i: usize| ((top[i] * (255 - t) + bottom[i] * t) / 255) as u8;
        if disc {
            [250, 220 - (byte(7) / 3) as u8, 120, 255]
        } else if stripe {
            [235, 235, 245, 255]
        } else {
            [mix(0), mix(1), mix(2), 255]
        }
    })
}

fn image(w: u32, h: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Image {
    let rgba = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .flat_map(|(x, y)| pixel(x, y))
        .collect();
    match Image::new(w, h, rgba) {
        Some(i) => i,
        // The sides are constants above 0 and the buffer holds w * h pixels.
        None => unreachable!("a procedural image is always valid"),
    }
}

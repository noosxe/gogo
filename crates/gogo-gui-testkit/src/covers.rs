//! The procedural art of [`gogo_gui_model::fixture::art`] as scene images, so no picture of
//! a real game is ever committed (design 08 §7).

use gogo_gui_model::fixture::art::{self, Pixels};
use gogo_ui_scene::{DisplayList, Image, ImageKey, MemoryStore};

pub use gogo_gui_model::fixture::art::CHECKER;

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

/// [`art::checker`] as an image.
pub fn checker() -> Image {
    image(art::checker())
}

/// [`art::cover`] as an image.
pub fn cover(name: &str) -> Image {
    image(art::cover(name))
}

fn image(p: Pixels) -> Image {
    match Image::new(p.width, p.height, p.rgba) {
        Some(i) => i,
        // The art's sides are above 0 and its buffer holds width * height pixels.
        None => unreachable!("a procedural image is always valid"),
    }
}

//! Images by reference: ids in the list, pixels in a store the app fills (design 28 §1, §4).

use std::collections::HashMap;
use std::sync::Arc;

use crate::noise::noise_tile;

/// An index into [`DisplayList::images`](crate::DisplayList::images). Valid for the list
/// that made it; caches that outlive a list key on the [`ImageKey`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageId(pub(crate) u32);

impl ImageId {
    /// The index into the list's image table.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// What an image is, never where its pixels are: no URL and no path (design 25 §7).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageKey {
    /// A handle the app gave the skin's tree, such as cover art.
    Handle(Arc<str>),
    /// A file of the skin package, by its package-relative path.
    Asset(Arc<str>),
    /// The grain tile of a noise effect; [`noise_tile`] makes it from the seed.
    Noise {
        /// The seed of the pattern.
        seed: u32,
    },
}

/// Straight-alpha RGBA8 pixels, row by row from the top-left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Arc<Vec<u8>>,
}

impl Image {
    /// An image from its pixels, or `None` when `rgba` is not `width * height * 4` bytes
    /// or a side is 0.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Image> {
        let len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (width > 0 && height > 0 && rgba.len() == len).then(|| Image {
            width,
            height,
            rgba: Arc::new(rgba),
        })
    }
}

/// Where a renderer finds pixels for an [`ImageKey`]. The app implements it over its
/// decoded covers and the skin's assets; `None` means "not available (yet)", and the
/// renderer draws nothing for that image.
pub trait ImageStore {
    /// The image for `key`.
    fn get(&self, key: &ImageKey) -> Option<Image>;
}

/// A store in memory. [`ImageKey::Noise`] tiles are made on demand, so a store that only
/// holds covers still serves noise effects.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    images: HashMap<ImageKey, Image>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> MemoryStore {
        MemoryStore::default()
    }

    /// Adds or replaces an image.
    pub fn insert(&mut self, key: ImageKey, image: Image) {
        self.images.insert(key, image);
    }
}

impl ImageStore for MemoryStore {
    fn get(&self, key: &ImageKey) -> Option<Image> {
        if let Some(i) = self.images.get(key) {
            return Some(i.clone());
        }
        match key {
            ImageKey::Noise { seed } => Some(noise_tile(*seed)),
            _ => None,
        }
    }
}

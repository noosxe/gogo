//! The renderers' image cache (design 28 §4): what a renderer made of an image, kept
//! across frames under its [`ImageKey`] within a byte budget.

use std::collections::HashMap;

use crate::{ImageKey, ImageStore};

/// The default byte budget of a renderer's image cache.
pub const DEFAULT_IMAGE_BUDGET: usize = 256 * 1024 * 1024;

#[derive(Debug)]
struct Entry<V> {
    value: V,
    bytes: usize,
    used: u64,
}

/// A renderer's own form of the images (a pixmap, a texture), by [`ImageKey`], evicting the
/// least recently used beyond a byte budget.
///
/// Images the current frame uses are never evicted, so a frame that needs more than the
/// budget keeps everything it draws until the next one. A key names one image for as long
/// as it is cached: a store that has new pixels gives them a new key, or the owner of the
/// renderer [clears](ImageCache::clear) the cache (an asset path means another file after a
/// skin change or reload).
#[derive(Debug)]
pub struct ImageCache<V> {
    entries: HashMap<ImageKey, Entry<V>>,
    budget: usize,
    bytes: usize,
    frame: u64,
    tick: u64,
    frame_start: u64,
}

impl<V> ImageCache<V> {
    /// An empty cache holding about `budget` bytes.
    pub fn new(budget: usize) -> ImageCache<V> {
        ImageCache {
            entries: HashMap::new(),
            budget,
            bytes: 0,
            frame: 0,
            tick: 0,
            frame_start: 0,
        }
    }

    /// Starts a frame: what is used from now on is safe until the next [`ImageCache::trim`].
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.frame_start = self.tick + 1;
    }

    /// The cached form of `key`. When it is not cached, `make` builds it from the store's
    /// pixels and says how many bytes it takes. `None` when the store lacks the image or
    /// `make` cannot use it; neither is remembered, so a later frame asks again.
    pub fn get_or_insert(
        &mut self,
        key: &ImageKey,
        store: &dyn ImageStore,
        make: impl FnOnce(crate::Image) -> Option<(V, usize)>,
    ) -> Option<&V> {
        self.tick += 1;
        if !self.entries.contains_key(key) {
            let (value, bytes) = make(store.get(key)?)?;
            self.bytes += bytes;
            self.entries.insert(
                key.clone(),
                Entry {
                    value,
                    bytes,
                    used: 0,
                },
            );
        }
        let e = self.entries.get_mut(key)?;
        e.used = self.tick;
        Some(&e.value)
    }

    /// Evicts least recently used entries until the cache is within budget, keeping what
    /// this frame used.
    pub fn trim(&mut self) {
        if self.bytes <= self.budget {
            return;
        }
        let mut old: Vec<(u64, ImageKey)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.used < self.frame_start)
            .map(|(k, e)| (e.used, k.clone()))
            .collect();
        old.sort_unstable_by_key(|(used, _)| *used);
        for (_, key) in old {
            if self.bytes <= self.budget {
                break;
            }
            if let Some(e) = self.entries.remove(&key) {
                self.bytes -= e.bytes;
            }
        }
    }

    /// Forgets every image.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Whether `key` is cached.
    pub fn contains(&self, key: &ImageKey) -> bool {
        self.entries.contains_key(key)
    }

    /// The number of cached images.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bytes the cached images take.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Image, MemoryStore};

    fn key(n: u32) -> ImageKey {
        ImageKey::Handle(n.to_string().into())
    }

    fn store(n: u32) -> MemoryStore {
        let mut s = MemoryStore::new();
        for i in 0..n {
            s.insert(
                key(i),
                Image::new(1, 1, vec![0; 4]).unwrap_or_else(|| unreachable!()),
            );
        }
        s
    }

    fn touch(c: &mut ImageCache<u32>, s: &MemoryStore, n: u32) -> Option<u32> {
        c.get_or_insert(&key(n), s, |_| Some((n, 10))).copied()
    }

    fn frame(c: &mut ImageCache<u32>, s: &MemoryStore, used: &[u32]) {
        c.begin_frame();
        for &n in used {
            assert_eq!(touch(c, s, n), Some(n));
        }
        c.trim();
    }

    #[test]
    fn entries_outlive_a_frame_that_does_not_use_them_while_within_budget() {
        let (mut c, s) = (ImageCache::new(100), store(3));
        frame(&mut c, &s, &[0, 1]);
        frame(&mut c, &s, &[2]);
        assert_eq!((c.len(), c.bytes()), (3, 30));
    }

    #[test]
    fn the_least_recently_used_goes_first() {
        let (mut c, s) = (ImageCache::new(20), store(3));
        frame(&mut c, &s, &[0]);
        frame(&mut c, &s, &[1]);
        frame(&mut c, &s, &[0]);
        frame(&mut c, &s, &[2]);
        assert!(c.contains(&key(0)) && c.contains(&key(2)) && !c.contains(&key(1)));
        assert_eq!(c.bytes(), 20);
    }

    #[test]
    fn a_frame_keeps_all_it_uses_even_over_budget() {
        let (mut c, s) = (ImageCache::new(10), store(3));
        frame(&mut c, &s, &[0, 1, 2]);
        assert_eq!(c.len(), 3);
        frame(&mut c, &s, &[1]);
        assert_eq!(c.len(), 1);
        assert!(c.contains(&key(1)));
    }

    #[test]
    fn clearing_forgets_everything() {
        let (mut c, s) = (ImageCache::new(100), store(2));
        frame(&mut c, &s, &[0, 1]);
        c.clear();
        assert!(c.is_empty() && c.bytes() == 0);
    }

    #[test]
    fn a_missing_image_is_not_remembered() {
        let mut c: ImageCache<u32> = ImageCache::new(10);
        assert_eq!(touch(&mut c, &store(0), 0), None);
        assert!(c.is_empty());
        assert_eq!(touch(&mut c, &store(1), 0), Some(0));
    }
}

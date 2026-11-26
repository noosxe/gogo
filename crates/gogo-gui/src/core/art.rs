//! Cover art (design 25 §7): fetched when a renderer first asks for it, from GOG's image
//! hosts only and with no credential, then decoded off the UI thread with hard limits.
//! A skin sees a handle, never an address (S3).
//!
//! The bytes are untrusted: the body is cut at [`MAX_BYTES`], the size is checked
//! before anything is decoded, the decoder has an allocation limit, and a panic in it
//! loses one picture.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use gogo_ui_scene::{Image, ImageKey, ImageStore, MemoryStore};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use tokio::runtime::Handle;
use tokio::sync::Semaphore;

use super::engine::Engine;

/// The largest picture body read.
const MAX_BYTES: usize = 8 * 1024 * 1024;
/// The longest side, and the most pixels, of a picture that is decoded at all.
const MAX_SIDE: u32 = 4096;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
/// What one decode may allocate.
const MAX_ALLOC: u64 = 4 * MAX_PIXELS + 16 * 1024 * 1024;
/// Decoded pictures kept in memory.
const BUDGET: usize = 128 * 1024 * 1024;
/// Fetches running at once, and waiting or running at once.
const AT_ONCE: usize = 4;
const MAX_PENDING: usize = 256;

const TIMEOUT: Duration = Duration::from_secs(30);

/// Which picture of a product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Cover,
    Background,
}

impl Kind {
    /// The box a picture is shrunk into: about twice what a skin draws.
    fn bounds(self) -> (u32, u32) {
        match self {
            Kind::Cover => (342, 482),
            Kind::Background => (1280, 720),
        }
    }

    fn word(self) -> &'static str {
        match self {
            Kind::Cover => "cover",
            Kind::Background => "background",
        }
    }
}

type Key = (u64, Kind);

/// `art:{id}:cover` and `art:{id}:background`; nothing else has a picture.
fn parse(handle: &str) -> Option<Key> {
    let (id, kind) = handle.strip_prefix("art:")?.split_once(':')?;
    let kind = match kind {
        "cover" => Kind::Cover,
        "background" => Kind::Background,
        _ => return None,
    };
    Some((id.parse().ok()?, kind))
}

/// Which addresses may be fetched, redirects included.
pub(crate) type Allow = Arc<dyn Fn(&reqwest::Url) -> bool + Send + Sync>;

/// GOG's image hosts over https, on the usual port, with nothing but a path.
pub(crate) fn gog_images(url: &reqwest::Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    // `images.<domain>` and the numbered `images-N.<domain>`.
    let ours = ["gog-statics.com", "gog.com"].iter().any(|domain| {
        host.strip_suffix(domain)
            .and_then(|h| h.strip_suffix('.'))
            .and_then(|h| h.strip_prefix("images"))
            .is_some_and(|n| {
                n.is_empty()
                    || n.strip_prefix('-')
                        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            })
    });
    url.scheme() == "https"
        && ours
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

/// A client for pictures: no credential is ever set on it, and a redirect is followed
/// only where [`Allow`] says.
pub(crate) fn client(allow: Allow) -> reqwest::Result<reqwest::Client> {
    let redirect = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() < 5 && allow(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("redirect refused")
        }
    });
    reqwest::Client::builder()
        .user_agent(concat!("gogo/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .redirect(redirect)
        .build()
}

/// Why there is no picture. It names no address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ArtError {
    #[error("there is none")]
    None,
    #[error("its address is not one of GOG's image hosts")]
    Host,
    #[error("it could not be fetched")]
    Fetch,
    #[error("it is too large")]
    TooLarge,
    #[error("it could not be decoded")]
    Decode,
}

/// Decodes a JPEG or WebP and shrinks it into `bounds`.
pub(crate) fn decode(bytes: &[u8], bounds: (u32, u32)) -> Result<DynamicImage, ArtError> {
    if bytes.len() > MAX_BYTES {
        return Err(ArtError::TooLarge);
    }
    catch_unwind(AssertUnwindSafe(|| decode_bounded(bytes, bounds)))
        .unwrap_or(Err(ArtError::Decode))
}

fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, ArtError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| ArtError::Decode)?;
    if !matches!(reader.format(), Some(ImageFormat::Jpeg | ImageFormat::WebP)) {
        return Err(ArtError::Decode);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    Ok(reader)
}

fn decode_bounded(bytes: &[u8], (w, h): (u32, u32)) -> Result<DynamicImage, ArtError> {
    // From the header alone.
    let (width, height) = reader(bytes)?
        .into_dimensions()
        .map_err(|_| ArtError::Decode)?;
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(ArtError::TooLarge);
    }
    let image = reader(bytes)?.decode().map_err(|_| ArtError::Decode)?;
    Ok(if image.width() > w || image.height() > h {
        image.thumbnail(w, h)
    } else {
        image
    })
}

fn pixels(image: DynamicImage) -> Option<Image> {
    let rgba = image.into_rgba8();
    let (w, h) = rgba.dimensions();
    Image::new(w, h, rgba.into_raw())
}

#[derive(Default)]
struct Inner {
    images: HashMap<Key, Image>,
    /// Least recently asked for first.
    order: VecDeque<Key>,
    bytes: usize,
    pending: HashSet<Key>,
    /// Not asked again until [`ArtStore::forget_failures`].
    failed: HashSet<Key>,
}

impl Inner {
    /// The picture of `key`, which is now the last to be dropped.
    fn get(&mut self, key: Key) -> Option<Image> {
        let image = self.images.get(&key)?.clone();
        if self.order.back() != Some(&key)
            && let Some(at) = self.order.iter().position(|k| *k == key)
        {
            self.order.remove(at);
            self.order.push_back(key);
        }
        Some(image)
    }

    fn insert(&mut self, key: Key, image: Image) {
        self.bytes += image.rgba.len();
        if let Some(old) = self.images.insert(key, image) {
            self.bytes -= old.rgba.len();
        } else {
            self.order.push_back(key);
        }
        self.trim(BUDGET);
    }

    fn trim(&mut self, budget: usize) {
        while self.bytes > budget {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.images.remove(&oldest) {
                self.bytes -= old.rgba.len();
            }
        }
    }
}

struct Shared {
    engine: Arc<Engine>,
    http: reqwest::Client,
    allow: Allow,
    /// Where decoded pictures are kept between runs.
    disk: Option<PathBuf>,
    inner: Mutex<Inner>,
    gate: Semaphore,
    arrived: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
}

/// The pictures the renderers ask for.
#[derive(Clone)]
pub(crate) struct ArtStore {
    shared: Arc<Shared>,
    runtime: Handle,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ArtStore {
    /// `wake` is called from a runtime thread when a picture arrived.
    pub(crate) fn new(
        engine: Arc<Engine>,
        runtime: Handle,
        allow: Allow,
        disk: Option<PathBuf>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> reqwest::Result<Self> {
        Ok(ArtStore {
            shared: Arc::new(Shared {
                engine,
                http: client(Arc::clone(&allow))?,
                allow,
                disk,
                inner: Mutex::default(),
                gate: Semaphore::new(AT_ONCE),
                arrived: AtomicBool::new(false),
                wake: Box::new(wake),
            }),
            runtime,
        })
    }

    /// Whether a picture arrived since the last call.
    pub(crate) fn take_arrived(&self) -> bool {
        self.shared.arrived.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn forget_failures(&self) {
        lock(&self.shared.inner).failed.clear();
    }

    fn request(&self, key: Key) {
        {
            let mut inner = lock(&self.shared.inner);
            if inner.failed.contains(&key)
                || inner.pending.len() >= MAX_PENDING
                || !inner.pending.insert(key)
            {
                return;
            }
        }
        let shared = Arc::clone(&self.shared);
        self.runtime.spawn(async move {
            let read = shared.load(key).await;
            let mut inner = lock(&shared.inner);
            inner.pending.remove(&key);
            match read {
                Ok(image) => {
                    inner.insert(key, image);
                    drop(inner);
                    if !shared.arrived.swap(true, Ordering::AcqRel) {
                        (shared.wake)();
                    }
                }
                Err(e) => {
                    inner.failed.insert(key);
                    if e != ArtError::None {
                        tracing::debug!("a game's {} is not shown: {e}", key.1.word());
                    }
                }
            }
        });
    }
}

impl ImageStore for ArtStore {
    fn get(&self, key: &ImageKey) -> Option<Image> {
        match key {
            ImageKey::Handle(name) => {
                let key = parse(name)?;
                if let Some(image) = lock(&self.shared.inner).get(key) {
                    return Some(image);
                }
                self.request(key);
                None
            }
            ImageKey::Noise { .. } => MemoryStore::new().get(key),
            _ => None,
        }
    }
}

impl Shared {
    async fn load(&self, key: Key) -> Result<Image, ArtError> {
        let _slot = self.gate.acquire().await.map_err(|_| ArtError::Fetch)?;
        let (id, kind) = key;
        let page = self.engine.overview(id).await;
        let address = page.as_ref().as_ref().and_then(|o| match kind {
            Kind::Cover => o.cover.clone(),
            Kind::Background => o.background.clone(),
        });
        // With the page read, only the picture of its present address will do, so a
        // changed one is fetched. Without it (offline), any kept one is better than
        // none.
        let offline = page.is_none();
        let prefix = format!("{id}-{}-", kind.word());
        let file = self
            .disk
            .as_ref()
            .zip(address.as_deref())
            .map(|(dir, a)| dir.join(format!("{prefix}{:016x}.jpg", fnv1a(a))));
        let kept_in = self.disk.clone().filter(|_| offline);
        let (wanted, any) = (file.clone(), prefix.clone());
        let kept = tokio::task::spawn_blocking(move || {
            let file = wanted.or_else(|| kept_of(&kept_in?, &any).next())?;
            read_kept(&file, kind)
        })
        .await;
        // An unreadable cache is ignored.
        if let Ok(Some(image)) = kept {
            return Ok(image);
        }
        let bytes = self.fetch(&address.ok_or(ArtError::None)?).await?;
        tokio::task::spawn_blocking(move || {
            let image = decode(&bytes, kind.bounds())?;
            if let Some(file) = &file {
                keep(file, &prefix, &image);
            }
            pixels(image).ok_or(ArtError::Decode)
        })
        .await
        .map_err(|_| ArtError::Decode)?
    }

    async fn fetch(&self, address: &str) -> Result<Vec<u8>, ArtError> {
        let url = reqwest::Url::parse(address).map_err(|_| ArtError::Host)?;
        if !(self.allow)(&url) {
            return Err(ArtError::Host);
        }
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|_| ArtError::Fetch)?;
        if !response.status().is_success() {
            return Err(ArtError::Fetch);
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
        {
            return Err(ArtError::TooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ArtError::Fetch)? {
            if body.len() + chunk.len() > MAX_BYTES {
                return Err(ArtError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// FNV-1a: a name for an address that stays the same from build to build.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The kept pictures whose names start with `prefix`.
fn kept_of(dir: &Path, prefix: &str) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(move |path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix) && n.ends_with(".jpg"))
        })
}

/// A picture from the disk cache, decoded with the same limits as a fetched one.
fn read_kept(file: &Path, kind: Kind) -> Option<Image> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .ok()?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    pixels(decode(&bytes, kind.bounds()).ok()?)
}

/// Writes the shrunk picture, atomically, and drops the ones of earlier addresses.
/// Best effort.
fn keep(file: &Path, prefix: &str, image: &DynamicImage) {
    let Some(dir) = file.parent() else { return };
    let mut bytes = Cursor::new(Vec::new());
    if DynamicImage::ImageRgb8(image.to_rgb8())
        .write_to(&mut bytes, ImageFormat::Jpeg)
        .is_err()
    {
        return;
    }
    let tmp = file.with_extension("tmp");
    let written = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&tmp, bytes.get_ref()))
        .and_then(|()| std::fs::rename(&tmp, file));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        tracing::debug!("a picture could not be kept in the cache");
        return;
    }
    for old in kept_of(dir, prefix) {
        if old != file {
            let _ = std::fs::remove_file(old);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).expect("a URL")
    }

    pub(crate) fn encoded(w: u32, h: u32, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(w, h)
            .write_to(&mut out, format)
            .expect("an encoder");
        out.into_inner()
    }

    #[test]
    fn only_gogs_image_hosts_are_fetched() {
        for ok in [
            "https://images.gog-statics.com/a.jpg",
            "https://images-3.gog-statics.com/a.jpg",
            "https://images-1.gog.com/a.jpg",
        ] {
            assert!(gog_images(&url(ok)), "{ok}");
        }
        for no in [
            "http://images.gog-statics.com/a.jpg",
            "https://images.gog-statics.com:8443/a.jpg",
            "https://user@images.gog-statics.com/a.jpg",
            "https://evil.example/images.gog-statics.com/a.jpg",
            "https://images.gog-statics.com.evil.example/a.jpg",
            "https://evilimages.gog.com/a.jpg",
            "https://images-.gog.com/a.jpg",
            "https://images-x.gog.com/a.jpg",
            "https://images-1.evilgog.com/a.jpg",
            "https://www.gog.com/a.jpg",
            "https://127.0.0.1/a.jpg",
            "file:///etc/passwd",
        ] {
            assert!(!gog_images(&url(no)), "{no}");
        }
    }

    #[test]
    fn handles_name_a_cover_or_a_background_only() {
        assert_eq!(parse("art:7:cover"), Some((7, Kind::Cover)));
        assert_eq!(parse("art:7:background"), Some((7, Kind::Background)));
        for no in [
            "art:7:logo",
            "art:x:cover",
            "art:7",
            "cover",
            "art:../7:cover",
        ] {
            assert_eq!(parse(no), None, "{no}");
        }
    }

    #[test]
    fn the_picture_dropped_is_the_one_asked_for_longest_ago() {
        let image = Image::new(1, 1, vec![0; 4]).expect("a pixel");
        let mut inner = Inner::default();
        for id in 1..=3 {
            inner.insert((id, Kind::Cover), image.clone());
        }
        assert!(inner.get((1, Kind::Cover)).is_some());
        inner.trim(8);
        assert!(inner.get((2, Kind::Cover)).is_none());
        assert!(inner.get((1, Kind::Cover)).is_some());
        assert!(inner.get((3, Kind::Cover)).is_some());
    }

    #[test]
    fn a_new_address_replaces_the_kept_picture() {
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_ne!(fnv1a("a"), fnv1a("b"));
        let dir = crate::core::tests::scratch();
        let image = DynamicImage::new_rgb8(2, 2);
        let name = |a: &str| dir.path().join(format!("7-cover-{:016x}.jpg", fnv1a(a)));
        let other = dir.path().join("70-cover-0.jpg");
        std::fs::write(&other, "another game's").expect("a write");
        keep(&name("old"), "7-cover-", &image);
        keep(&name("new"), "7-cover-", &image);
        let kept: Vec<_> = kept_of(dir.path(), "7-cover-").collect();
        assert_eq!(kept, [name("new")]);
        assert!(other.is_file());
    }

    #[test]
    fn a_picture_is_shrunk_into_its_box() {
        for format in [ImageFormat::Jpeg, ImageFormat::WebP] {
            let image = decode(&encoded(684, 964, format), (342, 482)).expect("decodes");
            assert_eq!((image.width(), image.height()), (342, 482));
        }
    }

    #[test]
    fn hostile_bytes_are_refused_without_panicking() {
        let bounds = Kind::Cover.bounds();
        // A header claiming more than the limits, before any pixel is read.
        let wide = encoded(MAX_SIDE + 1, 1, ImageFormat::Jpeg);
        assert!(matches!(
            decode(&wide, bounds),
            Err(ArtError::TooLarge | ArtError::Decode)
        ));
        let huge = vec![0u8; MAX_BYTES + 1];
        assert_eq!(decode(&huge, bounds).err(), Some(ArtError::TooLarge));
        // Other formats are not decoded at all.
        let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
        assert_eq!(decode(gif, bounds).err(), Some(ArtError::Decode));
        assert_eq!(decode(b"", bounds).err(), Some(ArtError::Decode));
        // Every truncation, and bytes flipped all over, of a real picture.
        for format in [ImageFormat::Jpeg, ImageFormat::WebP] {
            let good = encoded(64, 64, format);
            for end in 0..good.len() {
                let _ = decode(&good[..end], bounds);
            }
            for at in 0..good.len() {
                let mut bad = good.clone();
                bad[at] ^= 0xff;
                let _ = decode(&bad, bounds);
            }
        }
    }
}

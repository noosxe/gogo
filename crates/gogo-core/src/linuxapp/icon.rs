//! The game's menu icon (design 18 §7.1): `games/<gogid>/icon.png`, from
//! GOG's `goggame-<gogid>.ico` in the game dir, else the exe's own icon
//! (its first `RT_GROUP_ICON`). The largest image wins (then the deepest
//! colour). An icon image is a PNG already (Vista-style 256 px entries) or
//! a DIB (`BITMAPINFOHEADER`, 1–32 bpp, with its AND mask), which is
//! decoded and written as an RGBA PNG.
//!
//! No image crate: the PNG is one IDAT through `flate2` (already a
//! dependency), with `flate2`'s CRC-32.

use std::io::Write as _;
use std::path::Path;

use object::read::pe::{ImageNtHeaders, PeFile, ResourceDirectory, ResourceDirectoryEntryData};
use object::{FileKind, LittleEndian as LE, pe};

const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
/// Bigger than any icon: a corrupt header can't make us allocate gigabytes.
const MAX_SIDE: u32 = 1024;

/// One image of an icon: its size and depth as far as the directory (or
/// the image itself) says, and its bytes.
#[derive(Debug, Clone, Copy)]
struct Image<'a> {
    width: u32,
    height: u32,
    bpp: u16,
    data: &'a [u8],
}

impl Image<'_> {
    /// The real size: a PNG's IHDR, a DIB's header; the directory's is a
    /// hint (0 means 256).
    fn size(&self) -> (u32, u32) {
        if self.data.starts_with(PNG_SIG) && self.data.len() >= 24 {
            return (be32(self.data, 16), be32(self.data, 20));
        }
        if self.data.len() >= 16 && le32(self.data, 0) >= 40 {
            let w = le32(self.data, 4) as i32;
            let h = le32(self.data, 8) as i32;
            if w > 0 && h != 0 {
                return (w as u32, h.unsigned_abs() / 2);
            }
        }
        (self.width, self.height)
    }

    fn depth(&self) -> u16 {
        if self.data.starts_with(PNG_SIG) {
            return 32;
        }
        if self.data.len() >= 16 && le32(self.data, 0) >= 40 {
            return le16(self.data, 14);
        }
        self.bpp
    }
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The game's icon as PNG bytes: GOG's `.ico` in `game_dir`, else `exe`'s.
/// `None` when neither has one gogo can read.
pub fn find(game_dir: Option<&Path>, gogid: &str, exe: Option<&Path>) -> Option<Vec<u8>> {
    let ico = game_dir.and_then(|d| std::fs::read(d.join(format!("goggame-{gogid}.ico"))).ok());
    if let Some(png) = ico.as_deref().and_then(from_ico) {
        return Some(png);
    }
    from_exe(&std::fs::read(exe?).ok()?)
}

/// The best image of an `.ico` file, as PNG.
pub fn from_ico(ico: &[u8]) -> Option<Vec<u8>> {
    if ico.len() < 6 || le16(ico, 0) != 0 || le16(ico, 2) != 1 {
        return None;
    }
    let n = usize::from(le16(ico, 4));
    let images = (0..n).filter_map(|i| {
        let e = ico.get(6 + i * 16..6 + (i + 1) * 16)?;
        let (size, off) = (le32(e, 8) as usize, le32(e, 12) as usize);
        Some(Image {
            width: dim(e[0]),
            height: dim(e[1]),
            bpp: le16(e, 6),
            data: ico.get(off..off.checked_add(size)?)?,
        })
    });
    best_png(images)
}

/// The exe's first icon group (the one a file manager shows), as PNG.
pub fn from_exe(exe: &[u8]) -> Option<Vec<u8>> {
    match FileKind::parse(exe).ok()? {
        FileKind::Pe32 => exe_icon::<pe::ImageNtHeaders32>(exe),
        FileKind::Pe64 => exe_icon::<pe::ImageNtHeaders64>(exe),
        _ => None,
    }
}

fn dim(b: u8) -> u32 {
    if b == 0 { 256 } else { u32::from(b) }
}

fn exe_icon<Pe: ImageNtHeaders>(data: &[u8]) -> Option<Vec<u8>> {
    let file = PeFile::<Pe, &[u8]>::parse(data).ok()?;
    let sections = file.section_table();
    let rsrc = file
        .data_directories()
        .resource_directory(data, &sections)
        .ok()??;
    // type → name → language → data; the first entry at each level below
    // the type (the first group, its first language).
    let of_type = |kind: u16| {
        rsrc.root().ok()?.entries.iter().find_map(|e| {
            (e.name_or_id().id() == Some(kind))
                .then(|| e.data(rsrc).ok()?.table())
                .flatten()
        })
    };
    let bytes = |entry: &pe::ImageResourceDirectoryEntry| -> Option<&[u8]> {
        let leaf = first_leaf(rsrc, entry)?;
        let rva = leaf.offset_to_data.get(LE);
        let size = leaf.size.get(LE) as usize;
        sections.pe_data_at(data, rva)?.get(..size)
    };
    let group = of_type(pe::RT_GROUP_ICON)?;
    let group = bytes(group.entries.first()?)?;
    let icons = of_type(pe::RT_ICON)?;
    if group.len() < 6 {
        return None;
    }
    let n = usize::from(le16(group, 4));
    let images: Vec<Image> = (0..n)
        .filter_map(|i| {
            let e = group.get(6 + i * 14..6 + (i + 1) * 14)?;
            let id = le16(e, 12);
            let entry = icons
                .entries
                .iter()
                .find(|x| x.name_or_id().id() == Some(id))?;
            Some(Image {
                width: dim(e[0]),
                height: dim(e[1]),
                bpp: le16(e, 6),
                data: bytes(entry)?,
            })
        })
        .collect();
    best_png(images.into_iter())
}

/// Down the first entry of each table to a data leaf (the resource tree is
/// three levels deep; a damaged one isn't followed further than that).
fn first_leaf<'d>(
    rsrc: ResourceDirectory<'d>,
    entry: &pe::ImageResourceDirectoryEntry,
) -> Option<&'d pe::ImageResourceDataEntry> {
    let mut data = entry.data(rsrc).ok()?;
    for _ in 0..3 {
        match data {
            ResourceDirectoryEntryData::Data(d) => return Some(d),
            ResourceDirectoryEntryData::Table(t) => data = t.entries.first()?.data(rsrc).ok()?,
        }
    }
    None
}

/// The largest image (then the deepest) that converts.
fn best_png<'a>(images: impl Iterator<Item = Image<'a>>) -> Option<Vec<u8>> {
    let mut all: Vec<Image> = images.collect();
    all.sort_by_key(|i| {
        let (w, h) = i.size();
        std::cmp::Reverse((u64::from(w) * u64::from(h), i.depth()))
    });
    all.into_iter().find_map(to_png)
}

fn to_png(img: Image) -> Option<Vec<u8>> {
    if img.data.starts_with(PNG_SIG) {
        return Some(img.data.to_vec());
    }
    let (w, h, rgba) = decode_dib(img.data)?;
    Some(encode_png(w, h, &rgba))
}

/// An icon DIB: `BITMAPINFOHEADER` (height doubled: colour, then mask),
/// the palette for ≤ 8 bpp, the colour rows, the 1 bpp AND mask, rows
/// bottom-up and padded to 4 bytes. RGBA out.
fn decode_dib(d: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if d.len() < 40 {
        return None;
    }
    let hsize = le32(d, 0) as usize;
    let w = le32(d, 4) as i32;
    let h2 = le32(d, 8) as i32;
    let bpp = le16(d, 14);
    let compression = le32(d, 16);
    let clr_used = le32(d, 32) as usize;
    if hsize < 40 || w <= 0 || h2 == 0 || w as u32 > MAX_SIDE {
        return None;
    }
    let (w, h) = (w as usize, (h2.unsigned_abs() / 2) as usize);
    if h == 0 || h as u32 > MAX_SIDE {
        return None;
    }
    let bottom_up = h2 > 0;
    // BI_RGB, or BI_BITFIELDS for 16/32 bpp (masks after the header).
    let masks = match (compression, bpp) {
        (0, _) => 0,
        (3, 16 | 32) => 12,
        _ => return None,
    };
    let pal_n = match bpp {
        1 | 4 | 8 if clr_used > 0 => clr_used.min(256),
        1 | 4 | 8 => 1 << bpp,
        16 | 24 | 32 => 0,
        _ => return None,
    };
    let pal_at = hsize + masks;
    let px_at = pal_at + pal_n * 4;
    let row = (w * usize::from(bpp)).div_ceil(32) * 4;
    let mask_row = w.div_ceil(32) * 4;
    let mask_at = px_at + row * h;
    if d.len() < mask_at {
        return None;
    }
    let palette = &d[pal_at..px_at];
    let has_mask = d.len() >= mask_at + mask_row * h;
    let mut out = vec![0u8; w * h * 4];
    let mut any_alpha = false;
    for y in 0..h {
        let src_y = if bottom_up { h - 1 - y } else { y };
        let line = &d[px_at + src_y * row..px_at + (src_y + 1) * row];
        for x in 0..w {
            let (r, g, b, a) = match bpp {
                32 => (
                    line[x * 4 + 2],
                    line[x * 4 + 1],
                    line[x * 4],
                    line[x * 4 + 3],
                ),
                24 => (line[x * 3 + 2], line[x * 3 + 1], line[x * 3], 255),
                16 => {
                    let v = u16::from_le_bytes([line[x * 2], line[x * 2 + 1]]);
                    let c = |s: u16| (((v >> s) & 0x1f) as u8) << 3;
                    (c(10), c(5), c(0), 255)
                }
                _ => {
                    let per = 8 / usize::from(bpp);
                    let byte = line[x / per];
                    let shift = 8 - usize::from(bpp) * (x % per + 1);
                    let i = usize::from(byte >> shift) & ((1 << bpp) - 1);
                    match palette.get(i * 4..i * 4 + 3) {
                        Some(p) => (p[2], p[1], p[0], 255),
                        None => (0, 0, 0, 255),
                    }
                }
            };
            any_alpha |= bpp == 32 && a != 0;
            let o = (y * w + x) * 4;
            out[o..o + 4].copy_from_slice(&[r, g, b, a]);
        }
    }
    // The AND mask is the transparency, except for a 32 bpp image with an
    // alpha channel of its own.
    if bpp != 32 || !any_alpha {
        for y in 0..h {
            let src_y = if bottom_up { h - 1 - y } else { y };
            for x in 0..w {
                let masked = has_mask && {
                    let byte = d[mask_at + src_y * mask_row + x / 8];
                    byte & (0x80 >> (x % 8)) != 0
                };
                out[(y * w + x) * 4 + 3] = if masked { 0 } else { 255 };
            }
        }
    }
    Some((w as u32, h as u32, out))
}

/// An 8-bit RGBA PNG: IHDR, one IDAT (filter 0 rows, zlib), IEND.
fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    }
    let mut out = PNG_SIG.to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    let stride = w as usize * 4;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    for row in rgba.chunks(stride) {
        z.write_all(&[0]).expect("writing to a Vec");
        z.write_all(row).expect("writing to a Vec");
    }
    chunk(&mut out, b"IDAT", &z.finish().expect("writing to a Vec"));
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `w`×`w` 32 bpp DIB: every pixel `bgra`, the mask all clear.
    pub(crate) fn dib32(w: u32, bgra: [u8; 4]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes());
        d.extend_from_slice(&w.to_le_bytes());
        d.extend_from_slice(&(w * 2).to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&32u16.to_le_bytes());
        d.extend_from_slice(&[0; 24]);
        for _ in 0..w * w {
            d.extend_from_slice(&bgra);
        }
        d.extend(std::iter::repeat_n(0, (w.div_ceil(32) * 4 * w) as usize));
        d
    }

    /// An `.ico` holding `images` (directory sizes from each image).
    pub(crate) fn ico(images: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0, 0, 1, 0];
        out.extend_from_slice(&(images.len() as u16).to_le_bytes());
        let mut off = 6 + 16 * images.len();
        for (side, data) in images {
            out.extend_from_slice(&[*side, *side, 0, 0]);
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(off as u32).to_le_bytes());
            off += data.len();
        }
        for (_, data) in images {
            out.extend_from_slice(data);
        }
        out
    }

    /// Decode our own PNG back (IHDR and the unfiltered IDAT).
    fn png_pixels(png: &[u8]) -> (u32, u32, Vec<u8>) {
        assert!(png.starts_with(PNG_SIG));
        let (w, h) = (be32(png, 16), be32(png, 20));
        let mut at = 8;
        let mut idat = Vec::new();
        while at < png.len() {
            let len = be32(png, at) as usize;
            let kind = &png[at + 4..at + 8];
            let data = &png[at + 8..at + 8 + len];
            let mut crc = flate2::Crc::new();
            crc.update(kind);
            crc.update(data);
            assert_eq!(crc.sum(), be32(png, at + 8 + len), "CRC of {kind:?}");
            if kind == b"IDAT" {
                idat.extend_from_slice(data);
            }
            at += 12 + len;
        }
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&idat[..]), &mut raw)
            .unwrap();
        let px: Vec<u8> = raw
            .chunks(1 + w as usize * 4)
            .flat_map(|r| {
                assert_eq!(r[0], 0);
                r[1..].to_vec()
            })
            .collect();
        (w, h, px)
    }

    #[test]
    fn the_largest_dib_becomes_an_rgba_png() {
        let ico = ico(&[
            (16, dib32(16, [0, 0, 255, 255])),
            (32, dib32(32, [255, 0, 0, 128])),
        ]);
        let (w, h, px) = png_pixels(&from_ico(&ico).unwrap());
        assert_eq!((w, h), (32, 32));
        // BGRA blue at half alpha → RGBA.
        assert_eq!(&px[..4], &[0, 0, 255, 128]);
    }

    #[test]
    fn a_png_image_is_kept_as_it_is() {
        let png = encode_png(64, 64, &[9; 64 * 64 * 4]);
        let ico = ico(&[(16, dib32(16, [0; 4])), (0, png.clone())]);
        assert_eq!(from_ico(&ico).unwrap(), png);
    }

    #[test]
    fn without_alpha_the_mask_is_the_transparency() {
        // 32 bpp with alpha all 0: the AND mask decides (first pixel masked).
        let mut d = dib32(8, [1, 2, 3, 0]);
        let mask_at = d.len() - 4 * 8;
        // Bottom-up: the last mask row is the top row of the image.
        d[mask_at + 4 * 7] = 0x80;
        let (_, _, px) = decode_dib(&d).unwrap();
        assert_eq!(&px[..4], &[3, 2, 1, 0]);
        assert_eq!(&px[4..8], &[3, 2, 1, 255]);
    }

    #[test]
    fn palette_images_decode() {
        // 2×2, 1 bpp: palette black/white, rows "10" and "01" (bottom-up).
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes());
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&4u32.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&[0; 24]);
        d.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]);
        d.extend_from_slice(&[0x40, 0, 0, 0, 0x80, 0, 0, 0]);
        d.extend_from_slice(&[0; 8]);
        let (w, h, px) = decode_dib(&d).unwrap();
        assert_eq!((w, h), (2, 2));
        // Top row is the second stored row: white, black.
        assert_eq!(&px[..8], &[255, 255, 255, 255, 0, 0, 0, 255]);
        assert_eq!(&px[8..16], &[0, 0, 0, 255, 255, 255, 255, 255]);
    }

    const RVA: u32 = 0x1000;

    /// A `.rsrc` section at [`RVA`]: one `RT_ICON` (id 1) holding `icon`,
    /// one `RT_GROUP_ICON` naming it.
    fn rsrc(icon: &[u8], side: u8) -> Vec<u8> {
        let table = |n: u16| {
            let mut t = vec![0u8; 12];
            t.extend_from_slice(&0u16.to_le_bytes());
            t.extend_from_slice(&n.to_le_bytes());
            t
        };
        let entry = |id: u32, off: u32, dir: bool| {
            let mut e = id.to_le_bytes().to_vec();
            e.extend_from_slice(&(off | if dir { 0x8000_0000 } else { 0 }).to_le_bytes());
            e
        };
        let (a, a1, b, b1, da, db, icon_at) = (32, 56, 80, 104, 128, 144, 160u32);
        let group_at = icon_at + (icon.len() as u32).next_multiple_of(4);
        let mut group = vec![0, 0, 1, 0, 1, 0, side, side, 0, 0];
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&32u16.to_le_bytes());
        group.extend_from_slice(&(icon.len() as u32).to_le_bytes());
        group.extend_from_slice(&1u16.to_le_bytes());
        let mut s = table(2);
        s.extend(entry(u32::from(pe::RT_ICON), a, true));
        s.extend(entry(u32::from(pe::RT_GROUP_ICON), b, true));
        for (next, leaf) in [(a1, da), (0, 0), (b1, db)] {
            if next == 0 {
                continue;
            }
            s.extend(table(1));
            s.extend(entry(1, next, true));
            s.extend(table(1));
            s.extend(entry(0x409, leaf, false));
        }
        for (at, len) in [(icon_at, icon.len()), (group_at, group.len())] {
            s.extend_from_slice(&(RVA + at).to_le_bytes());
            s.extend_from_slice(&(len as u32).to_le_bytes());
            s.extend_from_slice(&[0; 8]);
        }
        assert_eq!(s.len(), icon_at as usize);
        s.extend_from_slice(icon);
        s.resize(group_at as usize, 0);
        s.extend_from_slice(&group);
        s
    }

    /// A PE32 whose one section is `sec`, the resource directory.
    fn pe32_with_rsrc(mut sec: Vec<u8>) -> Vec<u8> {
        const OFF: usize = 0x200;
        let len = sec.len() as u32;
        let raw = sec.len().next_multiple_of(0x200);
        sec.resize(raw, 0);
        let mut w = Vec::new();
        let u16le = |w: &mut Vec<u8>, v: u16| w.extend_from_slice(&v.to_le_bytes());
        let u32le = |w: &mut Vec<u8>, v: u32| w.extend_from_slice(&v.to_le_bytes());
        w.extend_from_slice(b"PE\0\0");
        u16le(&mut w, 0x014C);
        u16le(&mut w, 1);
        w.extend_from_slice(&[0; 12]);
        u16le(&mut w, 224);
        u16le(&mut w, 0x0102);
        u16le(&mut w, 0x10B);
        w.extend_from_slice(&[0, 0]);
        for v in [0, raw as u32, 0, 0, RVA, 0, 0x40_0000, 0x1000, 0x200] {
            u32le(&mut w, v);
        }
        for v in [6u16, 0, 0, 0, 6, 0] {
            u16le(&mut w, v);
        }
        for v in [
            0,
            RVA + (raw as u32).next_multiple_of(0x1000),
            OFF as u32,
            0,
        ] {
            u32le(&mut w, v);
        }
        u16le(&mut w, 2);
        u16le(&mut w, 0);
        w.extend_from_slice(&[0; 16]);
        u32le(&mut w, 0);
        u32le(&mut w, 16);
        for i in 0..16 {
            let (va, size) = if i == 2 { (RVA, len) } else { (0, 0) };
            u32le(&mut w, va);
            u32le(&mut w, size);
        }
        w.extend_from_slice(b".rsrc\0\0\0");
        for v in [len, RVA, raw as u32, OFF as u32, 0, 0, 0, 0x4000_0040] {
            u32le(&mut w, v);
        }
        let mut h = vec![0u8; OFF];
        h[0..2].copy_from_slice(b"MZ");
        h[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        h[0x40..0x40 + w.len()].copy_from_slice(&w);
        h.extend_from_slice(&sec);
        h
    }

    #[test]
    fn the_exe_icon_comes_from_its_first_icon_group() {
        let exe = pe32_with_rsrc(rsrc(&dib32(24, [0, 255, 0, 255]), 24));
        let (w, h, px) = png_pixels(&from_exe(&exe).unwrap());
        assert_eq!((w, h), (24, 24));
        assert_eq!(&px[..4], &[0, 255, 0, 255]);

        // GOG's .ico beats the exe's.
        let tmp = tempfile::tempdir().unwrap();
        let exe_path = tmp.path().join("game.exe");
        std::fs::write(&exe_path, &exe).unwrap();
        let from_dir = find(Some(tmp.path()), "7", Some(&exe_path)).unwrap();
        assert_eq!(png_pixels(&from_dir).0, 24);
        std::fs::write(
            tmp.path().join("goggame-7.ico"),
            ico(&[(48, dib32(48, [0; 4]))]),
        )
        .unwrap();
        let from_dir = find(Some(tmp.path()), "7", Some(&exe_path)).unwrap();
        assert_eq!(png_pixels(&from_dir).0, 48);
    }

    #[test]
    fn junk_is_no_icon() {
        assert!(from_ico(b"not an icon").is_none());
        assert!(from_ico(&ico(&[(16, vec![1, 2, 3])])).is_none());
        assert!(from_exe(b"MZ nothing").is_none());
        assert!(find(None, "1", None).is_none());
    }
}

//! The grain tile of the noise effect (design 28 §2: "a tiled noise image brush").

use std::sync::Arc;

use crate::Image;

/// The side of a noise tile in pixels.
pub const NOISE_SIZE: u32 = 128;

/// A tiling grey noise tile, opaque, deterministic in `seed` (a xorshift; seed 0 counts as
/// 1). The same seed gives the same pixels on every machine, so goldens stay stable.
pub fn noise_tile(seed: u32) -> Image {
    let mut s = seed.max(1).wrapping_mul(2_654_435_761).max(1);
    let n = (NOISE_SIZE * NOISE_SIZE) as usize;
    let mut rgba = Vec::with_capacity(n * 4);
    for _ in 0..n {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        let v = (s >> 24) as u8;
        rgba.extend_from_slice(&[v, v, v, 255]);
    }
    Image {
        width: NOISE_SIZE,
        height: NOISE_SIZE,
        rgba: Arc::new(rgba),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_is_deterministic_opaque_and_varies_with_the_seed() {
        let a = noise_tile(7);
        assert_eq!((a.width, a.height), (NOISE_SIZE, NOISE_SIZE));
        assert_eq!(a.rgba.len(), (NOISE_SIZE * NOISE_SIZE * 4) as usize);
        assert!(
            a.rgba
                .chunks(4)
                .all(|p| p[3] == 255 && p[0] == p[1] && p[1] == p[2])
        );
        assert_eq!(a, noise_tile(7));
        assert_ne!(a, noise_tile(8));
        assert_eq!(noise_tile(0), noise_tile(1));
        let distinct: std::collections::HashSet<u8> = a.rgba.chunks(4).map(|p| p[0]).collect();
        assert!(distinct.len() > 200, "{}", distinct.len());
    }
}

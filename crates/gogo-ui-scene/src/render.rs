//! The renderer boundary (design 24 §2, design 28).

use crate::{DisplayList, ImageStore};

/// Somewhere a renderer draws: a pixel buffer, a window's swap chain. What it is belongs to
/// the renderer that made it; this trait is what the shell and the tests need from any.
pub trait Surface {
    /// The size in device pixels.
    fn size(&self) -> (u32, u32);
}

/// Draws a [`DisplayList`] (design 28). The CPU renderer (vello_cpu) and the GPU renderer
/// (vello_gpu on wgpu) implement it; the shell and `gogo-gui` hold one without knowing
/// which (design 24 §2.1 rule 4).
///
/// A renderer:
/// - applies `scale` itself: the list is in logical px (design 28 §1);
/// - draws the ops first to last, honouring the layers that `Push…` ops open and
///   [`Op::Pop`](crate::Op::Pop) closes, and clears the surface to
///   [`DisplayList::background`] first;
/// - looks images up through `images` by [`ImageKey`](crate::ImageKey) and may cache them
///   under that key; an image the store lacks is skipped, not an error;
/// - never panics on a well-formed list (every `Push…` has its `Pop`).
pub trait Renderer {
    /// What the renderer draws into.
    type Surface: Surface;
    /// Why a frame could not be drawn.
    type Error: std::error::Error + Send + Sync + 'static;

    /// A short name for diagnostics, such as the about route: `"cpu"` or `"gpu"`.
    fn name(&self) -> &'static str;

    /// Draws `list` into `surface` at `scale` device px per logical px.
    ///
    /// # Errors
    /// The renderer's own error, for example a lost GPU device.
    fn render(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        surface: &mut Self::Surface,
    ) -> Result<(), Self::Error>;
}

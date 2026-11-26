//! The windowed path without a GPU: a [`PixelRenderer`]'s pixels through softbuffer
//! (design 24 decision 5).

use std::num::NonZeroU32;

use crate::{Frame, PixelRenderer, Presented, RenderError, WindowTarget};

/// Why softbuffer could not present.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CpuError {
    #[error("softbuffer: {0}")]
    Softbuffer(String),
    #[error("the renderer returned {got} bytes for {width}x{height} pixels")]
    Length { got: usize, width: u32, height: u32 },
}

fn failed(e: softbuffer::SoftBufferError) -> CpuError {
    CpuError::Softbuffer(e.to_string())
}

/// A pixel renderer and the window it presents to.
pub(crate) struct CpuWindow {
    surface: softbuffer::Surface<WindowTarget, WindowTarget>,
    renderer: Box<dyn PixelRenderer>,
}

impl CpuWindow {
    pub(crate) fn new(
        target: WindowTarget,
        renderer: Box<dyn PixelRenderer>,
    ) -> Result<CpuWindow, CpuError> {
        let context = softbuffer::Context::new(target.clone()).map_err(failed)?;
        let surface = softbuffer::Surface::new(&context, target).map_err(failed)?;
        Ok(CpuWindow { surface, renderer })
    }

    pub(crate) fn name(&self) -> &'static str {
        self.renderer.name()
    }

    pub(crate) fn present(
        &mut self,
        frame: &Frame<'_>,
        scale: f64,
        (width, height): (u32, u32),
        before_present: &mut dyn FnMut(),
    ) -> Result<Presented, RenderError> {
        let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            return Ok(Presented::Skipped);
        };
        if frame.images_changed {
            self.renderer.clear_images();
        }
        let rgba = self
            .renderer
            .draw(frame.list, frame.images, scale, width, height)?;
        if u64::try_from(rgba.len()) != Ok(u64::from(width) * u64::from(height) * 4) {
            return Err(Box::new(CpuError::Length {
                got: rgba.len(),
                width,
                height,
            }));
        }
        self.surface.resize(w, h).map_err(failed)?;
        let mut buffer = self.surface.buffer_mut().map_err(failed)?;
        pack(rgba, &mut buffer);
        before_present();
        buffer.present().map_err(failed)?;
        Ok(Presented::Shown)
    }
}

/// Premultiplied RGBA8 to softbuffer's `0x00RRGGBB`. The window is opaque, so a pixel that
/// is not is shown as it would be over black, which premultiplied colours already are.
fn pack(rgba: &[u8], out: &mut [u32]) {
    for (pixel, packed) in rgba.as_chunks::<4>().0.iter().zip(out) {
        *packed = u32::from_be_bytes([0, pixel[0], pixel[1], pixel[2]]);
    }
}

#[cfg(test)]
mod tests {
    use super::pack;

    #[test]
    fn pixels_are_packed_as_0rgb() {
        let rgba = [1, 2, 3, 255, 200, 100, 50, 128];
        let mut out = [u32::MAX; 2];
        pack(&rgba, &mut out);
        assert_eq!(out, [0x0001_0203, 0x00c8_6432]);
    }
}

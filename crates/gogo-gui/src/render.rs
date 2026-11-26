//! The two renderers behind the shell's traits (design 24 §4): `gogo-gui` picks and
//! injects them, so the shell knows neither (design 24 §2.1 rule 4).

use gogo_render_cpu::{CpuRenderer, CpuSurface};
use gogo_render_gpu::{GpuWindow, Shown};
use gogo_ui_scene::{DisplayList, ImageStore, Renderer as _, Surface as _};
use gogo_ui_shell::{PixelRenderer, Presented, RenderError, WindowRenderer, WindowTarget};

/// vello_cpu into a buffer the size of the window.
#[derive(Default)]
pub(crate) struct Cpu {
    renderer: CpuRenderer,
    surface: Option<CpuSurface>,
}

impl PixelRenderer for Cpu {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn draw(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        width: u32,
        height: u32,
    ) -> Result<&[u8], RenderError> {
        let surface = match self.surface.take() {
            Some(s) if s.size() == (width, height) => self.surface.insert(s),
            _ => self.surface.insert(CpuSurface::new(width, height)?),
        };
        self.renderer.render(list, images, scale, surface)?;
        Ok(surface.premultiplied())
    }

    fn clear_images(&mut self) {
        self.renderer.clear_images();
    }
}

/// vello_gpu on the window's own wgpu surface.
pub(crate) struct Gpu(GpuWindow);

impl Gpu {
    pub(crate) fn new(target: WindowTarget, width: u32, height: u32) -> Result<Gpu, RenderError> {
        let window = GpuWindow::new(target, width, height)?;
        if let Some(adapter) = window.adapter_info() {
            tracing::info!("GPU: {} ({:?})", adapter.name, adapter.backend);
        }
        Ok(Gpu(window))
    }
}

impl WindowRenderer for Gpu {
    fn name(&self) -> &'static str {
        "gpu"
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        Ok(self.0.resize(width, height)?)
    }

    fn present(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        before_present: &mut dyn FnMut(),
    ) -> Result<Presented, RenderError> {
        Ok(match self.0.present(list, images, scale, before_present)? {
            Shown::Presented => Presented::Shown,
            Shown::Skipped => Presented::Skipped,
        })
    }

    fn clear_images(&mut self) {
        self.0.clear_images();
    }
}

//! Presenting to a window (design 24 §4 item 6): the renderer makes and owns the window's
//! wgpu surface from its raw handles, so whoever runs the window never touches wgpu.

use gogo_ui_scene::{DisplayList, ImageStore, Renderer as _};
use wgpu::rwh::{HasDisplayHandle, HasWindowHandle};

use crate::{GpuError, GpuRenderer, GpuSurface, catching};

/// What happened to a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// The frame is on its way to the screen.
    Presented,
    /// Nothing was drawn: the window is hidden, has no size, or the compositor did not
    /// give a frame in time. Draw again on the next event.
    Skipped,
}

/// A renderer and the surface of one window.
#[derive(Debug)]
pub struct GpuWindow {
    // Dropped before the renderer's device.
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: GpuRenderer,
    /// The size asked for, which may be 0 (a minimised window).
    size: (u32, u32),
}

impl GpuWindow {
    /// A renderer for `window`, `width` by `height` device pixels. `WGPU_BACKEND` and
    /// wgpu's other environment variables apply. On macOS, call it on the main thread.
    ///
    /// # Errors
    /// [`GpuError::NoAdapter`], [`GpuError::NoDevice`], or [`GpuError::Surface`] when the
    /// window cannot be drawn to.
    pub fn new<W>(window: W, width: u32, height: u32) -> Result<GpuWindow, GpuError>
    where
        W: HasWindowHandle + HasDisplayHandle + Clone + std::fmt::Debug + Send + Sync + 'static,
    {
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone())),
        );
        let surface = instance
            .create_surface(window)
            .map_err(|e| GpuError::Surface(e.to_string()))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..wgpu::RequestAdapterOptions::default()
        }))
        .map_err(|e| GpuError::NoAdapter(e.to_string()))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gogo window"),
            required_limits: adapter.limits(),
            ..wgpu::DeviceDescriptor::default()
        }))
        .map_err(|e| GpuError::NoDevice(e.to_string()))?;

        let formats = surface.get_capabilities(&adapter).formats;
        let format = pick_format(&formats).ok_or_else(|| {
            GpuError::Surface(format!("no non-sRGB 8-bit format among {formats:?}"))
        })?;
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or_else(|| GpuError::Surface("the adapter cannot present here".to_owned()))?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::AutoVsync;

        // Pipelines are built here: a driver that cannot is found now, not on a frame.
        let mut renderer = catching(&device, || {
            Ok(GpuRenderer::new(device.clone(), queue, format))
        })?;
        renderer.adapter = Some(adapter.get_info());
        let mut window = GpuWindow {
            surface,
            config,
            renderer,
            size: (width, height),
        };
        window.configure()?;
        Ok(window)
    }

    /// The adapter in use.
    pub fn adapter_info(&self) -> Option<&wgpu::AdapterInfo> {
        self.renderer.adapter_info()
    }

    /// The window now has `width` by `height` device pixels. Takes effect on the next
    /// frame; 0 means there is nothing to draw.
    ///
    /// # Errors
    /// [`GpuError::Size`] when the device cannot hold it.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), GpuError> {
        self.size = (width, height);
        if width == 0 || height == 0 || (width, height) == (self.config.width, self.config.height) {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.configure()
    }

    /// Forgets the cached images (see [`GpuRenderer::clear_images`]).
    pub fn clear_images(&mut self) {
        self.renderer.clear_images();
    }

    /// Draws `list` at `scale` into the window's next frame. `before_present` runs after
    /// drawing and before the frame is queued, for the window system's frame pacing.
    ///
    /// # Errors
    /// [`GpuError`], when the surface or the device is lost.
    pub fn present(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        before_present: &mut dyn FnMut(),
    ) -> Result<Shown, GpuError> {
        use wgpu::CurrentSurfaceTexture as Next;

        if self.size.0 == 0 || self.size.1 == 0 {
            return Ok(Shown::Skipped);
        }
        let mut next = self.surface.get_current_texture();
        if matches!(next, Next::Outdated | Next::Lost) {
            self.configure()?;
            next = self.surface.get_current_texture();
        }
        let (texture, stale) = match next {
            Next::Success(texture) => (texture, false),
            Next::Suboptimal(texture) => (texture, true),
            Next::Timeout | Next::Occluded => return Ok(Shown::Skipped),
            other => {
                return Err(GpuError::Surface(format!(
                    "no frame to draw into: {other:?}"
                )));
            }
        };
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut target = GpuSurface::from_view(view)?;
        self.renderer.render(list, images, scale, &mut target)?;
        drop(target);
        before_present();
        self.renderer.queue.present(texture);
        if stale {
            self.configure()?;
        }
        Ok(Shown::Presented)
    }

    fn configure(&mut self) -> Result<(), GpuError> {
        let (width, height) = (self.config.width, self.config.height);
        let device = &self.renderer.device;
        if width.max(height) > device.limits().max_texture_dimension_2d {
            return Err(GpuError::Size { width, height });
        }
        catching(device, || {
            self.surface.configure(device, &self.config);
            Ok(())
        })
    }
}

/// The colours of a display list are written as they are, so the format must not be sRGB.
fn pick_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    use wgpu::TextureFormat::{Bgra8Unorm, Rgba8Unorm};
    formats
        .iter()
        .copied()
        .find(|f| matches!(f, Bgra8Unorm | Rgba8Unorm))
}

#[cfg(test)]
mod tests {
    use super::pick_format;
    use wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgb10a2Unorm, Rgba8Unorm};

    #[test]
    fn the_window_format_is_never_srgb() {
        assert_eq!(pick_format(&[Bgra8UnormSrgb, Bgra8Unorm]), Some(Bgra8Unorm));
        assert_eq!(pick_format(&[Rgb10a2Unorm, Rgba8Unorm]), Some(Rgba8Unorm));
        assert_eq!(pick_format(&[Bgra8UnormSrgb]), None);
    }
}

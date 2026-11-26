//! The window (design 24 §2): the winit loop, surfaces, frame pacing, the platform
//! accessibility adapter and file watching for hot reload.
//!
//! The shell knows nothing about a skin, an app or a renderer. An application implements
//! [`ShellApp`]: it hands over display lists and makes the renderers, a [`WindowRenderer`]
//! that owns the window's GPU surface or a [`PixelRenderer`] whose pixels the shell presents
//! through softbuffer (design 24 §4).
//!
//! # Frames
//!
//! Nothing is drawn on a timer. A frame is drawn after a resize or a scale change, after a
//! watched file changed, after [`Waker::wake`] if [`ShellApp::wake`] says so, and when the
//! window system asks. Requests between two frames become one frame, and the window
//! system's frame callbacks set the pace (design 25 §3).
//!
//! # Which renderer
//!
//! [`RendererChoice::Auto`] tries the GPU and falls back to the CPU. A GPU renderer that
//! fails later is made again once; if that fails too, `Auto` carries on with the CPU.
//! [`RendererChoice::Gpu`] never falls back: it ends with an error.
//!
//! Input is not delivered yet: it arrives with `gogo-ui-interact` (#533).
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod a11y;
mod cpu;
mod output;
mod run;
mod watch;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

pub use accesskit;
use gogo_ui_scene::{DisplayList, ImageStore};
use winit::raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};

pub use run::{Shell, Waker};

/// An error of an application's renderer.
pub type RenderError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Which renderer to use (design 24 §4 item 4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RendererChoice {
    /// The GPU, or the CPU when the GPU does not work.
    #[default]
    Auto,
    /// The GPU only.
    Gpu,
    /// The CPU only.
    Cpu,
}

/// A name that is not `auto`, `gpu` or `cpu`.
#[derive(Debug, thiserror::Error)]
#[error("unknown renderer {0:?}: expected auto, gpu or cpu")]
pub struct UnknownRenderer(String);

impl std::str::FromStr for RendererChoice {
    type Err = UnknownRenderer;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(RendererChoice::Auto),
            "gpu" => Ok(RendererChoice::Gpu),
            "cpu" => Ok(RendererChoice::Cpu),
            other => Err(UnknownRenderer(other.to_owned())),
        }
    }
}

/// Why the window could not run.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShellError {
    /// There is no window system, or its event loop failed.
    #[error("the window system: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),
    /// The window could not be made.
    #[error("could not open a window: {0}")]
    Window(#[from] winit::error::OsError),
    /// No renderer works.
    #[error("no renderer works: {0}")]
    Renderer(RenderError),
}

/// How the window looks to the window system.
#[derive(Debug, Clone)]
pub struct WindowOptions {
    /// The title.
    pub title: String,
    /// The application id (Wayland) or class (X11).
    pub app_id: String,
    /// The size to ask for, in logical px.
    pub size: (f64, f64),
    /// The smallest size, in logical px.
    pub min_size: Option<(f64, f64)>,
}

/// One frame to draw.
pub struct Frame<'a> {
    /// What to draw, laid out for the size the shell asked for.
    pub list: &'a DisplayList,
    /// Its images.
    pub images: &'a dyn ImageStore,
    /// A key may now name other pixels (a skin was reloaded): drop cached images.
    pub images_changed: bool,
}

/// The window's handles, for a GPU renderer to make its surface from.
#[derive(Debug, Clone)]
pub struct WindowTarget(Arc<winit::window::Window>);

impl HasWindowHandle for WindowTarget {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.0.window_handle()
    }
}

impl HasDisplayHandle for WindowTarget {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.0.display_handle()
    }
}

/// What happened to a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    /// It is on its way to the screen.
    Shown,
    /// Nothing was drawn, for example because the window is hidden.
    Skipped,
}

/// A renderer that owns the window's surface (the GPU one).
pub trait WindowRenderer {
    /// A short name, such as `"gpu"`.
    fn name(&self) -> &'static str;

    /// The window has a new size in device pixels; 0 means there is nothing to draw.
    ///
    /// # Errors
    /// The renderer's own.
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError>;

    /// Draws and presents a frame. `before_present` runs once, just before the frame is
    /// queued for the screen.
    ///
    /// # Errors
    /// The renderer's own, such as a lost device.
    fn present(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        before_present: &mut dyn FnMut(),
    ) -> Result<Presented, RenderError>;

    /// Forgets cached images.
    fn clear_images(&mut self);
}

/// A renderer into memory (the CPU one). The shell presents its pixels.
pub trait PixelRenderer {
    /// A short name, such as `"cpu"`.
    fn name(&self) -> &'static str;

    /// Draws `list` into `width` by `height` device pixels and returns them as
    /// premultiplied RGBA8, row by row from the top-left.
    ///
    /// # Errors
    /// The renderer's own, such as a size it cannot draw.
    fn draw(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        width: u32,
        height: u32,
    ) -> Result<&[u8], RenderError>;

    /// Forgets cached images.
    fn clear_images(&mut self);
}

/// The application in the window. Everything runs on the main thread.
pub trait ShellApp {
    /// The window to open.
    fn window(&self) -> WindowOptions;

    /// The frame for a window of `width` by `height` logical px at `scale` device px per
    /// logical px.
    fn frame(&mut self, width: f32, height: f32, scale: f64) -> Frame<'_>;

    /// Makes the GPU renderer for the window.
    ///
    /// # Errors
    /// Why there is none: the shell then uses [`ShellApp::cpu`], unless told not to.
    fn gpu(
        &mut self,
        target: WindowTarget,
        width: u32,
        height: u32,
    ) -> Result<Box<dyn WindowRenderer>, RenderError>;

    /// Makes the CPU renderer.
    fn cpu(&mut self) -> Box<dyn PixelRenderer>;

    /// [`Waker::wake`] was called. Returns whether anything on screen changed.
    fn wake(&mut self) -> bool {
        false
    }

    /// A folder to watch, with everything below it.
    fn watch(&self) -> Option<PathBuf> {
        None
    }

    /// Something in the watched folder changed. Returns whether to draw again.
    fn files_changed(&mut self) -> bool {
        false
    }

    /// A frame drawn by `renderer` was presented at `at`.
    fn presented(&mut self, at: Instant, renderer: &'static str) {
        let _ = (at, renderer);
    }

    /// The accessibility tree of what is on screen. The default is the window alone.
    fn accessibility_tree(&mut self) -> accesskit::TreeUpdate {
        a11y::window_only(&self.window().title)
    }

    /// Assistive technology asks for something. Returns whether to draw again.
    fn accessibility_action(&mut self, request: accesskit::ActionRequest) -> bool {
        let _ = request;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::RendererChoice;

    #[test]
    fn renderer_names() {
        assert_eq!(
            "auto".parse::<RendererChoice>().ok(),
            Some(RendererChoice::Auto)
        );
        assert_eq!(
            "gpu".parse::<RendererChoice>().ok(),
            Some(RendererChoice::Gpu)
        );
        assert_eq!(
            "cpu".parse::<RendererChoice>().ok(),
            Some(RendererChoice::Cpu)
        );
        let error = "vulkan"
            .parse::<RendererChoice>()
            .err()
            .map(|e| e.to_string());
        assert_eq!(
            error.as_deref(),
            Some("unknown renderer \"vulkan\": expected auto, gpu or cpu")
        );
    }
}

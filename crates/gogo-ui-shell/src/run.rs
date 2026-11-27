//! The winit loop (design 24 §2, §5). The main thread owns the window, the application and
//! the renderers; other threads only send events.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::clipboard::Clipboard;
use crate::cpu::CpuWindow;
use crate::input::Translator;
use crate::output::{self, Output, Recovery};
use crate::watch::{self, Watch};
use crate::{
    Presented, RenderError, RendererChoice, ShellApp, ShellError, WindowOptions, WindowRenderer,
    WindowTarget,
};

/// What other threads send to the main one.
#[derive(Debug)]
enum UserEvent {
    Wake,
    FilesChanged,
    Access(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        UserEvent::Access(event)
    }
}

/// Wakes the main thread from any other. Calls made before it has woken become one.
#[derive(Debug, Clone)]
pub struct Waker {
    proxy: EventLoopProxy<UserEvent>,
    pending: Arc<AtomicBool>,
}

impl Waker {
    /// Has [`ShellApp::wake`] called on the main thread. Does nothing once the loop is
    /// gone.
    pub fn wake(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            let _ = self.proxy.send_event(UserEvent::Wake);
        }
    }
}

/// The event loop, before it runs. It is made first so that the application can have its
/// [`Waker`].
#[derive(Debug)]
pub struct Shell {
    event_loop: EventLoop<UserEvent>,
    pending: Arc<AtomicBool>,
}

impl Shell {
    /// Connects to the window system. On macOS this must be the main thread.
    ///
    /// # Errors
    /// [`ShellError::EventLoop`] when there is none.
    pub fn new() -> Result<Shell, ShellError> {
        Ok(Shell {
            event_loop: EventLoop::with_user_event().build()?,
            pending: Arc::default(),
        })
    }

    /// The waker for this loop.
    pub fn waker(&self) -> Waker {
        Waker {
            proxy: self.event_loop.create_proxy(),
            pending: Arc::clone(&self.pending),
        }
    }

    /// Opens the window and runs until it is closed.
    ///
    /// # Errors
    /// [`ShellError`] when there is no window or no renderer.
    pub fn run<A: ShellApp>(self, app: A, choice: RendererChoice) -> Result<(), ShellError> {
        let mut runner = Runner {
            app,
            choice,
            proxy: self.event_loop.create_proxy(),
            pending: self.pending,
            open: None,
            watch: None,
            error: None,
            started: Instant::now(),
            input: Translator::default(),
        };
        self.event_loop.run_app(&mut runner)?;
        match runner.error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

type AnyOutput = Output<Box<dyn WindowRenderer>, CpuWindow>;

/// The open window.
struct Open {
    // Dropped before the window. Opened on the first paste.
    clipboard: Option<Clipboard>,
    // Dropped before the window.
    output: Option<AnyOutput>,
    accessibility: accesskit_winit::Adapter,
    window: Arc<Window>,
    /// The GPU renderer was made again and has not presented since.
    remade: bool,
    /// Cached images are out of date and no frame has been presented since.
    images_stale: bool,
    /// A CPU frame failed and was asked for again.
    cpu_retried: bool,
}

struct Runner<A> {
    app: A,
    choice: RendererChoice,
    proxy: EventLoopProxy<UserEvent>,
    pending: Arc<AtomicBool>,
    open: Option<Open>,
    watch: Option<Watch>,
    error: Option<ShellError>,
    /// The zero of the clock input and presented frames are timed on.
    started: Instant,
    input: Translator,
}

fn attributes(options: &WindowOptions) -> WindowAttributes {
    let mut attributes = Window::default_attributes()
        .with_title(options.title.clone())
        .with_inner_size(LogicalSize::new(options.size.0, options.size.1))
        // The accessibility adapter must exist before the window is shown.
        .with_visible(false);
    if let Some((width, height)) = options.min_size {
        attributes = attributes.with_min_inner_size(LogicalSize::new(width, height));
    }
    #[cfg(target_os = "linux")]
    {
        // The Wayland app id; X11 reads the same field as its class.
        use winit::platform::wayland::WindowAttributesExtWayland;
        attributes = attributes.with_name(options.app_id.clone(), options.app_id.clone());
    }
    attributes
}

fn make_gpu<A: ShellApp>(
    app: &mut A,
    window: &Arc<Window>,
) -> Result<Box<dyn WindowRenderer>, RenderError> {
    let size = window.inner_size();
    app.gpu(WindowTarget(Arc::clone(window)), size.width, size.height)
}

fn make_cpu<A: ShellApp>(app: &mut A, window: &Arc<Window>) -> Result<CpuWindow, RenderError> {
    let renderer = app.cpu();
    Ok(CpuWindow::new(WindowTarget(Arc::clone(window)), renderer)?)
}

fn make_output<A: ShellApp>(
    app: &mut A,
    window: &Arc<Window>,
    choice: RendererChoice,
) -> Result<AnyOutput, RenderError> {
    output::open(
        choice,
        app,
        |app| make_gpu(app, window),
        |app| make_cpu(app, window),
    )
}

/// Tells the application about a frame only when it is on its way to the screen. Returns
/// whether it wants another.
fn report_shown(
    app: &mut impl ShellApp,
    drawn: &Result<Presented, RenderError>,
    at: Duration,
    renderer: &'static str,
) -> bool {
    matches!(drawn, Ok(Presented::Shown)) && app.presented(at, renderer)
}

impl<A: ShellApp> Runner<A> {
    fn stop(&mut self, event_loop: &ActiveEventLoop, error: ShellError) {
        self.error = Some(error);
        event_loop.exit();
    }

    /// Reads the clipboard for the application, if it asked.
    fn paste_if_asked(&mut self) {
        if !self.app.wants_paste() {
            return;
        }
        let Some(open) = &mut self.open else { return };
        if open.clipboard.is_none() {
            open.clipboard = Clipboard::open(&open.window);
        }
        let text = open.clipboard.as_mut().and_then(Clipboard::text);
        if self.app.pasted(text) {
            self.redraw();
        }
    }

    fn redraw(&self) {
        if let Some(open) = &self.open {
            open.window.request_redraw();
        }
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), ShellError> {
        let window = Arc::new(event_loop.create_window(attributes(&self.app.window()))?);
        let accessibility = accesskit_winit::Adapter::with_event_loop_proxy(
            event_loop,
            &window,
            self.proxy.clone(),
        );
        window.set_visible(true);
        let output =
            make_output(&mut self.app, &window, self.choice).map_err(ShellError::Renderer)?;
        window.request_redraw();
        self.open = Some(Open {
            clipboard: None,
            output: Some(output),
            accessibility,
            window,
            remade: false,
            images_stale: false,
            cpu_retried: false,
        });
        if let Some(dir) = self.app.watch() {
            let proxy = self.proxy.clone();
            let changed = move || {
                let _ = proxy.send_event(UserEvent::FilesChanged);
            };
            match watch::watch(&dir, changed) {
                Ok(watch) => self.watch = Some(watch),
                // The window still works, without reloading.
                Err(e) => tracing::warn!("not watching {}: {e}", dir.display()),
            }
        }
        Ok(())
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(open) = &mut self.open else { return };
        let PhysicalSize { width, height } = open.window.inner_size();
        if width == 0 || height == 0 {
            return;
        }
        let Some(output) = &mut open.output else {
            return;
        };
        let scale = open.window.scale_factor();
        let logical = |v: u32| (f64::from(v) / scale) as f32;
        let mut frame = self.app.frame(logical(width), logical(height), scale);
        open.images_stale |= frame.images_changed;
        frame.images_changed = open.images_stale;

        let window = &open.window;
        let mut before_present = || window.pre_present_notify();
        let (name, drawn) = match output {
            Output::Gpu(gpu) => {
                if frame.images_changed {
                    gpu.clear_images();
                }
                let drawn = gpu.resize(width, height).and_then(|()| {
                    gpu.present(frame.list, frame.images, scale, &mut before_present)
                });
                (gpu.name(), drawn)
            }
            Output::Cpu(cpu) => (
                cpu.name(),
                cpu.present(&frame, scale, (width, height), &mut before_present),
            ),
        };

        let again = report_shown(&mut self.app, &drawn, self.started.elapsed(), name);
        match drawn {
            Ok(Presented::Shown) => {
                open.remade = false;
                open.cpu_retried = false;
                open.images_stale = false;
                let app = &mut self.app;
                open.accessibility
                    .update_if_active(|| app.accessibility_tree());
                if again {
                    open.window.request_redraw();
                }
            }
            Ok(Presented::Skipped) => {}
            Err(e) if matches!(output, Output::Cpu(_)) => {
                // Nothing to fall back to: once more, then wait for the next event.
                tracing::error!("the frame could not be drawn: {e}");
                if !open.cpu_retried {
                    open.cpu_retried = true;
                    open.window.request_redraw();
                }
            }
            Err(e) => self.recover(event_loop, e),
        }
    }

    /// The GPU renderer failed on a frame.
    fn recover(&mut self, event_loop: &ActiveEventLoop, error: RenderError) {
        let Some(open) = &mut self.open else { return };
        let recovery = output::recover(self.choice, open.remade);
        tracing::warn!("the GPU renderer failed ({recovery:?}): {error}");
        // The old surface must be gone before another is made for the window.
        open.output = None;
        let made = match recovery {
            Recovery::Remake => {
                open.remade = true;
                make_output(&mut self.app, &open.window, self.choice)
            }
            Recovery::UseCpu => make_cpu(&mut self.app, &open.window).map(Output::Cpu),
            Recovery::Stop => Err(error),
        };
        match made {
            Ok(output) => {
                open.output = Some(output);
                open.window.request_redraw();
            }
            Err(e) => self.stop(event_loop, ShellError::Renderer(e)),
        }
    }
}

impl<A: ShellApp> ApplicationHandler<UserEvent> for Runner<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.open.is_none()
            && let Err(e) = self.start(event_loop)
        {
            self.stop(event_loop, e);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(open) = &mut self.open else { return };
        if open.window.id() != id {
            return;
        }
        open.accessibility.process_event(&open.window, &event);
        if let Some(input) = self.input.translate(&event, open.window.scale_factor()) {
            let at = self.started.elapsed();
            let typed = self.input.text(&event);
            let mut redraw = self.app.input(input, at);
            if let Some(typed) = typed {
                redraw |= self.app.input(typed, at);
            }
            if redraw {
                open.window.request_redraw();
            }
            self.paste_if_asked();
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. }
            | WindowEvent::Occluded(false) => open.window.request_redraw(),
            WindowEvent::RedrawRequested => self.draw(event_loop),
            _ => {}
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        let redraw = match event {
            UserEvent::Wake => {
                // Cleared first: a change made while the application reads wakes again.
                self.pending.store(false, Ordering::Release);
                self.app.wake()
            }
            UserEvent::FilesChanged => self.app.files_changed(),
            UserEvent::Access(event) => match event.window_event {
                accesskit_winit::WindowEvent::InitialTreeRequested => {
                    if let Some(open) = &mut self.open {
                        let app = &mut self.app;
                        open.accessibility
                            .update_if_active(|| app.accessibility_tree());
                    }
                    false
                }
                accesskit_winit::WindowEvent::ActionRequested(request) => {
                    let redraw = self
                        .app
                        .accessibility_action(request, self.started.elapsed());
                    self.paste_if_asked();
                    redraw
                }
                accesskit_winit::WindowEvent::AccessibilityDeactivated => false,
            },
        };
        if redraw {
            self.redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.watch = None;
        self.open = None;
    }
}

#[cfg(test)]
mod tests {
    use gogo_ui_scene::{DisplayList, MemoryStore};

    use super::*;
    use crate::{Frame, PixelRenderer};

    struct Counting {
        list: DisplayList,
        images: MemoryStore,
        presented: usize,
    }

    impl ShellApp for Counting {
        fn window(&self) -> WindowOptions {
            WindowOptions {
                title: String::new(),
                app_id: String::new(),
                size: (1.0, 1.0),
                min_size: None,
            }
        }

        fn frame(&mut self, _width: f32, _height: f32, _scale: f64) -> Frame<'_> {
            Frame {
                list: &self.list,
                images: &self.images,
                images_changed: false,
            }
        }

        fn gpu(
            &mut self,
            _target: WindowTarget,
            _width: u32,
            _height: u32,
        ) -> Result<Box<dyn WindowRenderer>, RenderError> {
            Err("none".into())
        }

        fn cpu(&mut self) -> Box<dyn PixelRenderer> {
            unreachable!("no window is opened")
        }

        fn presented(&mut self, _at: Duration, _renderer: &'static str) -> bool {
            self.presented += 1;
            true
        }
    }

    #[test]
    fn only_a_shown_frame_is_presented() {
        let mut app = Counting {
            list: DisplayList::blank(1.0, 1.0),
            images: MemoryStore::new(),
            presented: 0,
        };
        let skipped = Ok(Presented::Skipped);
        let failed = Err(RenderError::from("lost"));
        for drawn in [&skipped, &failed] {
            assert!(!report_shown(&mut app, drawn, Duration::ZERO, "cpu"));
        }
        assert_eq!(app.presented, 0);
        let shown = Ok(Presented::Shown);
        assert!(report_shown(&mut app, &shown, Duration::ZERO, "cpu"));
        assert_eq!(app.presented, 1);
    }
}

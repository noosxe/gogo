//! The application in the shell's window: the frame, the renderers and hot reload (design
//! 24 §4, design 25 §8).
//!
//! It prints a line on stdout when a frame is presented and when the skin is reloaded;
//! `scripts/gui-headless.sh` waits for them.

use std::path::{Path, PathBuf};
use std::time::Instant;

use gogo_ui_scene::DisplayList;
use gogo_ui_shell::{
    Frame, PixelRenderer, RenderError, ShellApp, WindowOptions, WindowRenderer, WindowTarget,
};

use crate::data::{Data, Images};
use crate::page::{DrawError, Page};
use crate::{panel, render};

pub(crate) struct App {
    page: Page,
    data: Data,
    images: Images,
    list: DisplayList,
    /// The skin folder being watched.
    dev: Option<PathBuf>,
    /// Why the skin on screen is not the one in the folder, one line each.
    problems: Vec<String>,
    /// A reload may have changed what an image key names.
    images_changed: bool,
}

impl App {
    pub(crate) fn new(dev: Option<&Path>, data: Data) -> Result<App, DrawError> {
        let mut app = App {
            page: Page::new()?,
            images: data.images(),
            data,
            list: DisplayList::blank(0.0, 0.0),
            dev: dev.map(Path::to_owned),
            problems: Vec::new(),
            images_changed: false,
        };
        if app.dev.is_some() {
            app.reload("loaded");
        }
        Ok(app)
    }

    /// Loads the watched folder again, saying how it went.
    fn reload(&mut self, done: &str) {
        let Some(dir) = &self.dev else { return };
        let start = Instant::now();
        let route = SHELL_ROUTE;
        let result = self.page.reload(dir, &mut self.data, route);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(()) => {
                self.problems.clear();
                self.images_changed = true;
                println!("gui: skin {done} in {ms:.1} ms");
            }
            Err(problems) => {
                println!(
                    "gui: skin reload failed in {ms:.1} ms: {} errors",
                    problems.len()
                );
                for line in &problems {
                    eprintln!("{line}");
                }
                self.problems = problems;
            }
        }
    }
}

/// The route the window shows: it draws the model's route in its outlet.
const SHELL_ROUTE: &str = "shell";

impl ShellApp for App {
    fn window(&self) -> WindowOptions {
        WindowOptions {
            title: "gogo".to_owned(),
            app_id: "gogo-gui".to_owned(),
            size: (1280.0, 800.0),
            min_size: Some((640.0, 400.0)),
        }
    }

    fn frame(&mut self, width: f32, height: f32, _scale: f64) -> Frame<'_> {
        let mut problems = self.problems.clone();
        self.list = match self.page.draw(&mut self.data, SHELL_ROUTE, (width, height)) {
            Ok(list) => list,
            Err(e) => {
                problems.push(e.to_string());
                DisplayList::blank(f64::from(width), f64::from(height))
            }
        };
        if !problems.is_empty()
            && let Err(e) = panel::draw(
                &mut self.list,
                &mut self.page.fonts,
                (width, height),
                "The skin has errors",
                &problems,
            )
        {
            tracing::error!("the error panel could not be drawn: {e}");
        }
        Frame {
            list: &self.list,
            images: &self.images,
            images_changed: std::mem::take(&mut self.images_changed),
        }
    }

    fn gpu(
        &mut self,
        target: WindowTarget,
        width: u32,
        height: u32,
    ) -> Result<Box<dyn WindowRenderer>, RenderError> {
        Ok(Box::new(render::Gpu::new(target, width, height)?))
    }

    fn cpu(&mut self) -> Box<dyn PixelRenderer> {
        Box::new(render::Cpu::default())
    }

    fn wake(&mut self) -> bool {
        self.data.pump()
    }

    fn watch(&self) -> Option<PathBuf> {
        self.dev.clone()
    }

    fn files_changed(&mut self) -> bool {
        self.reload("reloaded");
        true
    }

    fn presented(&mut self, _at: Instant, renderer: &'static str) {
        println!("gui: frame presented renderer={renderer}");
    }
}

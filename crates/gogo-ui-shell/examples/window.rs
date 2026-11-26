//! The shell on its own: a coloured window drawn by a stand-in CPU renderer. For the
//! headless runner, never the host display:
//!
//! ```text
//! nix develop -c cargo build -p gogo-ui-shell --example window
//! nix develop -c scripts/gui-headless.sh run -- target/debug/examples/window
//! ```

use gogo_ui_ir::{Color, Kind, Node, Paint, Tree};
use gogo_ui_layout::{Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, ImageStore, MemoryStore, SceneState, build};
use gogo_ui_shell::{
    Frame, PixelRenderer, RenderError, RendererChoice, Shell, ShellApp, WindowOptions,
    WindowRenderer, WindowTarget,
};
use gogo_ui_text::FontSet;

/// Fills the window with the list's background and draws a stripe across it.
#[derive(Default)]
struct Stripes(Vec<u8>);

impl PixelRenderer for Stripes {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn draw(
        &mut self,
        list: &DisplayList,
        _images: &dyn ImageStore,
        _scale: f64,
        width: u32,
        height: u32,
    ) -> Result<&[u8], RenderError> {
        let background = list.background.to_rgba8();
        let background = [background.r, background.g, background.b, 255];
        self.0 = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                if (x + y) % 160 < 40 {
                    [240, 180, 60, 255]
                } else {
                    background
                }
            })
            .collect();
        Ok(&self.0)
    }

    fn clear_images(&mut self) {}
}

struct App {
    fonts: FontSet,
    list: Option<DisplayList>,
    images: MemoryStore,
}

impl App {
    fn list(&mut self, width: f32, height: f32) -> Result<DisplayList, Box<dyn std::error::Error>> {
        let mut root = Node::new(Kind::Box);
        root.paint = Paint::solid(Color([30, 70, 110, 255]));
        let tree = Tree::new("root", root);
        let laid = layout(
            &tree,
            &mut self.fonts,
            Viewport::new(width, height),
            &Scroll::none(),
        )?;
        Ok(build(&tree, &laid, &SceneState::new()))
    }
}

impl ShellApp for App {
    fn window(&self) -> WindowOptions {
        WindowOptions {
            title: "shell example".to_owned(),
            app_id: "gogo-ui-shell-example".to_owned(),
            size: (640.0, 400.0),
            min_size: None,
        }
    }

    fn frame(&mut self, width: f32, height: f32, _scale: f64) -> Frame<'_> {
        self.list = self.list(width, height).ok();
        Frame {
            // A tree of one box always lays out.
            list: self.list.as_ref().expect("the example's tree lays out"),
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
        Err("the example has no GPU renderer".into())
    }

    fn cpu(&mut self) -> Box<dyn PixelRenderer> {
        Box::new(Stripes::default())
    }

    fn presented(&mut self, _at: std::time::Instant, renderer: &'static str) {
        println!("gui: frame presented renderer={renderer}");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let choice: RendererChoice = std::env::var("GOGO_GUI_RENDERER")
        .unwrap_or_else(|_| "auto".to_owned())
        .parse()?;
    let app = App {
        fonts: FontSet::bundled(),
        list: None,
        images: MemoryStore::new(),
    };
    Shell::new()?.run(app, choice)?;
    Ok(())
}

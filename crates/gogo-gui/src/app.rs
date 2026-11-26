//! The application in the shell's window: the frame, the renderers, input and
//! accessibility, and hot reload (design 24 §4, design 25 §4 and §8).
//!
//! It prints a line on stdout when a frame is presented, when the skin is reloaded and when
//! the route changes; `scripts/gui-headless.sh` waits for them.
//!
//! The safety flow's own screens are not drawn yet (#536). An action that needs one is
//! cancelled, and the model is never told that a screen was shown.

use std::borrow::Cow;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gogo_contract::{Value, actions};
use gogo_gui_model::{Effect, InputEvent, Invocation, Outcome};
use gogo_ui_a11y::A11yTree;
use gogo_ui_interact::{Config, Frame as Layers, Intent, Interaction, Primary};
use gogo_ui_scene::DisplayList;
use gogo_ui_shell::accesskit::{ActionRequest, TreeUpdate};
use gogo_ui_shell::{
    Frame, Input, PixelRenderer, RenderError, ShellApp, WindowOptions, WindowRenderer, WindowTarget,
};

use crate::data::{Data, Images};
use crate::page::{DrawError, Page, Screen};
use crate::{act, panel, render};

/// The route the window shows: it draws the model's route in its outlet.
const SHELL_ROUTE: &str = "shell";

const TITLE: &str = "gogo";

#[cfg(target_os = "macos")]
const PRIMARY: Primary = Primary::Meta;
#[cfg(not(target_os = "macos"))]
const PRIMARY: Primary = Primary::Ctrl;

/// A line for whoever watches the run. A closed stream is no reason to stop drawing,
/// which `println!` would do by panicking.
fn say(line: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

pub(crate) struct App {
    page: Page,
    data: Data,
    images: Images,
    list: DisplayList,
    /// The last screen that bound: what input and assistive technology act on.
    screen: Option<Screen>,
    interaction: Interaction,
    a11y: A11yTree,
    scale: f64,
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
            screen: None,
            interaction: Interaction::new(Config::new(PRIMARY)),
            a11y: A11yTree::new(TITLE),
            scale: 1.0,
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
        let result = self.page.reload(dir, &mut self.data, SHELL_ROUTE);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(()) => {
                self.problems.clear();
                self.images_changed = true;
                say(format_args!("gui: skin {done} in {ms:.1} ms"));
            }
            Err(problems) => {
                say(format_args!(
                    "gui: skin reload failed in {ms:.1} ms: {} errors",
                    problems.len()
                ));
                for line in &problems {
                    let _ = writeln!(std::io::stderr(), "{line}");
                }
                self.problems = problems;
            }
        }
    }

    /// Does what one intent asks.
    fn act(&mut self, intent: Intent) {
        let route = self.data.location().route.clone();
        match intent {
            Intent::Invoke {
                action,
                input,
                provenance,
                ..
            } => {
                // Resolved against the screen the press was on, before anything rebinds.
                let call = self.screen.as_ref().and_then(|s| s.actions.get(action));
                let Some(call) = call else { return };
                match act::invocation(call, &self.data.scope(), input.on) {
                    Ok(invocation) => self.invoke(&invocation, act::number(provenance.event())),
                    Err(e) => tracing::warn!("{}: {e}", call.location),
                }
            }
            Intent::Dismiss { provenance } => {
                if self.data.dismiss() == Outcome::Ignored {
                    self.invoke_named("nav.back", act::number(provenance.event()));
                }
            }
            Intent::Palette { provenance } => {
                self.invoke_named("palette.open", act::number(provenance.event()));
            }
            // Enter, holds and app controls answer the app's own screens, none of which
            // is drawn yet; no skin has a text field yet.
            _ => {}
        }
        self.effects();
        if self.data.location().route != route {
            say(format_args!("gui: route {}", self.data.location().route));
        }
    }

    fn invoke_named(&mut self, name: &str, event: InputEvent) {
        if let Some(spec) = actions::action(name) {
            self.invoke(&Invocation::new(spec), event);
        }
    }

    fn invoke(&mut self, invocation: &Invocation, event: InputEvent) {
        let name = invocation.spec.name;
        if let Outcome::Refused(error) = self.data.invoke(invocation, event) {
            tracing::warn!("{name} was refused: {error:?}");
        }
        if self.data.has_modal() {
            say(format_args!(
                "gui: {name} needs a dialog, which is not drawn yet: cancelled"
            ));
            self.data.dismiss();
        }
    }

    fn effects(&mut self) {
        for effect in self.data.take_effects() {
            match effect {
                Effect::SetUi { var, value } => self.set_ui(&var, value),
                // Named only: an effect can carry the user's data.
                Effect::OpenPalette { .. } => {
                    say(format_args!("gui: the palette is not built yet"))
                }
                Effect::AccountMenu => say(format_args!("gui: the account menu is not built yet")),
                Effect::Copy(_) => say(format_args!("gui: the clipboard is not wired in yet")),
                _ => {}
            }
        }
    }

    fn set_ui(&mut self, var: &str, value: Value) {
        if let Err(e) = self.page.set_ui(var, value) {
            tracing::warn!("ui.set: {e}");
        }
    }

    /// Hands `input` to the interaction layer and acts on what comes back. Returns whether
    /// to draw again.
    fn handle(&mut self, input: Input, at: Duration) -> bool {
        let Some(screen) = &self.screen else {
            return false;
        };
        let layers = [screen.layer()];
        let response = self
            .interaction
            .handle(input, at, &Layers { layers: &layers });
        let acted = !response.intents.is_empty();
        for intent in response.intents {
            self.act(intent);
        }
        response.redraw || acted
    }

    /// Binds, lays out and builds the frame for a window of `size` logical px.
    fn draw(&mut self, size: (f32, f32)) {
        let mut problems = Cow::Borrowed(self.problems.as_slice());
        match self.page.screen(&mut self.data, SHELL_ROUTE, size) {
            Ok(screen) => self.screen = Some(screen),
            // The last screen stays, and stays usable: Esc goes back from a route no
            // skin draws.
            Err(e) => problems.to_mut().push(e.to_string()),
        }
        self.list = match &self.screen {
            Some(screen) => {
                let layers = [screen.layer()];
                let frame = Layers { layers: &layers };
                // Focus may have lost its control in the rebuild.
                self.interaction.sync(&frame);
                let focus = self.interaction.focus_visible().map(|t| t.node);
                screen.list(focus)
            }
            None => DisplayList::blank(f64::from(size.0), f64::from(size.1)),
        };
        if !problems.is_empty()
            && let Err(e) = panel::draw(
                &mut self.list,
                &mut self.page.fonts,
                size,
                "Something is wrong with this screen",
                &problems,
            )
        {
            tracing::error!("the error panel could not be drawn: {e}");
        }
    }
}

impl ShellApp for App {
    fn window(&self) -> WindowOptions {
        WindowOptions {
            title: TITLE.to_owned(),
            app_id: "gogo-gui".to_owned(),
            size: (1280.0, 800.0),
            min_size: Some((640.0, 400.0)),
        }
    }

    fn frame(&mut self, width: f32, height: f32, scale: f64) -> Frame<'_> {
        self.scale = scale;
        self.draw((width, height));
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
        let changed = self.data.pump();
        self.effects();
        changed
    }

    fn watch(&self) -> Option<PathBuf> {
        self.dev.clone()
    }

    fn files_changed(&mut self) -> bool {
        self.reload("reloaded");
        true
    }

    fn presented(&mut self, _at: Duration, renderer: &'static str) {
        say(format_args!("gui: frame presented renderer={renderer}"));
    }

    fn input(&mut self, input: Input, at: Duration) -> bool {
        self.handle(input, at)
    }

    fn accessibility_tree(&mut self) -> TreeUpdate {
        match &self.screen {
            Some(screen) => {
                let layers = [screen.layer()];
                let frame = Layers { layers: &layers };
                self.a11y.update(&frame, &self.interaction, self.scale)
            }
            None => self
                .a11y
                .update(&Layers { layers: &[] }, &self.interaction, self.scale),
        }
    }

    fn accessibility_action(&mut self, request: ActionRequest, at: Duration) -> bool {
        match self.a11y.map_request(&request) {
            Some(request) => self.handle(Input::Accessibility(request), at),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use gogo_ui_interact::{Key, Modifiers, PointerButton};
    use gogo_ui_shell::accesskit::{Action, Role, TreeId};

    use super::*;

    const SIZE: (f32, f32) = (1280.0, 800.0);

    fn app() -> App {
        let data = Data::fixture(|| {}).expect("fixture data");
        let mut app = App::new(None, data).expect("the built-in skin");
        app.draw(SIZE);
        app
    }

    /// The middle of the node the skin calls `name`.
    fn middle(app: &App, name: &str) -> (f64, f64) {
        let screen = app.screen.as_ref().expect("a screen");
        let node = screen.tree.by_name(name).expect(name);
        let rect = screen.laid.rect(node).expect("laid out");
        ((rect.x0 + rect.x1) / 2.0, (rect.y0 + rect.y1) / 2.0)
    }

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn click(app: &mut App, name: &str, ms: u64) {
        let (x, y) = middle(app, name);
        let button = PointerButton::Primary;
        app.input(Input::PointerMoved { x, y }, at(ms));
        app.input(Input::PointerDown { button, x, y }, at(ms + 1));
        app.input(Input::PointerUp { button, x, y }, at(ms + 2));
        app.draw(SIZE);
    }

    fn key(app: &mut App, key: Key, ms: u64) {
        let mods = Modifiers::default();
        app.input(
            Input::KeyDown {
                key,
                mods,
                repeat: false,
            },
            at(ms),
        );
        app.input(Input::KeyUp { key }, at(ms + 1));
        app.draw(SIZE);
    }

    #[test]
    fn a_click_goes_to_a_route_and_esc_goes_back() {
        let mut app = app();
        assert_eq!(app.data.location().route, "game");
        let good = app.list.clone();
        // No skin draws the library yet (#576): the last screen stays, with the reason.
        click(&mut app, "nav-library", 10);
        assert_eq!(app.data.location().route, "library");
        assert_ne!(app.list, good);
        key(&mut app, Key::Escape, 20);
        assert_eq!(app.data.location().route, "game");
    }

    #[test]
    fn tab_shows_a_focus_ring() {
        let mut app = app();
        let before = app.list.clone();
        key(&mut app, Key::Tab, 10);
        assert!(app.interaction.focus_visible().is_some());
        assert_ne!(app.list, before);
    }

    /// Presses the control assistive technology sees as `label` with `role`.
    fn press(app: &mut App, role: Role, label: &str, ms: u64) {
        let tree = app.accessibility_tree();
        let (id, _) = tree
            .nodes
            .iter()
            .find(|(_, n)| n.role() == role && n.label() == Some(label))
            .unwrap_or_else(|| panic!("no {role:?} called {label}"));
        let request = ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: *id,
            data: None,
        };
        assert!(app.accessibility_action(request, at(ms)));
        app.draw(SIZE);
    }

    #[test]
    fn a_screen_reader_can_press_a_control() {
        let mut app = app();
        press(&mut app, Role::Button, "Library", 10);
        assert_eq!(app.data.location().route, "library");
    }

    #[test]
    fn a_tab_sets_the_skins_ui_value() {
        let mut app = app();
        let before = app.list.clone();
        assert_eq!(app.page.ui("tab"), Some(&Value::str("overview")));
        press(&mut app, Role::Tab, "Builds", 10);
        assert_eq!(app.page.ui("tab"), Some(&Value::str("builds")));
        assert!(app.problems.is_empty());
        assert_ne!(app.list, before);
        assert_eq!(app.data.location().route, "game");
    }
}

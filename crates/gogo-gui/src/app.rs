//! The application in the shell's window: the frame, the renderers, input and
//! accessibility, and hot reload (design 24 §4, design 25 §4 and §8).
//!
//! It prints a line on stdout when a frame is presented, when the skin is reloaded and when
//! the route changes; `scripts/gui-headless.sh` waits for them.
//!
//! Input and accessibility act only on what was presented: a frame is built as pending and
//! takes over when the shell says it reached the screen.
//!
//! The safety flow's own screens are not drawn yet (#536, #614). Whatever raises one, a control,
//! a job's question or a failed job, it is cancelled at once, which answers a question
//! with a cancel, and the model is never told that a screen was shown.

use std::borrow::Cow;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gogo_contract::{Value, actions};
use gogo_gui_model::{Effect, InputEvent, Invocation, Outcome};
use gogo_ui_a11y::A11yTree;
use gogo_ui_interact::{Config, Frame as Layers, Intent, Interaction, Layer, Primary};
use gogo_ui_scene::DisplayList;
use gogo_ui_shell::accesskit::{ActionRequest, TreeUpdate};
use gogo_ui_shell::{
    Frame, Input, PixelRenderer, RenderError, ShellApp, WindowOptions, WindowRenderer, WindowTarget,
};

use crate::data::{Data, Images};
use crate::page::{DrawError, Page, Screen};
use crate::panel::Panel;
use crate::{act, render};

/// The route the window shows: it draws the model's route in its outlet.
const SHELL_ROUTE: &str = "shell";

const TITLE: &str = "gogo";

/// A bound on cancelling one screen of the safety flow after another.
const MODALS: usize = 64;

#[cfg(target_os = "macos")]
const PRIMARY: Primary = Primary::Meta;
#[cfg(not(target_os = "macos"))]
const PRIMARY: Primary = Primary::Ctrl;

/// A line for whoever watches the run. A closed stream is no reason to stop drawing,
/// which `println!` would do by panicking.
fn say(line: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// What one frame holds that input can land on.
#[derive(Clone, Default)]
struct View {
    /// The last screen that bound.
    screen: Option<Rc<Screen>>,
    /// What is wrong, over it.
    panel: Option<Rc<Panel>>,
    scale: f64,
}

impl View {
    /// The layers, bottom first.
    fn layers(&self) -> Vec<Layer<'_>> {
        let screen = self.screen.iter().map(|s| s.layer());
        screen.chain(self.panel.iter().map(|p| p.layer())).collect()
    }
}

pub(crate) struct App {
    page: Page,
    data: Data,
    images: Images,
    list: DisplayList,
    /// What is on the screen: what input and assistive technology act on.
    shown: View,
    /// What `list` draws, not yet presented.
    pending: View,
    interaction: Interaction,
    a11y: A11yTree,
    /// The kinds of the safety screens cancelled so far.
    #[cfg(test)]
    cancelled: Vec<&'static str>,
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
            shown: View::default(),
            pending: View::default(),
            interaction: Interaction::new(Config::new(PRIMARY)),
            a11y: A11yTree::new(TITLE),
            #[cfg(test)]
            cancelled: Vec::new(),
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
        // Under the error panel only Esc does anything.
        if self.shown.panel.is_some() && !matches!(intent, Intent::Dismiss { .. }) {
            return;
        }
        let route = self.data.location().route.clone();
        match intent {
            Intent::Invoke {
                action,
                input,
                provenance,
                ..
            } => {
                // Resolved against the screen the press was on, before anything rebinds.
                let screen = self.shown.screen.clone();
                let call = screen.as_ref().and_then(|s| s.actions.get(action));
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
        self.cancel_modals();
    }

    /// Cancels every screen the safety flow wants up, as none is drawn yet (#536, #614).
    /// Returns whether there was one.
    fn cancel_modals(&mut self) -> bool {
        let mut any = false;
        for _ in 0..MODALS {
            let Some(kind) = self.data.modal_kind() else {
                break;
            };
            say(format_args!("gui: {kind} is not drawn yet: cancelled"));
            #[cfg(test)]
            self.cancelled.push(kind);
            any = true;
            if self.data.dismiss() != Outcome::Accepted {
                break;
            }
        }
        any
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
        let shown = self.shown.clone();
        let layers = shown.layers();
        if layers.is_empty() {
            return false;
        }
        let response = self
            .interaction
            .handle(input, at, &Layers { layers: &layers });
        let acted = !response.intents.is_empty();
        for intent in response.intents {
            self.act(intent);
        }
        response.redraw || acted
    }

    /// Binds, lays out and builds the next frame for a window of `size` logical px.
    fn draw(&mut self, size: (f32, f32)) {
        let failure = match self.page.screen(&mut self.data, SHELL_ROUTE, size) {
            Ok(screen) => {
                self.pending.screen = Some(Rc::new(screen));
                None
            }
            // The last screen stays in sight, under the panel; Esc goes back.
            Err(e) => Some(e.to_string()),
        };
        // Binding takes what the sources sent, which can raise a safety screen.
        self.cancel_modals();
        let mut problems = Cow::Borrowed(self.problems.as_slice());
        if let Some(failure) = failure {
            problems.to_mut().push(failure);
        }
        self.pending.panel = None;
        if !problems.is_empty() {
            let heading = "Something is wrong with this screen";
            match Panel::new(&mut self.page.fonts, size, heading, &problems) {
                Ok(panel) => self.pending.panel = Some(Rc::new(panel)),
                Err(e) => {
                    tracing::error!("the error panel could not be drawn: {e}");
                    // Without it, nothing may be pressed on a screen that is stale.
                    self.pending.screen = None;
                }
            }
        }
        self.list = match &self.pending.screen {
            Some(screen) => {
                // The ring goes where focus will be once this frame is presented: on the
                // same control of the same screen.
                let focus = self
                    .interaction
                    .focus_visible()
                    .filter(|t| self.pending.panel.is_none() && t.layer == screen.layer().id)
                    .and_then(|t| screen.tree.by_key(&t.key));
                screen.list(focus)
            }
            None => DisplayList::blank(f64::from(size.0), f64::from(size.1)),
        };
        if let Some(panel) = &self.pending.panel {
            panel.draw(&mut self.list);
        }
    }

    /// The pending frame is on the screen. Returns whether focus moved with it, so that it
    /// has to be drawn again.
    fn show(&mut self) -> bool {
        self.shown = self.pending.clone();
        let layers = self.shown.layers();
        self.interaction.sync(&Layers { layers: &layers }).redraw
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
        self.pending.scale = scale;
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
        let cancelled = self.cancel_modals();
        self.effects();
        changed || cancelled
    }

    fn watch(&self) -> Option<PathBuf> {
        self.dev.clone()
    }

    fn files_changed(&mut self) -> bool {
        self.reload("reloaded");
        true
    }

    fn presented(&mut self, _at: Duration, renderer: &'static str) -> bool {
        say(format_args!("gui: frame presented renderer={renderer}"));
        self.show()
    }

    fn input(&mut self, input: Input, at: Duration) -> bool {
        self.handle(input, at)
    }

    fn accessibility_tree(&mut self) -> TreeUpdate {
        let layers = self.shown.layers();
        self.a11y.update(
            &Layers { layers: &layers },
            &self.interaction,
            self.shown.scale,
        )
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
        app.pending.scale = 1.0;
        frame(&mut app);
        app
    }

    /// A frame built and presented.
    fn frame(app: &mut App) {
        app.draw(SIZE);
        app.show();
    }

    /// The middle of the node the skin calls `name`.
    fn middle(app: &App, name: &str) -> (f64, f64) {
        let screen = app.shown.screen.as_ref().expect("a screen");
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
        frame(app);
    }

    /// Where assistive technology sees the control called `label` with `role`.
    fn bounds(app: &mut App, role: Role, label: &str) -> (f64, f64) {
        let tree = app.accessibility_tree();
        let rect = tree
            .nodes
            .iter()
            .find(|(_, n)| n.role() == role && n.label() == Some(label))
            .and_then(|(_, n)| n.bounds())
            .unwrap_or_else(|| panic!("no {role:?} called {label}"));
        ((rect.x0 + rect.x1) / 2.0, (rect.y0 + rect.y1) / 2.0)
    }

    /// A pointer click at a point, without building a frame.
    fn click_at(app: &mut App, (x, y): (f64, f64), ms: u64) {
        let button = PointerButton::Primary;
        app.input(Input::PointerMoved { x, y }, at(ms));
        app.input(Input::PointerDown { button, x, y }, at(ms + 1));
        app.input(Input::PointerUp { button, x, y }, at(ms + 2));
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
        frame(app);
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
        frame(app);
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

    #[test]
    fn a_jobs_question_is_cancelled_when_it_arrives() {
        use std::sync::{Arc, Mutex};

        use gogo_gui_model::{PromptReply, PromptSpec};

        let mut app = app();
        press(&mut app, Role::Button, "Verify files", 10);
        assert_eq!(app.data.backend().runs.len(), 1);
        let answers = Arc::new(Mutex::new(Vec::new()));
        let heard = Arc::clone(&answers);
        app.data.backend().sink(0).ask(
            PromptSpec::Confirm {
                label: "Replace the files?".to_owned(),
            },
            PromptReply::new(move |answer| heard.lock().expect("not poisoned").push(answer)),
        );
        assert!(app.wake());
        assert_eq!(app.cancelled, ["a prompt"]);
        // No answer is a cancel.
        assert!(matches!(answers.lock().expect("not poisoned")[..], [None]));
        assert_eq!(app.data.modal_kind(), None);
        frame(&mut app);
        // The next click does what it says, and nothing else ran.
        let tab = bounds(&mut app, Role::Tab, "Builds");
        click_at(&mut app, tab, 20);
        assert_eq!(app.page.ui("tab"), Some(&Value::str("builds")));
        assert_eq!(app.data.backend().runs.len(), 1);
        assert_eq!(app.cancelled, ["a prompt"]);
    }

    #[test]
    fn an_action_that_needs_a_dialog_runs_nothing() {
        let mut app = app();
        press(&mut app, Role::Tab, "Snapshots", 10);
        let create = bounds(&mut app, Role::Button, "Create snapshot");
        click_at(&mut app, create, 20);
        assert_eq!(app.cancelled, ["a prompt"]);
        assert_eq!(app.data.modal_kind(), None);
        assert!(app.data.backend().runs.is_empty());
    }

    #[test]
    fn the_error_panel_takes_the_input() {
        let mut app = app();
        let verify = bounds(&mut app, Role::Button, "Verify files");
        click(&mut app, "nav-library", 10);
        assert_eq!(app.data.location().route, "library");
        assert!(app.shown.panel.is_some());
        let tree = app.accessibility_tree();
        let heard = format!("{tree:?}");
        assert!(
            heard.contains("Something is wrong with this screen"),
            "{heard}"
        );
        assert!(heard.contains("library"), "{heard}");
        assert!(
            !tree
                .nodes
                .iter()
                .any(|(_, n)| n.label() == Some("Verify files")),
            "the stale page is not exposed"
        );
        // On the stale button, and on the panel itself.
        for (i, point) in [verify, (200.0, f64::from(SIZE.1) - 20.0)]
            .into_iter()
            .enumerate()
        {
            click_at(&mut app, point, 20 + 10 * i as u64);
        }
        key(&mut app, Key::Enter, 50);
        assert!(app.data.backend().runs.is_empty());
        assert_eq!(app.data.location().route, "library");
        key(&mut app, Key::Escape, 60);
        assert_eq!(app.data.location().route, "game");
        assert!(app.shown.panel.is_none());
    }

    #[test]
    fn input_acts_on_what_was_presented() {
        let mut app = app();
        let verify = bounds(&mut app, Role::Button, "Verify files");
        let snapshots = bounds(&mut app, Role::Tab, "Snapshots");
        // The tab is pressed and the next frame built, but it never reaches the screen: a
        // skipped frame, or one that failed to present.
        click_at(&mut app, snapshots, 10);
        app.draw(SIZE);
        let tree = app.accessibility_tree();
        let labelled = |label: &str| tree.nodes.iter().any(|(_, n)| n.label() == Some(label));
        assert!(!labelled("Create snapshot"), "not on screen yet");
        click_at(&mut app, verify, 20);
        assert_eq!(app.data.backend().runs.len(), 1, "the old screen's button");
        // Presented, the new screen takes over.
        frame(&mut app);
        let tree = app.accessibility_tree();
        assert!(
            tree.nodes
                .iter()
                .any(|(_, n)| n.label() == Some("Create snapshot"))
        );
    }

    #[test]
    fn nothing_acts_before_the_first_frame_is_presented() {
        let data = Data::fixture(|| {}).expect("fixture data");
        let mut app = App::new(None, data).expect("the built-in skin");
        app.draw(SIZE);
        let escape = Input::KeyDown {
            key: Key::Escape,
            mods: Modifiers::default(),
            repeat: false,
        };
        assert!(!app.input(escape, at(1)));
        assert!(app.accessibility_tree().nodes.len() <= 1);
    }

    #[test]
    fn presenting_the_same_frame_again_asks_for_nothing() {
        let mut app = app();
        key(&mut app, Key::Tab, 10);
        app.draw(SIZE);
        assert!(!app.show());
    }
}

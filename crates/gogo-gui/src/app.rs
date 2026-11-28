//! The application in the shell's window: the frame, the renderers, input and
//! accessibility, and hot reload (design 24 §4, design 25 §4 and §8).
//!
//! It prints a line on stdout when a frame is presented, when the skin is reloaded and when
//! the route changes; `scripts/gui-headless.sh` waits for them.
//!
//! Input and accessibility act only on what was presented: a frame is built as pending and
//! takes over when the shell says it reached the screen.
//!
//! While a source the route reads has no value yet, the app's own "Loading" panel is
//! over the last screen.
//!
//! Every screen of the safety flow is the app's own modal dialog ([`crate::dialog`]),
//! wired in [`answer`]. None is cancelled except by the user.
//!
//! With `--dev`, F12 opens the inspector ([`crate::inspector`]) beside the page.
//!
//! A skin's menus and selects open the app's popup ([`crate::popup`]) over the page, and
//! Ctrl K the app's command palette ([`crate::palette`]). A safety screen or a panel
//! always wins: it closes the popup, which does not come back.

mod answer;
#[cfg(test)]
mod choosing;
#[cfg(test)]
mod commands;
#[cfg(test)]
mod dev;
#[cfg(test)]
mod idle;
mod import;
#[cfg(test)]
mod importing;
#[cfg(test)]
mod menus;
#[cfg(test)]
mod safety;

use std::borrow::Cow;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gogo_contract::{Value, actions};
use gogo_gui_model::{Effect, ErrorView, InputEvent, Invocation, Outcome, Ticket};
use gogo_skin::ActionCall;
use gogo_ui_a11y::A11yTree;
use gogo_ui_interact::{Config, Frame as Layers, Intent, Interaction, Layer, PopupStack, Primary};
use gogo_ui_scene::DisplayList;
use gogo_ui_shell::accesskit::{ActionRequest, TreeUpdate};
use gogo_ui_shell::{
    Frame, ImeArea, Input, PixelRenderer, RenderError, ShellApp, WindowOptions, WindowRenderer,
    WindowTarget, Zeroizing,
};
use gogo_ui_text::EditOp;

use crate::check;
use crate::data::{Data, Images, SkinSetting};
use crate::dialog::{self, Dialog};
use crate::inspector::{self, Inspector};
use crate::page::{DrawError, Page, Screen};
use crate::palette;
use crate::panel::Panel;
use crate::popup;
use crate::skins::{self, DEFAULT, Skins};
use crate::{act, render};

/// The route the window shows: it draws the model's route in its outlet.
const SHELL_ROUTE: &str = "shell";

const TITLE: &str = "gogo";

#[cfg(target_os = "macos")]
const PRIMARY: Primary = Primary::Meta;
#[cfg(not(target_os = "macos"))]
const PRIMARY: Primary = Primary::Ctrl;

/// What was said, for the test that looks for secrets in it.
#[cfg(test)]
pub(crate) static SAID: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// A line for whoever watches the run. A closed stream is no reason to stop drawing,
/// which `println!` would do by panicking.
fn say(line: std::fmt::Arguments<'_>) {
    #[cfg(test)]
    if let Ok(mut said) = SAID.lock() {
        said.push(line.to_string());
    }
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// What the pending frame was built from, besides the safety screen.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Built {
    /// [`Data::revision`] as its bind read it.
    revision: u64,
    /// Whether it says "Signing in…", which no source tells.
    signing_in: bool,
}

/// What is open over the page.
enum Over {
    /// A menu or a select's list, by its control.
    Anchored(popup::Open),
    Palette(Box<palette::Open>),
}

/// What one frame holds that input can land on.
#[derive(Clone, Default)]
struct View {
    /// The last screen that bound.
    screen: Option<Rc<Screen>>,
    /// The inspector, beside it.
    inspector: Option<Rc<inspector::Drawn>>,
    /// A menu, a select's list or the palette, over it.
    popup: Option<Rc<popup::Drawn>>,
    /// What is wrong, over them.
    panel: Option<Rc<Panel>>,
    /// The app's own dialog, over everything.
    dialog: Option<Rc<Dialog>>,
    /// The screen whose dialog could not be built: the panel stands for it.
    stuck: Option<Ticket>,
    scale: f64,
    /// The window's size in logical px.
    size: (f32, f32),
}

impl View {
    /// The layers, bottom first.
    fn layers(&self) -> Vec<Layer<'_>> {
        let screen = self.screen.iter().map(|s| s.layer());
        screen
            .chain(self.inspector.iter().map(|i| i.layer()))
            .chain(self.popup.iter().map(|p| p.layer()))
            .chain(self.panel.iter().map(|p| p.layer()))
            .chain(self.dialog.iter().map(|d| d.layer()))
            .collect()
    }
}

use answer::{Entry, Holding, Shown};

pub(crate) struct App {
    page: Page,
    data: Data,
    images: Images,
    list: DisplayList,
    /// What is on the screen: what input and assistive technology act on.
    shown: View,
    /// What `list` draws, not yet presented.
    pending: View,
    built: Built,
    /// A frame was built and has not reached the display: it was skipped or failed.
    unpresented: bool,
    interaction: Interaction,
    /// What is open over the page.
    popups: PopupStack<Over>,
    a11y: A11yTree,
    entry: Entry,
    /// Since when a dialog has been on the display.
    since: Option<Shown>,
    /// A press asked to paste into the dialog of this ticket.
    paste: Option<Ticket>,
    /// What a press copied from a field, for the clipboard.
    copied: Option<Zeroizing<String>>,
    /// A press asked to paste into the palette's field.
    paste_search: bool,
    /// The hold the model has accepted, while it is down.
    holding: Option<Holding>,
    /// When the last frame reached the display.
    now: Option<Duration>,
    /// The screen that shows a plan because the one confirmed changed.
    changed: Option<Ticket>,
    /// The layer of last resort, built while building still works.
    fallback: Rc<Panel>,
    /// Makes building a dialog fail.
    #[cfg(test)]
    break_dialogs: bool,
    /// Makes building the panel that says so fail.
    #[cfg(test)]
    break_panels: bool,
    /// The skin folder being watched.
    dev: Option<PathBuf>,
    /// The inspector, while it is open. Only a dev run has one.
    inspector: Option<Inspector>,
    /// Why the skin on screen is not the one in the folder, one line each.
    problems: Vec<String>,
    /// A reload may have changed what an image key names.
    images_changed: bool,
    /// The installed skins, when there is a place for them.
    skins: Option<Skins>,
    /// The name of the skin drawn, outside dev mode.
    on_screen: String,
    /// Where a skin import is, and the question it waits on.
    importing: Option<(Ticket, import::Step)>,
}

impl App {
    pub(crate) fn new(dev: Option<&Path>, data: Data) -> Result<App, DrawError> {
        let mut page = Page::new()?;
        let fallback = Panel::bare(
            &mut page.fonts,
            "A question could not be shown. Press Escape to cancel it.",
        )?;
        let mut app = App {
            fallback: Rc::new(fallback),
            page,
            images: data.images(),
            data,
            list: DisplayList::blank(0.0, 0.0),
            shown: View::default(),
            pending: View::default(),
            built: Built::default(),
            unpresented: false,
            interaction: Interaction::new(Config::new(PRIMARY)),
            popups: PopupStack::default(),
            a11y: A11yTree::new(TITLE),
            entry: Entry::default(),
            since: None,
            paste: None,
            copied: None,
            paste_search: false,
            holding: None,
            now: None,
            changed: None,
            #[cfg(test)]
            break_dialogs: false,
            #[cfg(test)]
            break_panels: false,
            dev: dev.map(Path::to_owned),
            inspector: None,
            problems: Vec::new(),
            images_changed: false,
            skins: None,
            on_screen: DEFAULT.to_owned(),
            importing: None,
        };
        if app.dev.is_some() {
            app.reload("loaded");
        }
        Ok(app)
    }

    /// Takes the skin folder and draws a skin from it: `once` for this run only, else
    /// the one chosen before. A dev run keeps to its folder.
    pub(crate) fn with_skins(mut self, skins: Option<Skins>, once: Option<&str>) -> App {
        self.skins = skins;
        if self.dev.is_some() {
            return self;
        }
        let name = once
            .map(str::to_owned)
            .or_else(|| self.skins.as_ref().and_then(Skins::chosen))
            .unwrap_or_else(|| DEFAULT.to_owned());
        // The choice stays, so that a mended skin comes back.
        if name != DEFAULT
            && let Err(why) = self.show_skin(&name)
        {
            self.skin_failed(&format!("{why}. The Default skin is drawn."));
        }
        self.offer_skins();
        self
    }

    /// Draws with the skin called `name`. What is on screen stays if it can't.
    fn show_skin(&mut self, name: &str) -> Result<(), String> {
        if name == DEFAULT {
            self.page.use_default();
        } else {
            // Only a name finds a folder, whatever a skin asked for (S3).
            let dir = self
                .skins
                .as_ref()
                .and_then(|s| s.folder.path(name))
                .ok_or_else(|| match skins::named(name) {
                    true => format!("The skin {name} is not installed"),
                    false => "There is no such skin".to_owned(),
                })?;
            self.page.choose(&dir).map_err(|lines| {
                let first = lines.first().map(String::as_str).unwrap_or_default();
                format!("The skin {name} does not load: {first}")
            })?;
        }
        self.on_screen = name.to_owned();
        // It hung from a control of the old skin.
        self.popups.clear();
        self.images_changed = true;
        say(format_args!("gui: skin chosen {name}"));
        Ok(())
    }

    /// `settings.set gui.skin`: draws with `name` and keeps the choice.
    fn set_skin(&mut self, name: &str) {
        if self.dev.is_some() {
            self.skin_failed("Dev mode draws the folder it was started with.");
            return;
        }
        match self.show_skin(name) {
            Ok(()) => {
                if let Some(skins) = &self.skins
                    && let Err(e) = skins.choose(name)
                {
                    self.skin_failed(&format!("The choice could not be saved: {e}"));
                }
            }
            Err(why) => self.skin_failed(&why),
        }
        self.offer_skins();
    }

    fn skin_failed(&mut self, why: &str) {
        let title = "The skin can't be used";
        self.data.failures().report(ErrorView::failed(title, why));
        // Taken now, for the next frame to show.
        self.data.pump();
    }

    /// Tells the `settings` source which skin is drawn and which there are.
    fn offer_skins(&mut self) {
        if self.dev.is_some() {
            return;
        }
        let choices = self
            .skins
            .as_ref()
            .map_or_else(|| vec![DEFAULT.to_owned()], Skins::choices);
        let stored = self.skins.as_ref().and_then(Skins::chosen).is_some();
        self.data.offer_skin(SkinSetting {
            value: self.on_screen.clone(),
            choices,
            stored,
            importable: self.skins.is_some(),
        });
    }

    /// Loads the watched folder again, saying how it went.
    fn reload(&mut self, done: &str) {
        let Some(dir) = &self.dev else { return };
        let start = Instant::now();
        let result = self.page.reload(dir, &mut self.data, SHELL_ROUTE);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        // The window lists them in the inspector.
        for line in self
            .page
            .report
            .as_ref()
            .map(check::warning_lines)
            .unwrap_or_default()
        {
            let _ = writeln!(std::io::stderr(), "{line}");
        }
        match result {
            Ok(()) => {
                self.problems.clear();
                self.images_changed = true;
                // It hung from a control of the old skin.
                self.popups.clear();
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

    /// Whether the inspector is on the display, over a traced page.
    #[cfg(test)]
    pub(crate) fn inspecting(&self) -> bool {
        let screen = self.shown.screen.as_ref();
        self.shown.inspector.is_some() && screen.is_some_and(|s| s.provenance.is_some())
    }

    /// The text of everything on the display, for the test that looks for secrets.
    #[cfg(test)]
    pub(crate) fn dump(&self) -> String {
        let mut out = format!("{:?}", self.problems);
        if let Some(screen) = &self.shown.screen {
            out += &format!("{:?}", screen.tree);
        }
        if let Some(inspector) = &self.shown.inspector {
            out += &format!("{:?}", inspector.tree());
        }
        if let Some(popup) = &self.shown.popup {
            out += &format!("{:?}", popup.tree());
        }
        if let Some(panel) = &self.shown.panel {
            out += &format!("{:?}", panel.tree());
        }
        if let Some(dialog) = &self.shown.dialog {
            out += &format!("{:?}", dialog.tree());
        }
        out
    }

    /// Does what one intent asks.
    fn act(&mut self, intent: Intent) {
        // A hold ends whatever is on the display by now.
        if let Intent::HoldEnd { .. } = intent {
            self.stop_hold();
            return;
        }
        if let Some(dialog) = self.shown.dialog.clone() {
            self.act_on_dialog(&dialog, intent);
            self.effects();
            return;
        }
        // A screen of the safety flow that is not on the display yet takes nothing, Esc
        // included: it would cancel what the user has not seen. One that could not be
        // built is cancelled by Esc on the panel that says so.
        if let Some(ticket) = self.data.modal_ticket() {
            if self.shown.stuck == Some(ticket) && matches!(intent, Intent::Dismiss { .. }) {
                self.close(ticket);
            }
            return;
        }
        // Under the error panel only Esc does anything.
        if self.shown.panel.is_some() && !matches!(intent, Intent::Dismiss { .. }) {
            return;
        }
        if self.act_on_inspector(&intent) {
            return;
        }
        let route = self.data.location().route.clone();
        let open = self.popups.top().map(|(id, _)| id);
        match (open, self.shown.popup.clone()) {
            (None, None) => self.act_on_page(intent),
            (Some(id), Some(popup)) if popup.id == id => self.act_on_popup(&popup, intent),
            // A popup is on its way to the display or off it: what was pressed was meant
            // for what is still there, so it does nothing, but for Esc closing the popup.
            _ => {
                if matches!(intent, Intent::Dismiss { .. }) {
                    self.popups.dismiss_top();
                }
            }
        }
        self.effects();
        if self.data.location().route != route {
            say(format_args!("gui: route {}", self.data.location().route));
        }
    }

    /// What an intent asks of the inspector: whether it was for it. Nothing here runs an
    /// action. Under a popup the popup takes it all.
    fn act_on_inspector(&mut self, intent: &Intent) -> bool {
        if self.dev.is_none() || self.shown.popup.is_some() || self.popups.top().is_some() {
            return false;
        }
        // While picking, or while the frame that says so is up, the page gets nothing.
        let picking = self.inspector.as_ref().is_some_and(Inspector::picking)
            || self.shown.inspector.as_ref().is_some_and(|d| d.picking);
        match intent {
            Intent::Dismiss { .. } if picking => {
                if let Some(inspector) = &mut self.inspector {
                    inspector.stop_picking();
                }
                true
            }
            Intent::Inspect => {
                self.inspector = match self.inspector {
                    Some(_) => None,
                    None => Some(Inspector::new()),
                };
                let now = if self.inspector.is_some() {
                    "opened"
                } else {
                    "closed"
                };
                say(format_args!("gui: inspector {now}"));
                true
            }
            Intent::App { target, .. } => {
                let (Some(inspector), Some(drawn)) = (&mut self.inspector, &self.shown.inspector)
                else {
                    return picking;
                };
                // A press elsewhere: the page's, which gets nothing while picking.
                if target.layer != drawn.id {
                    return picking;
                }
                if inspector.press(drawn, &target.key).is_some() {
                    self.say_selected();
                } else if inspector.picking() {
                    say(format_args!("gui: inspector picking"));
                }
                true
            }
            _ => picking,
        }
    }

    /// Follows the pointer while picking. Returns whether the outline moved.
    fn point_inspector(&mut self, input: &Input) -> bool {
        let point = match *input {
            Input::PointerMoved { x, y } | Input::PointerDown { x, y, .. } => Some((x, y)),
            Input::PointerLeft => None,
            _ => return false,
        };
        // Only what the presented frame shows as picking, with nothing over it.
        if !self.shown.inspector.as_ref().is_some_and(|d| d.picking)
            || self.shown.panel.is_some()
            || self.shown.dialog.is_some()
        {
            return false;
        }
        let (Some(inspector), Some(screen)) = (&mut self.inspector, &self.shown.screen) else {
            return false;
        };
        let page = f64::from(self.shown.size.0 - inspector::width(self.shown.size.0));
        inspector.point(screen, point.filter(|&(x, _)| x < page))
    }

    /// Names the selected node: its key and where the skin has it, never what it shows.
    fn say_selected(&self) {
        let Some(inspector) = &self.inspector else {
            return;
        };
        let key = inspector.selected();
        let location = self.shown.screen.as_ref().and_then(|screen| {
            let node = screen.tree.by_key(key)?;
            Some(&screen.provenance.as_ref()?.get(node)?.location)
        });
        let location = location.map_or("-", String::as_str);
        say(format_args!("gui: inspector selected {key} {location}"));
    }

    /// What an intent asks of the page.
    fn act_on_page(&mut self, intent: Intent) {
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
                let input = gogo_skin::Input {
                    on: input.on,
                    value: None,
                };
                self.invoke_call(call, &input, act::number(provenance.event()));
            }
            Intent::Open { target, .. } => {
                let screen = self.shown.screen.as_ref();
                if let Some(page) = screen.map(|s| s.layer().id)
                    && page == target.layer
                {
                    self.popups.clear();
                    let open = popup::Open::new(page, target.key, self.shown.size);
                    self.popups.open(Over::Anchored(open));
                }
            }
            Intent::Dismiss { provenance } => {
                self.invoke_named("nav.back", act::number(provenance.event()));
            }
            Intent::Palette { provenance } => {
                self.invoke_named("palette.open", act::number(provenance.event()));
            }
            // Enter, holds and text belong to dialogs; no skin has a text field yet.
            _ => {}
        }
    }

    /// What an intent asks of the popup `drawn`, which takes input alone.
    fn act_on_popup(&mut self, drawn: &popup::Drawn, intent: Intent) {
        match intent {
            Intent::Dismiss { .. } => {
                self.popups.dismiss_top();
            }
            Intent::Palette { provenance } => {
                // Over the palette it closes it; over a menu it opens the palette.
                let palette = self.palette().is_some();
                self.popups.clear();
                if !palette {
                    self.invoke_named("palette.open", act::number(provenance.event()));
                }
            }
            Intent::App {
                target, provenance, ..
            } => self.choose(drawn, &target.key, act::number(provenance.event())),
            Intent::Move { step, .. } => {
                if let Some(palette) = self.palette() {
                    palette.step(step);
                }
            }
            Intent::Edit { target, edit } if target.key == palette::FIELD => {
                if let Some(op) = answer::edit_op(&edit, |x, y| drawn.field().point(x, y)) {
                    self.edit_search(op);
                }
            }
            Intent::SetValue { target, value } if target.key == palette::FIELD => {
                self.edit_search(EditOp::Set(&value));
            }
            Intent::Copy { target, cut, .. } if target.key == palette::FIELD => {
                self.copy_search(cut);
            }
            Intent::Paste { .. } => self.paste_search = self.palette().is_some(),
            _ => {}
        }
    }

    /// Edits the palette's field. Whether the palette is open to take it.
    fn edit_search(&mut self, op: EditOp<'_>) -> bool {
        let Some(Over::Palette(palette)) = self.popups.top_mut() else {
            return false;
        };
        palette.edit(&mut self.page.fonts, op);
        true
    }

    /// Takes the selection of the palette's field for the clipboard.
    fn copy_search(&mut self, cut: bool) {
        let Some(text) = self.palette().and_then(|p| p.selected()) else {
            return;
        };
        let text = Zeroizing::new(text.to_owned());
        self.copied = Some(text);
        if cut {
            self.edit_search(EditOp::Delete {
                forward: false,
                word: false,
            });
        }
    }

    /// The palette, when it is the open popup.
    fn palette(&mut self) -> Option<&mut palette::Open> {
        match self.popups.top_mut() {
            Some(Over::Palette(palette)) => Some(palette),
            _ => None,
        }
    }

    /// A row or a button of the popup `drawn` was activated.
    fn choose(&mut self, drawn: &popup::Drawn, key: &str, event: InputEvent) {
        if matches!(key, popup::EARLIER | popup::LATER) {
            match self.popups.top_mut() {
                Some(Over::Anchored(open)) => open.turn(key),
                Some(Over::Palette(palette)) => palette.turn(key),
                None => {}
            }
            return;
        }
        // The search or the highlight changed since this frame was built: the press
        // was made on a list that is not the one asked for any more.
        if self
            .palette()
            .is_some_and(|p| p.revision() != drawn.revision)
        {
            return;
        }
        let Some(choice) = drawn.choice(key).cloned() else {
            return;
        };
        // Closed first: whatever the call raises is over the page.
        self.popups.clear();
        match choice {
            popup::Choice::Call { call, value } => {
                let input = gogo_skin::Input { on: None, value };
                self.invoke_call(&call, &input, event);
            }
            popup::Choice::Command(invocation) => self.invoke(&invocation, event),
            popup::Choice::ImportSkin => self.start_import(),
        }
    }

    /// Runs a skin's call: of a control, or of a popup's entry.
    fn invoke_call(&mut self, call: &ActionCall, input: &gogo_skin::Input, event: InputEvent) {
        match act::invocation(call, &self.data.scope(), input) {
            Ok(invocation) => self.invoke(&invocation, event),
            Err(e) => tracing::warn!("{}: {e}", call.location),
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
    }

    fn effects(&mut self) {
        for effect in self.data.take_effects() {
            match effect {
                Effect::SetUi { var, value } => self.set_ui(&var, value),
                Effect::SetSkin(name) => self.set_skin(&name),
                Effect::Answered { ticket, answer } => self.import_answered(ticket, answer),
                // Named only: an effect can carry the user's data.
                // The scope is not used: the palette lists everything.
                Effect::OpenPalette { .. } => {
                    self.popups.clear();
                    let open = palette::Open::new(&mut self.page.fonts);
                    self.popups.open(Over::Palette(Box::new(open)));
                    say(format_args!("gui: palette opened"));
                }
                Effect::AccountMenu => say(format_args!("gui: the account menu is not built yet")),
                Effect::Copy(_) => say(format_args!("gui: the clipboard is not wired in yet")),
                // The new card is on top.
                Effect::PlanChanged { .. } => self.changed = self.data.modal_ticket(),
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
        let pointed = self.point_inspector(&input);
        let response = self
            .interaction
            .handle(input, at, &Layers { layers: &layers });
        let acted = !response.intents.is_empty();
        for intent in response.intents {
            self.act(intent);
        }
        // Any input tries again for a frame that never reached the display.
        response.redraw || acted || pointed || self.unpresented || self.stale()
    }

    /// Binds, lays out and builds the next frame for a window of `size` logical px.
    fn draw(&mut self, size: (f32, f32)) {
        let mut loading = false;
        let scrolled = Some(&self.interaction);
        // The inspector takes its width from the page rather than covering it.
        self.page.traced = self.inspector.is_some();
        let beside = if self.page.traced {
            inspector::width(size.0)
        } else {
            0.0
        };
        let page = (size.0 - beside, size.1);
        let failure = match self
            .page
            .screen(&mut self.data, SHELL_ROUTE, page, scrolled)
        {
            Ok(screen) => {
                self.pending.screen = Some(Rc::new(screen));
                None
            }
            // The last screen stays in sight under the app's own "Loading".
            Err(DrawError::Loading) => {
                loading = true;
                None
            }
            // The last screen stays in sight, under the panel; Esc goes back.
            Err(e) => Some(e.to_string()),
        };
        // Read before anything else looks at the data: what changes from here on is not in
        // this frame.
        self.built = Built {
            revision: self.data.read_at(),
            signing_in: self.data.signing_in(),
        };
        // Binding takes what the sources sent, which can raise a safety screen.
        self.effects();
        let mut problems = Cow::Borrowed(self.problems.as_slice());
        if let Some(failure) = failure {
            problems.to_mut().push(failure);
        }
        self.pending.panel = None;
        let waiting = if self.built.signing_in {
            Some("Signing in…")
        } else {
            loading.then_some("Loading…")
        };
        if let Some(waiting) = waiting.filter(|_| problems.is_empty()) {
            match Panel::waiting(&mut self.page.fonts, size, waiting) {
                Ok(panel) => self.pending.panel = Some(Rc::new(panel)),
                Err(e) => {
                    tracing::error!("the loading panel could not be drawn: {e}");
                    self.pending.screen = None;
                }
            }
        } else if !problems.is_empty() {
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
        // A safety screen or a panel wins over a popup, for good.
        if self.data.modal_ticket().is_some() || self.pending.panel.is_some() {
            self.popups.clear();
            if let Some(inspector) = &mut self.inspector {
                inspector.stop_picking();
            }
        }
        self.pending.size = size;
        self.pending.inspector = self.build_inspector(size);
        self.pending.popup = self.build_popup(size);
        self.list = match &self.pending.screen {
            Some(screen) => {
                // The ring goes where focus will be once this frame is presented: on the
                // same control of the same screen.
                let focus = self
                    .interaction
                    .focus_visible()
                    .filter(|t| {
                        self.pending.panel.is_none()
                            && self.pending.popup.is_none()
                            && self.data.modal_ticket().is_none()
                            && t.layer == screen.layer().id
                    })
                    .and_then(|t| screen.tree.by_key(&t.key));
                screen.list(focus)
            }
            None => DisplayList::blank(f64::from(size.0), f64::from(size.1)),
        };
        if let Some(inspector) = &self.pending.inspector {
            // The page was laid out narrower than the frame.
            (self.list.width, self.list.height) = (f64::from(size.0), f64::from(size.1));
            let focus = self
                .interaction
                .focus_visible()
                .filter(|t| {
                    t.layer == inspector.id
                        && self.pending.panel.is_none()
                        && self.pending.popup.is_none()
                        && self.data.modal_ticket().is_none()
                })
                .map(|t| t.key.as_str());
            let outline = self.pending.panel.is_none()
                && self.pending.popup.is_none()
                && self.data.modal_ticket().is_none();
            inspector.draw(&mut self.list, focus, outline);
        }
        if let Some(popup) = &self.pending.popup {
            // A text field shows where typing goes however it got focus.
            let focus = self
                .interaction
                .focus_visible()
                .or_else(|| self.interaction.focus().filter(|t| t.key == palette::FIELD))
                .filter(|t| t.layer == popup.id)
                .map(|t| t.key.as_str());
            popup.draw(&mut self.list, focus);
        }
        if let Some(panel) = &self.pending.panel {
            panel.draw(&mut self.list);
        }
        self.draw_dialog(size);
        self.unpresented = true;
    }

    /// The inspector beside the screen about to be shown, while it is open.
    fn build_inspector(&mut self, size: (f32, f32)) -> Option<Rc<inspector::Drawn>> {
        let inspector = self.inspector.as_mut()?;
        let screen = self.pending.screen.as_deref();
        let findings = inspector::Findings::of(self.page.report.as_ref(), &self.problems, screen);
        match inspector.draw(&mut self.page.fonts, size, screen, &findings) {
            Ok(drawn) => Some(Rc::new(drawn)),
            Err(e) => {
                tracing::error!("the inspector could not be drawn: {e}");
                None
            }
        }
    }

    /// The open popup over the screen about to be shown. One that can't be drawn over
    /// it is closed.
    fn build_popup(&mut self, size: (f32, f32)) -> Option<Rc<popup::Drawn>> {
        let (id, over) = self.popups.top()?;
        let commands = match over {
            Over::Palette(_) => palette::commands(&mut self.data),
            Over::Anchored(_) => Vec::new(),
        };
        let screen = self.pending.screen.clone();
        let focus = self
            .interaction
            .focus()
            .filter(|t| t.layer == id)
            .map(|t| t.key.clone());
        let fonts = &mut self.page.fonts;
        let drawn = match (self.popups.top_mut(), screen) {
            (Some(Over::Anchored(open)), Some(screen)) if open.size == size => {
                popup::Drawn::new(fonts, open, id, &screen, focus.as_deref())
            }
            (Some(Over::Palette(open)), screen) => {
                let page = screen.map(|s| s.layer().id);
                // The caret shows in the field however it got focus, from the start.
                let focused = focus.as_deref().is_none_or(|key| key == palette::FIELD);
                let drawn = open.draw(fonts, id, page, size, commands, focused);
                if let Ok(Some(drawn)) = &drawn
                    && let Some(scroll) = drawn.field().scroll()
                {
                    open.scroll = scroll;
                }
                drawn
            }
            _ => Ok(None),
        };
        match drawn {
            Ok(Some(drawn)) => Some(Rc::new(drawn)),
            Ok(None) => {
                self.popups.clear();
                None
            }
            Err(e) => {
                tracing::error!("a popup could not be drawn: {e}");
                self.popups.clear();
                None
            }
        }
    }

    /// The pending frame is on the screen, since `at`. Returns whether to draw again.
    fn show(&mut self, at: Duration) -> bool {
        self.unpresented = false;
        let redraw = self.take_over();
        let again = self.shown_screen(at);
        redraw || again || self.stale()
    }

    /// The pending frame becomes the one input acts on. Returns whether focus moved.
    fn take_over(&mut self) -> bool {
        self.shown = self.pending.clone();
        let shown = self.shown.clone();
        let layers = shown.layers();
        let response = self.interaction.sync(&Layers { layers: &layers });
        // A hold ends when its control moved or changed with this frame.
        for intent in response.intents {
            self.act(intent);
        }
        response.redraw
    }

    /// Whether what the last frame was built from has changed: the data, a sign-in, or
    /// the safety flow's screen on top.
    fn stale(&self) -> bool {
        let dialog = self
            .pending
            .dialog
            .as_ref()
            .map(|d| d.ticket())
            .or(self.pending.stuck);
        let now = Built {
            revision: self.data.revision(),
            signing_in: self.data.signing_in(),
        };
        self.built != now || self.data.modal_ticket() != dialog
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

    fn close_requested(&mut self) -> bool {
        !self.data.refuse_close()
    }

    fn wake(&mut self) -> bool {
        self.data.pump();
        self.effects();
        // A question or an error that arrived by itself changes no source.
        self.stale()
    }

    fn watch(&self) -> Option<PathBuf> {
        self.dev.clone()
    }

    fn files_changed(&mut self) -> bool {
        self.reload("reloaded");
        true
    }

    fn presented(&mut self, at: Duration, renderer: &'static str) -> bool {
        say(format_args!("gui: frame presented renderer={renderer}"));
        self.show(at)
    }

    fn wants_paste(&mut self) -> bool {
        self.paste.is_some() || self.paste_search
    }

    fn ime_area(&self) -> Option<ImeArea> {
        let focus = self.interaction.focus()?;
        // The dialog's field, or with no dialog the palette's.
        let caret = if let Some(dialog) = &self.shown.dialog {
            (focus.key == dialog::FIELD && focus.layer == dialog.layer().id)
                .then(|| dialog.field_caret())??
        } else {
            let popup = self.shown.popup.as_ref()?;
            (focus.key == palette::FIELD && focus.layer == popup.id)
                .then(|| popup.field().caret())??
        };
        Some(ImeArea {
            x: caret.x0.into(),
            y: caret.y0.into(),
            width: (caret.x1 - caret.x0).into(),
            height: (caret.y1 - caret.y0).into(),
        })
    }

    fn copied(&mut self) -> Option<Zeroizing<String>> {
        self.copied.take()
    }

    fn pasted(&mut self, text: Option<Zeroizing<String>>) -> bool {
        if std::mem::take(&mut self.paste_search) {
            // A palette that closed meanwhile takes nothing.
            return text.is_some_and(|text| self.edit_search(EditOp::Paste(&text)));
        }
        self.take_paste(text)
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
pub(super) mod tests {
    use gogo_ui_interact::{Key, Modifiers, PointerButton};
    use gogo_ui_shell::accesskit::{Action, Role, TreeId};

    use gogo_gui_model::DWELL;

    use super::*;

    pub(super) const SIZE: (f32, f32) = (1280.0, 800.0);

    pub(super) fn app() -> App {
        let data = Data::fixture(|| {}).expect("fixture data");
        let mut app = App::new(None, data).expect("the built-in skin");
        app.pending.scale = 1.0;
        frame(&mut app);
        app
    }

    /// A frame built and presented.
    pub(super) fn frame(app: &mut App) {
        app.draw(SIZE);
        app.show(Duration::ZERO);
    }

    /// The middle of the node the skin calls `name`.
    pub(super) fn middle(app: &App, name: &str) -> (f64, f64) {
        let screen = app.shown.screen.as_ref().expect("a screen");
        let node = screen.tree.by_name(name).expect(name);
        let rect = screen.laid.rect(node).expect("laid out");
        ((rect.x0 + rect.x1) / 2.0, (rect.y0 + rect.y1) / 2.0)
    }

    pub(super) fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    pub(super) fn click(app: &mut App, name: &str, ms: u64) {
        let (x, y) = middle(app, name);
        let button = PointerButton::Primary;
        app.input(Input::PointerMoved { x, y }, at(ms));
        app.input(Input::PointerDown { button, x, y }, at(ms + 1));
        app.input(Input::PointerUp { button, x, y }, at(ms + 2));
        frame(app);
    }

    /// Where assistive technology sees the control called `label` with `role`.
    pub(super) fn bounds(app: &mut App, role: Role, label: &str) -> (f64, f64) {
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
    pub(super) fn click_at(app: &mut App, (x, y): (f64, f64), ms: u64) {
        let button = PointerButton::Primary;
        app.input(Input::PointerMoved { x, y }, at(ms));
        app.input(Input::PointerDown { button, x, y }, at(ms + 1));
        app.input(Input::PointerUp { button, x, y }, at(ms + 2));
    }

    pub(super) fn key(app: &mut App, key: Key, ms: u64) {
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

    /// How far the route on the display is scrolled.
    fn scrolled(app: &App) -> f64 {
        let screen = app.shown.screen.as_ref().expect("a screen");
        let main = screen.tree.by_name("main").expect("the route's container");
        screen.laid.scroll_box(main).expect("a scroll box").offset
    }

    #[test]
    fn going_back_restores_where_a_route_was_scrolled() {
        let mut app = app();
        click(&mut app, "nav-library", 1_000);
        let (x, y) = (700.0, 500.0);
        app.input(Input::Wheel { x, y, dy: 200.0 }, at(2_000));
        frame(&mut app);
        assert_eq!(scrolled(&app), 200.0);

        let settings = bounds(&mut app, Role::Button, "Settings");
        click_at(&mut app, settings, 3_000);
        frame(&mut app);
        assert_eq!(app.data.location().route, "settings");
        assert_eq!(scrolled(&app), 0.0, "another route has its own offset");
        app.input(Input::Wheel { x, y, dy: 20.0 }, at(4_000));
        frame(&mut app);

        key(&mut app, Key::Escape, 5_000);
        assert_eq!(app.data.location().route, "library");
        assert_eq!(scrolled(&app), 200.0);
    }

    #[test]
    fn a_click_goes_to_a_route_and_esc_goes_back() {
        let mut app = app();
        assert_eq!(app.data.location().route, "game");
        let good = app.list.clone();
        click(&mut app, "nav-library", 10);
        assert_eq!(app.data.location().route, "library");
        assert_ne!(app.list, good);
        key(&mut app, Key::Escape, 20);
        assert_eq!(app.data.location().route, "game");
    }

    #[test]
    fn only_a_fresh_press_acts_on_a_dialog() {
        let model = |n: u64| {
            let mut model =
                gogo_gui_model::Model::new(gogo_gui_model::fake::FakeBackend::new(), || {});
            let failures = model.failures();
            let mut ticket = None;
            for i in 0..n {
                failures.report(gogo_gui_model::ErrorView::failed("x", i.to_string()));
                model.pump();
                ticket = model.modal().map(|m| m.ticket());
                model.dismiss();
            }
            ticket.expect("a ticket")
        };
        let (ticket, other) = (model(1), model(2));
        let shown = Shown {
            ticket,
            at: at(1000),
            after: 5,
            complete: true,
        };
        let seen = at(1000) + DWELL;
        assert!(shown.takes(ticket, 6, seen, false));
        // Numbered when it went down, which was before the dialog appeared.
        assert!(!shown.takes(ticket, 5, seen, false));
        assert!(!shown.takes(ticket, 4, at(9000), false));
        // Too soon, before it was shown at all, repeating, or for another screen.
        assert!(!shown.takes(ticket, 6, seen - at(1), false));
        assert!(!shown.takes(ticket, 6, at(10), false));
        assert!(!shown.takes(ticket, 6, seen, true));
        assert!(!shown.takes(other, 6, seen, false));
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
    pub(super) fn press(app: &mut App, role: Role, label: &str, ms: u64) {
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

    /// The built-in skin with a `library` route that loads and never binds: `$nope` is no
    /// parameter of it.
    pub(super) fn with_an_unbindable_library() -> Vec<(String, Vec<u8>)> {
        let mut files = gogo_skins::default().to_files();
        files.retain(|(path, _)| path != "routes/library.kdl");
        files.push((
            "routes/library.kdl".to_owned(),
            b"route \"library\" { text \"{$nope}\" }".to_vec(),
        ));
        files
    }

    #[test]
    fn the_error_panel_takes_the_input() {
        let mut app = app();
        app.page.replace_built_in(with_an_unbindable_library());
        frame(&mut app);
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
        assert!(!app.show(Duration::ZERO));
    }
}

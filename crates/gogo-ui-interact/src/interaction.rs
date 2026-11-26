//! The state machine: focus, hover, presses and holds.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gogo_ui_ir::{AppPart, DrawnBy, NodeId, Role};

use crate::focus::FocusOrder;
use crate::frame::{Frame, Identity, Layer, LayerId, LayerKind, Target, activatable, control};
use crate::input::{Input, Key, Modifiers, PointerButton, Request, RequestKind};
use crate::intent::{ControlInput, InputEvent, InputProvenance, Intent, Response};
use crate::keys;

/// The least time between the two screen-reader activations that start a hold.
pub const HOLD_ARM: Duration = Duration::from_millis(500);
/// How long the first of them stays good: after this the next one arms again.
pub const ARM_EXPIRES: Duration = Duration::from_secs(10);

/// The modifier of the platform's shortcuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Primary {
    /// Ctrl (Linux).
    #[default]
    Ctrl,
    /// Command (macOS).
    Meta,
}

/// What differs between platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Config {
    /// The palette is this modifier with K.
    pub primary: Primary,
}

impl Config {
    /// A configuration with this primary modifier.
    pub fn new(primary: Primary) -> Self {
        Config { primary }
    }
}

/// How many character keys are tracked as held; more only means releases were lost.
const MAX_HELD_CHARS: usize = 32;

/// What a control was when a press went down.
#[derive(Debug, Clone, PartialEq)]
struct Signature {
    identity: Option<Identity>,
    app: Option<AppPart>,
    checked: Option<bool>,
    activatable: bool,
    hold: bool,
}

#[derive(Debug, Clone)]
struct Press {
    target: Target,
    signature: Signature,
    event: InputEvent,
    at: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldBy {
    Pointer,
    Key(Key),
    Assistive,
}

#[derive(Debug, Clone)]
struct Hold {
    target: Target,
    signature: Signature,
    by: HoldBy,
}

#[derive(Debug, Clone)]
struct Armed {
    target: Target,
    signature: Signature,
    at: Duration,
}

/// Input state kept across frames.
#[derive(Debug, Default)]
pub struct Interaction {
    config: Config,
    latest: InputEvent,
    focus: Option<Target>,
    visible: bool,
    /// The last focused key of each layer, to give focus back when a dialog closes.
    memory: HashMap<LayerId, String>,
    /// The order of the last frame, to find a neighbour when a control disappears.
    order: FocusOrder,
    modal: Option<LayerId>,
    pointer_at: Option<(f64, f64)>,
    hovered: Option<Target>,
    pointer: Option<Press>,
    pointer_inside: bool,
    space: Option<Press>,
    hold: Option<Hold>,
    armed: Option<Armed>,
    down: HashSet<Key>,
}

fn signature(layer: &Layer<'_>, node: NodeId) -> Option<Signature> {
    let n = layer.tree.get(node)?;
    Some(Signature {
        identity: layer.action_identity(node),
        app: match n.drawn_by {
            DrawnBy::App(part) => Some(part),
            _ => None,
        },
        checked: n.a11y.checked,
        activatable: activatable(n),
        hold: layer
            .tree
            .key(node)
            .is_some_and(|k| layer.hold.contains(&k)),
    })
}

/// The layer and node a target names in this frame, if it can still take input.
fn find<'a>(frame: &Frame<'a>, layer: LayerId, key: &str) -> Option<(&'a Layer<'a>, NodeId)> {
    let layer = frame.layer(layer)?;
    Some((layer, layer.tree.by_key(key)?))
}

fn unchanged(frame: &Frame<'_>, target: &Target, was: &Signature) -> bool {
    find(frame, target.layer, &target.key)
        .is_some_and(|(l, n)| signature(l, n).as_ref() == Some(was))
}

/// The control under a point: the nearest control at or above the topmost node there.
fn control_at(frame: &Frame<'_>, x: f64, y: f64) -> Option<Target> {
    // What is drawn over a dialog takes no input, and a click on it must not reach the
    // dialog control it covers.
    let covered = frame.over_modal().iter().any(|layer| {
        layer
            .laid
            .hit(x, y)
            .is_some_and(|n| layer.kind != LayerKind::Overlay || n != layer.tree.root())
    });
    if covered {
        return None;
    }
    for layer in frame.interactive().iter().rev() {
        let Some(mut node) = layer.laid.hit(x, y) else {
            continue;
        };
        if layer.kind == LayerKind::Overlay && node == layer.tree.root() {
            continue;
        }
        loop {
            let n = layer.tree.get(node)?;
            if n.a11y.disabled {
                return None;
            }
            if control(n) {
                return layer.target(node);
            }
            node = layer.tree.parent(node)?;
        }
    }
    None
}

fn is_hold(layer: &Layer<'_>, target: &Target) -> bool {
    layer.hold.contains(&target.key.as_str())
}

fn activation(layer: &Layer<'_>, target: Target, provenance: InputProvenance) -> Option<Intent> {
    let node = layer.tree.get(target.node)?;
    if !activatable(node) {
        return None;
    }
    match node.drawn_by {
        DrawnBy::App(part) => Some(Intent::App {
            target,
            part,
            provenance,
        }),
        _ => Some(Intent::Invoke {
            action: node.action?,
            input: ControlInput {
                on: (node.a11y.role == Role::Switch)
                    .then_some(node.a11y.checked)
                    .flatten(),
            },
            target,
            provenance,
        }),
    }
}

impl Interaction {
    /// Nothing focused, nothing pressed.
    pub fn new(config: Config) -> Self {
        Interaction {
            config,
            ..Interaction::default()
        }
    }

    /// The focused control, as of the last [`handle`](Self::handle) or
    /// [`sync`](Self::sync). Assistive technology follows this.
    pub fn focus(&self) -> Option<&Target> {
        self.focus.as_ref()
    }

    /// The focused control when its focus ring should show: give its node to the scene
    /// of its layer.
    pub fn focus_visible(&self) -> Option<&Target> {
        self.focus.as_ref().filter(|_| self.visible)
    }

    /// The control under the pointer.
    pub fn hovered(&self) -> Option<&Target> {
        self.hovered.as_ref()
    }

    /// The control being pressed or held.
    pub fn pressed(&self) -> Option<&Target> {
        self.pointer
            .as_ref()
            .filter(|_| self.pointer_inside)
            .map(|p| &p.target)
            .or_else(|| self.space.as_ref().map(|p| &p.target))
            .or_else(|| self.hold.as_ref().map(|h| &h.target))
    }

    /// The hold control a screen reader activated once and may activate again.
    pub fn armed(&self) -> Option<&Target> {
        self.armed.as_ref().map(|a| &a.target)
    }

    /// The number of the latest input: what the app records when it has drawn a screen,
    /// so only later input can answer it.
    pub fn latest(&self) -> InputEvent {
        self.latest
    }

    /// Takes in a new frame: keeps focus on the same key, gives a new dialog its initial
    /// focus, gives focus back when a dialog closes, and cancels presses whose control
    /// changed. Call it after a rebuild or a layer change; [`handle`](Self::handle) does
    /// it too.
    pub fn sync(&mut self, frame: &Frame<'_>) -> Response {
        let before = self.shown();
        let mut out = Response::default();
        self.refresh(frame, &mut out);
        out.redraw = before != self.shown();
        out
    }

    /// One input on the frame that was on screen when it happened. `at` is a monotonic
    /// time on the clock the app compares presses with.
    pub fn handle(&mut self, input: Input, at: Duration, frame: &Frame<'_>) -> Response {
        let before = self.shown();
        let mut out = Response::default();
        self.refresh(frame, &mut out);
        match input {
            Input::PointerMoved { x, y } => self.pointer_moved(x, y, frame, &mut out),
            Input::PointerLeft => {
                self.pointer_at = None;
                self.hovered = None;
                self.pointer_inside = false;
                self.end_hold_by(HoldBy::Pointer, &mut out);
            }
            Input::PointerDown { button, x, y } => {
                if button == PointerButton::Primary {
                    self.pointer_down(x, y, at, frame, &mut out);
                } else {
                    self.cancel_assistive(&mut out);
                }
            }
            Input::PointerUp { button, x, y } => {
                if button == PointerButton::Primary {
                    self.pointer_up(x, y, frame, &mut out);
                }
            }
            Input::KeyDown { key, mods, repeat } => {
                self.key_down(key.normal(), mods, repeat, at, frame, &mut out);
            }
            Input::KeyUp { key } => self.key_up(key.normal(), frame, &mut out),
            Input::FocusLost => {
                self.down.clear();
                self.cancel_presses(&mut out);
            }
            Input::Accessibility(request) => self.assistive(request, at, frame, &mut out),
        }
        out.redraw = before != self.shown();
        out
    }

    fn shown(&self) -> [Option<Target>; 5] {
        [
            self.focus().cloned(),
            self.focus_visible().cloned(),
            self.hovered().cloned(),
            self.pressed().cloned(),
            self.armed().cloned(),
        ]
    }

    fn number(&mut self) -> InputEvent {
        self.latest = InputEvent(self.latest.0 + 1);
        self.latest
    }

    fn end_hold(&mut self, out: &mut Response) {
        if let Some(hold) = self.hold.take() {
            out.intents.push(Intent::HoldEnd {
                target: hold.target,
            });
        }
    }

    fn end_hold_by(&mut self, by: HoldBy, out: &mut Response) {
        if self.hold.as_ref().is_some_and(|h| h.by == by) {
            self.end_hold(out);
        }
    }

    fn cancel_presses(&mut self, out: &mut Response) {
        self.pointer = None;
        self.space = None;
        self.armed = None;
        self.end_hold(out);
    }

    /// Moves focus; whatever was being pressed or armed elsewhere is cancelled.
    fn set_focus(&mut self, target: Option<Target>, out: &mut Response) {
        let unmoved = match (&self.focus, &target) {
            (Some(a), Some(b)) => a.same(b),
            (None, None) => true,
            _ => false,
        };
        if unmoved {
            self.focus = target;
            return;
        }
        let stays = |other: &Target| target.as_ref().is_some_and(|t| t.same(other));
        if self.hold.as_ref().is_some_and(|h| !stays(&h.target)) {
            self.end_hold(out);
        }
        if self.armed.as_ref().is_some_and(|a| !stays(&a.target)) {
            self.armed = None;
        }
        if self.space.as_ref().is_some_and(|p| !stays(&p.target)) {
            self.space = None;
        }
        if let Some(t) = &target {
            self.memory.insert(t.layer, t.key.clone());
        }
        self.focus = target;
    }

    fn remembered(&self, frame: &Frame<'_>) -> Option<Target> {
        frame.interactive().iter().rev().find_map(|layer| {
            let node = layer.tree.by_key(self.memory.get(&layer.id)?)?;
            let target = layer.target(node)?;
            layer
                .tree
                .get(node)
                .filter(|n| n.a11y.focusable && !n.a11y.disabled)?;
            Some(target)
        })
    }

    fn refresh(&mut self, frame: &Frame<'_>, out: &mut Response) {
        debug_assert!(
            frame
                .interactive()
                .iter()
                .all(Layer::identifies_every_action),
            "Layer::identity needs an entry for every action"
        );
        let order = FocusOrder::of(frame);
        let modal = frame.modal().map(|l| l.id);
        let current = |t: &Target| order.position(t).map(|i| order.targets()[i].clone());

        let kept = self.focus.as_ref().and_then(current);
        // The control is gone but its screen is not: the nearest earlier one.
        let neighbour = self
            .focus
            .as_ref()
            .filter(|_| kept.is_none())
            .and_then(|old| {
                let at = self.order.position(old)?;
                self.order.targets()[..at]
                    .iter()
                    .rev()
                    .filter(|t| t.layer == old.layer)
                    .find_map(current)
            });
        let lost = self.focus.is_some() && kept.is_none();
        let focus = kept.or(neighbour);

        let opened = frame
            .modal()
            .filter(|l| self.modal != Some(l.id) && !self.memory.contains_key(&l.id));
        if let Some(dialog) = opened {
            let initial = dialog
                .initial_focus
                .and_then(|key| dialog.tree.by_key(key))
                .and_then(|node| dialog.target(node))
                .and_then(|t| current(&t));
            // A dialog's starting point is always shown.
            self.visible = initial.is_some();
            self.set_focus(initial, out);
        } else if (self.modal != modal && self.modal.is_some()) || (lost && focus.is_none()) {
            let back = focus.or_else(|| self.remembered(frame));
            self.set_focus(back, out);
        } else {
            self.set_focus(focus, out);
        }
        self.modal = modal;
        self.memory
            .retain(|id, _| frame.layers.iter().any(|l| l.id == *id));
        self.order = order;

        if self
            .pointer
            .as_ref()
            .is_some_and(|p| !unchanged(frame, &p.target, &p.signature))
        {
            self.pointer = None;
        }
        if self
            .space
            .as_ref()
            .is_some_and(|p| !unchanged(frame, &p.target, &p.signature))
        {
            self.space = None;
        }
        if self
            .armed
            .as_ref()
            .is_some_and(|a| !unchanged(frame, &a.target, &a.signature))
        {
            self.armed = None;
        }
        if self
            .hold
            .as_ref()
            .is_some_and(|h| !unchanged(frame, &h.target, &h.signature))
        {
            self.end_hold(out);
        }
        // The same key may have a new node id in a rebuilt tree.
        let renumber = |t: &mut Target| {
            if let Some((_, node)) = find(frame, t.layer, &t.key) {
                t.node = node;
            }
        };
        if let Some(p) = &mut self.pointer {
            renumber(&mut p.target);
        }
        if let Some(p) = &mut self.space {
            renumber(&mut p.target);
        }
        if let Some(h) = &mut self.hold {
            renumber(&mut h.target);
        }
        if let Some(a) = &mut self.armed {
            renumber(&mut a.target);
        }
        // The control may have moved, or something may now cover it.
        self.hovered = self.pointer_at.and_then(|(x, y)| control_at(frame, x, y));
        self.follow_pointer(out);
    }

    /// A pointer hold lasts only while the pointer is on its control.
    fn follow_pointer(&mut self, out: &mut Response) {
        let over = |t: &Target| self.hovered.as_ref().is_some_and(|h| h.same(t));
        self.pointer_inside = self.pointer.as_ref().is_some_and(|p| over(&p.target));
        if self
            .hold
            .as_ref()
            .is_some_and(|h| h.by == HoldBy::Pointer && !over(&h.target))
        {
            self.end_hold(out);
        }
    }

    /// Another key or button: a screen reader's arm or running hold is off.
    fn cancel_assistive(&mut self, out: &mut Response) {
        self.armed = None;
        self.end_hold_by(HoldBy::Assistive, out);
    }

    fn pointer_moved(&mut self, x: f64, y: f64, frame: &Frame<'_>, out: &mut Response) {
        self.pointer_at = Some((x, y));
        self.hovered = control_at(frame, x, y);
        self.follow_pointer(out);
    }

    fn pointer_down(
        &mut self,
        x: f64,
        y: f64,
        at: Duration,
        frame: &Frame<'_>,
        out: &mut Response,
    ) {
        self.pointer_at = Some((x, y));
        self.cancel_presses(out);
        let event = self.number();
        self.visible = false;
        self.hovered = control_at(frame, x, y);
        let Some(target) = self.hovered.clone() else {
            return;
        };
        out.consumed = true;
        let Some((layer, node)) = find(frame, target.layer, &target.key) else {
            return;
        };
        if layer.tree.get(node).is_some_and(|n| n.a11y.focusable) {
            self.set_focus(Some(target.clone()), out);
        }
        let Some(signature) = signature(layer, node) else {
            return;
        };
        if is_hold(layer, &target) {
            self.start_hold(layer, target, signature, HoldBy::Pointer, event, at, out);
        } else {
            self.pointer = Some(Press {
                target,
                signature,
                event,
                at,
            });
            self.pointer_inside = true;
        }
    }

    fn pointer_up(&mut self, x: f64, y: f64, frame: &Frame<'_>, out: &mut Response) {
        self.pointer_at = Some((x, y));
        self.hovered = control_at(frame, x, y);
        self.end_hold_by(HoldBy::Pointer, out);
        let Some(press) = self.pointer.take() else {
            return;
        };
        out.consumed = true;
        if self.hovered.as_ref().is_some_and(|h| h.same(&press.target)) {
            self.release(press, frame, out);
        }
    }

    /// A press that ends on its control acts, if the control is still what it was.
    fn release(&mut self, press: Press, frame: &Frame<'_>, out: &mut Response) {
        if !unchanged(frame, &press.target, &press.signature) {
            return;
        }
        let Some((layer, _)) = find(frame, press.target.layer, &press.target.key) else {
            return;
        };
        let provenance = InputProvenance {
            event: press.event,
            at: press.at,
        };
        out.intents
            .extend(activation(layer, press.target, provenance));
    }

    #[allow(clippy::too_many_arguments)]
    fn start_hold(
        &mut self,
        layer: &Layer<'_>,
        target: Target,
        signature: Signature,
        by: HoldBy,
        event: InputEvent,
        at: Duration,
        out: &mut Response,
    ) {
        if !layer.tree.get(target.node).is_some_and(activatable) {
            return;
        }
        out.intents.push(Intent::HoldStart {
            target: target.clone(),
            part: signature.app,
            provenance: InputProvenance { event, at },
        });
        self.hold = Some(Hold {
            target,
            signature,
            by,
        });
    }

    fn key_down(
        &mut self,
        key: Key,
        mods: Modifiers,
        repeat: bool,
        at: Duration,
        frame: &Frame<'_>,
        out: &mut Response,
    ) {
        // Unnamed keys (bare modifiers among them, which screen readers hold for their
        // own commands) share one value: they can't be tracked and cancel nothing.
        let chars = |k: &&Key| matches!(k, Key::Char(_));
        if matches!(key, Key::Char(_))
            && !self.down.contains(&key)
            && self.down.iter().filter(chars).count() >= MAX_HELD_CHARS
        {
            self.down.retain(|k| !matches!(k, Key::Char(_)));
        }
        let again = key != Key::Other && !self.down.insert(key);
        if again && !repeat {
            // Down twice with no release and no repeat flag: a release was lost, so a
            // press still waiting on this key can't be trusted.
            if key == Key::Space {
                self.space = None;
            }
            self.end_hold_by(HoldBy::Key(key), out);
        }
        let repeat = repeat || again;
        if !repeat && key != Key::Other {
            self.cancel_assistive(out);
        }
        let plain = !(mods.ctrl || mods.alt || mods.meta || mods.shift);
        let palette = key == Key::Char('k')
            && !mods.alt
            && !mods.shift
            && match self.config.primary {
                Primary::Ctrl => mods.ctrl && !mods.meta,
                Primary::Meta => mods.meta && !mods.ctrl,
            };
        match key {
            Key::Escape => {
                out.consumed = true;
                if !repeat {
                    self.cancel_presses(out);
                    let provenance = self.provenance(at);
                    out.intents.push(Intent::Dismiss { provenance });
                }
            }
            _ if palette => {
                out.consumed = true;
                if !repeat {
                    self.cancel_presses(out);
                    let provenance = self.provenance(at);
                    out.intents.push(Intent::Palette { provenance });
                }
            }
            Key::Tab if !(mods.ctrl || mods.alt || mods.meta) => {
                out.consumed = true;
                let order = FocusOrder::of(frame);
                if let Some(next) = order.step(self.focus.as_ref(), mods.shift).cloned() {
                    self.visible = true;
                    self.set_focus(Some(next), out);
                }
            }
            Key::ArrowLeft | Key::ArrowRight if plain => {
                if let Some(next) = self.neighbour_tab(key == Key::ArrowLeft, frame) {
                    out.consumed = true;
                    self.visible = true;
                    self.set_focus(Some(next), out);
                }
            }
            Key::Enter | Key::Space if plain => {
                out.consumed = self.focus.is_some() || frame.modal().is_some();
                if !repeat {
                    self.activate_focus(key, at, frame, out);
                }
            }
            Key::Char(_) if !repeat => self.shortcut(key, mods, at, frame, out),
            _ => {}
        }
    }

    fn provenance(&mut self, at: Duration) -> InputProvenance {
        InputProvenance {
            event: self.number(),
            at,
        }
    }

    fn neighbour_tab(&self, back: bool, frame: &Frame<'_>) -> Option<Target> {
        let focus = self.focus.as_ref()?;
        let (layer, node) = find(frame, focus.layer, &focus.key)?;
        let is_tab = |&n: &NodeId| {
            layer
                .tree
                .get(n)
                .is_some_and(|n| n.a11y.role == Role::Tab && n.a11y.focusable && !n.a11y.disabled)
        };
        if !is_tab(&node) {
            return None;
        }
        let tabs: Vec<NodeId> = layer
            .tree
            .children(layer.tree.parent(node)?)
            .iter()
            .copied()
            .filter(is_tab)
            .collect();
        let i = tabs.iter().position(|&t| t == node)?;
        let n = tabs.len();
        let next = if back { (i + n - 1) % n } else { (i + 1) % n };
        layer.target(tabs[next])
    }

    /// Enter or Space went down.
    fn activate_focus(&mut self, key: Key, at: Duration, frame: &Frame<'_>, out: &mut Response) {
        self.cancel_presses(out);
        let Some(target) = self.focus.clone() else {
            if key == Key::Enter && frame.modal().is_some() {
                let provenance = self.provenance(at);
                out.intents.push(Intent::Enter { provenance });
            }
            return;
        };
        let Some((layer, node)) = find(frame, target.layer, &target.key) else {
            return;
        };
        let Some(signature) = signature(layer, node) else {
            return;
        };
        let event = self.number();
        self.visible = true;
        if is_hold(layer, &target) {
            self.start_hold(layer, target, signature, HoldBy::Key(key), event, at, out);
        } else if key == Key::Enter {
            let provenance = InputProvenance { event, at };
            out.intents.extend(activation(layer, target, provenance));
        } else {
            self.space = Some(Press {
                target,
                signature,
                event,
                at,
            });
        }
    }

    fn key_up(&mut self, key: Key, frame: &Frame<'_>, out: &mut Response) {
        // A release with no press seen here (the key went down elsewhere) does nothing.
        if !self.down.remove(&key) {
            return;
        }
        self.end_hold_by(HoldBy::Key(key), out);
        if key == Key::Space
            && let Some(press) = self.space.take()
        {
            out.consumed = true;
            self.release(press, frame, out);
        }
    }

    /// A skin's `key=`: on the page only, never under a dialog or in a text field.
    fn shortcut(
        &mut self,
        key: Key,
        mods: Modifiers,
        at: Duration,
        frame: &Frame<'_>,
        out: &mut Response,
    ) {
        let typing = self.focus.as_ref().is_some_and(|f| {
            find(frame, f.layer, &f.key)
                .and_then(|(l, n)| l.tree.get(n))
                .is_some_and(|n| n.a11y.role == Role::TextInput)
        });
        if typing || frame.modal().is_some() {
            return;
        }
        let found = frame
            .interactive()
            .iter()
            .filter(|l| l.kind == LayerKind::Page)
            .find_map(|layer| {
                let node = layer.tree.iter().find(|&id| {
                    layer.tree.get(id).is_some_and(|n| {
                        n.drawn_by == DrawnBy::Skin
                            && activatable(n)
                            && n.a11y
                                .key
                                .as_deref()
                                .is_some_and(|spec| keys::matches(spec, key, mods))
                    })
                })?;
                let target = layer.target(node)?;
                (!is_hold(layer, &target)).then_some((layer, target))
            });
        if let Some((layer, target)) = found {
            out.consumed = true;
            self.cancel_presses(out);
            let provenance = self.provenance(at);
            out.intents.extend(activation(layer, target, provenance));
        }
    }

    fn assistive(&mut self, request: Request, at: Duration, frame: &Frame<'_>, out: &mut Response) {
        let Some((layer, node)) = find(frame, request.layer, &request.key) else {
            return;
        };
        let (Some(n), Some(target)) = (layer.tree.get(node), layer.target(node)) else {
            return;
        };
        // Requests arrive late: the control must still do what it did when it was read.
        if !control(n) || request.identity != layer.action_identity(node) {
            return;
        }
        let focusable = n.a11y.focusable;
        match request.kind {
            RequestKind::Focus => {
                if focusable {
                    out.consumed = true;
                    self.visible = true;
                    self.set_focus(Some(target), out);
                }
            }
            RequestKind::SetValue(value) => {
                if n.a11y.role == Role::TextInput {
                    out.consumed = true;
                    self.visible = true;
                    self.set_focus(Some(target.clone()), out);
                    out.intents.push(Intent::SetValue { target, value });
                }
            }
            RequestKind::Activate => {
                if !activatable(n) {
                    return;
                }
                let Some(signature) = signature(layer, node) else {
                    return;
                };
                out.consumed = true;
                self.pointer = None;
                self.space = None;
                self.visible = true;
                if focusable {
                    self.set_focus(Some(target.clone()), out);
                }
                let event = self.number();
                if !is_hold(layer, &target) {
                    self.armed = None;
                    self.end_hold(out);
                    let provenance = InputProvenance { event, at };
                    out.intents.extend(activation(layer, target, provenance));
                    return;
                }
                if self.hold.as_ref().is_some_and(|h| h.target.same(&target)) {
                    return;
                }
                self.end_hold(out);
                match &self.armed {
                    Some(a) if a.target.same(&target) && at.saturating_sub(a.at) <= ARM_EXPIRES => {
                        // Too soon (a double activation): stay armed.
                        if at.saturating_sub(a.at) >= HOLD_ARM {
                            self.armed = None;
                            self.start_hold(
                                layer,
                                target,
                                signature,
                                HoldBy::Assistive,
                                event,
                                at,
                                out,
                            );
                        }
                    }
                    _ => {
                        out.intents.push(Intent::HoldArmed {
                            target: target.clone(),
                        });
                        self.armed = Some(Armed {
                            target,
                            signature,
                            at,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gogo_ui_ir::{A11y, ActionRef, Node, Role, Tree};
    use gogo_ui_layout::{Scroll, Viewport, layout};
    use gogo_ui_text::FontSet;

    use super::*;

    fn tree() -> Tree {
        let button = Node::container()
            .with_a11y(A11y::control(Role::Button, "Play"))
            .with_action(ActionRef::new(0));
        let mut tree = Tree::new("root", Node::container());
        let root = tree.root();
        tree.add_child(root, "play", button).expect("add");
        tree
    }

    /// Whether a press on `play` of one tree still stands on the other.
    fn survives(before: &Tree, after: &Tree, identity: &[u64]) -> bool {
        let lay = |t: &Tree| {
            let view = Viewport::new(100.0, 100.0);
            layout(t, &mut FontSet::bundled(), view, &Scroll::none()).expect("layout")
        };
        let (a, b) = (lay(before), lay(after));
        let layer = |t, l| Layer::new(LayerId(1), LayerKind::Page, t, l).identity(identity);
        let (old, new) = ([layer(before, &a)], [layer(after, &b)]);
        let node = before.by_key("play").expect("node");
        let target = old[0].target(node).expect("target");
        let was = signature(&old[0], node).expect("signature");
        unchanged(&Frame { layers: &new }, &target, &was)
    }

    #[test]
    fn s4_without_an_identity_no_press_outlives_a_rebuild() {
        let first = tree();
        let rebuilt = tree();
        assert_eq!(first, rebuilt, "the same content");
        assert!(survives(&first, &first, &[]), "the same build");
        assert!(survives(&first, &first.clone(), &[]));
        assert!(!survives(&first, &rebuilt, &[]));
        assert!(survives(&first, &rebuilt, &[7]), "the app vouches for it");
        let mut changed = first.clone();
        let root = changed.root();
        changed.node_mut(root);
        assert!(!survives(&first, &changed, &[]));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "an entry for every action")]
    fn debug_builds_insist_on_an_identity_per_action() {
        let tree = tree();
        let view = Viewport::new(100.0, 100.0);
        let laid = layout(&tree, &mut FontSet::bundled(), view, &Scroll::none()).expect("layout");
        let layers = [Layer::new(LayerId(1), LayerKind::Page, &tree, &laid)];
        Interaction::new(Config::default()).sync(&Frame { layers: &layers });
    }
}

//! The app's own dialogs (design 26 §5): every screen of the safety flow, as an app
//! widget in a modal layer. The app builds them itself; nothing of the skin is in them,
//! and no skin can read them (S1).
//!
//! A secret is taken by pasting only. The dialog says whether one is there and never
//! shows it: no characters, no dots and no length, on screen or to assistive technology.
//!
//! Nothing scrolls. A list that does not fit (a plan's steps, a question's options, the
//! conflicting files) is split into pages; everything else, the warnings, totals and
//! buttons among it, is always on the card. When even that does not fit the window, the
//! dialog says so and can only be cancelled ([`Dialog::answerable`]).
//!
//! Fixed colours for now; styling them with a skin's tokens is a follow-up (#640).

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::ops::Range;

use gogo_gui_model::{ConflictFile, ErrorView, Plan, PlanStep, Side, StepKind, Ticket};
use gogo_ui_interact::{Layer, LayerId, LayerKind};
use gogo_ui_ir::{
    A11y, ActionRef, Align, AppPart, Color, Dim, Direction, DrawnBy, Justify, Kind, Node, NodeId,
    Paint, Role, Sides, Stroke, TextMarks, TextStyle, Tree, WidgetId,
};
use gogo_ui_layout::{Laid, Placed, Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, SceneState, build};
use gogo_ui_text::{EditBuffer, FontSet, TextLayout, TextRect};

use crate::panel::PanelError;

/// The keys of the controls.
pub(crate) const PASTE: &str = "dialog/paste";
pub(crate) const CLEAR: &str = "dialog/clear";
pub(crate) const OPEN: &str = "dialog/open";
/// Sends a pasted secret.
pub(crate) const SUBMIT: &str = "dialog/submit";
/// Cancels; also the only way out of a dialog that does not fit.
pub(crate) const CANCEL: &str = "dialog/cancel";
/// Dismisses an error.
pub(crate) const CLOSE: &str = "dialog/close";
/// The plan card's confirm.
pub(crate) const CONFIRM: &str = "dialog/confirm";
/// Submits the typed title.
pub(crate) const TYPED: &str = "dialog/typed";
/// The press-and-hold confirmation.
pub(crate) const HOLD_BUTTON: &str = "dialog/hold";
/// The text field.
pub(crate) const FIELD: &str = "dialog/field";
/// The text inside it.
const FIELD_TEXT: &str = "dialog/field/text";
pub(crate) const OK: &str = "dialog/ok";
pub(crate) const YES: &str = "dialog/yes";
pub(crate) const NO: &str = "dialog/no";
pub(crate) const APPLY: &str = "dialog/apply";
pub(crate) const PREVIOUS: &str = "dialog/previous";
pub(crate) const NEXT: &str = "dialog/next";
/// Followed by an option's index.
pub(crate) const OPTION: &str = "dialog/option/";
/// Followed by a conflicting file's index.
pub(crate) const LOCAL: &str = "dialog/local/";
/// Followed by a conflicting file's index.
pub(crate) const CLOUD: &str = "dialog/cloud/";

const HOLDS: &[&str] = &[HOLD_BUTTON];

/// The widget id of these controls. The binder numbers a skin's widgets from 0, and
/// these are in a layer of their own.
const WIDGET: WidgetId = WidgetId::new(u32::MAX);

/// Left free above and below the card, together.
const MARGIN: f32 = 16.0;
const PADDING: f32 = 20.0;
const GAP: f32 = 8.0;
const CARD_WIDTH: f32 = 560.0;
/// A run of this many characters without a space may be broken anywhere after them.
const UNBROKEN: usize = 32;

const SCRIM: Color = Color([0x00, 0x00, 0x00, 0xb0]);
const CARD: Color = Color([0x1c, 0x1f, 0x26, 0xff]);
const EDGE: Color = Color([0x8a, 0x92, 0xa6, 0xff]);
const BUTTON: Color = Color([0x2c, 0x31, 0x3d, 0xff]);
const PRIMARY: Color = Color([0x2b, 0x50, 0xa8, 0xff]);
const DANGER: Color = Color([0x8e, 0x2f, 0x2a, 0xff]);
const FIELD_FILL: Color = Color([0x10, 0x12, 0x17, 0xff]);
const RING: Color = Color([0x9d, 0xba, 0xff, 0xff]);
const SELECTED: Color = Color([0x2b, 0x50, 0xa8, 0xff]);
const TEXT: Color = Color([0xf5, 0xf5, 0xf5, 0xff]);
const DIM: Color = Color([0xa6, 0xad, 0xbd, 0xff]);
const WARN: Color = Color([0xff, 0xcf, 0x70, 0xff]);
const BAD: Color = Color([0xff, 0xa5, 0x9d, 0xff]);

/// What the secret dialog's field holds, in words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Field {
    #[default]
    Empty,
    Pasted,
    /// What was pasted holds no code, and was dropped.
    Rejected,
    /// The clipboard had no text.
    NothingToPaste,
}

impl Field {
    /// For the sign-in code, or any other secret.
    pub(crate) fn words(self, sign_in: bool) -> &'static str {
        match self {
            Field::Empty if sign_in => "No code pasted yet.",
            Field::Pasted if sign_in => "A code is pasted.",
            Field::Rejected if sign_in => "That was not a GOG sign-in address or code.",
            Field::Empty => "Nothing pasted yet.",
            Field::Pasted => "Something is pasted.",
            Field::Rejected => "That could not be used.",
            Field::NothingToPaste => "There was no text to paste.",
        }
    }
}

/// Where a hold confirmation is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) enum Hold {
    #[default]
    Idle,
    /// A screen reader activated it once.
    Armed,
    /// How much of the hold has passed, 0 to 1.
    Holding(f32),
}

/// A text field as it is drawn.
#[derive(Clone, PartialEq, Default)]
pub(crate) struct FieldView {
    /// The text that would be answered: without a composition.
    pub(crate) text: String,
    /// The text on screen.
    pub(crate) shown: String,
    /// The selected bytes of `shown`.
    pub(crate) selection: Range<usize>,
    /// The selection as assistive technology is told it, in bytes of `text`: the end
    /// that stays, then the caret.
    pub(crate) ends: (usize, usize),
    /// Where the caret is, while the field has focus.
    pub(crate) caret: Option<usize>,
    /// The IME's composition.
    pub(crate) preedit: Option<Range<usize>>,
    /// How far the text had moved left.
    pub(crate) scroll: f32,
}

impl FieldView {
    /// What `buffer` holds, as it is drawn; the caret shows while `focused`.
    pub(crate) fn of(buffer: &EditBuffer, focused: bool, scroll: f32) -> FieldView {
        FieldView {
            text: buffer.text(),
            shown: buffer.shown().to_owned(),
            selection: buffer.selected_range(),
            // A composition is not part of what assistive technology reads: the caret
            // is where it will go.
            ends: match buffer.preedit() {
                Some(preedit) => (preedit.start, preedit.start),
                None => buffer.selection(),
            },
            caret: buffer.caret().filter(|_| focused),
            preedit: buffer.preedit(),
            scroll,
        }
    }
}

/// What a plan's dialog asks for.
#[derive(Clone, PartialEq)]
pub(crate) enum Ask {
    /// The plan card.
    Confirm,
    /// Type `want`.
    Typed {
        want: String,
        typed: FieldView,
    },
    Hold(Hold),
}

/// What a dialog shows.
#[derive(Clone, PartialEq)]
pub(crate) enum Content {
    /// Waiting for a plan.
    Planning {
        confirming: bool,
    },
    Plan {
        plan: Plan,
        /// The plan that was confirmed is no longer the plan.
        changed: bool,
        ask: Ask,
    },
    /// `label` says what to paste; it holds no secret.
    Secret {
        label: String,
        field: Field,
        /// The GOG sign-in code, whose page can be opened again.
        sign_in: bool,
    },
    Text {
        label: String,
        value: FieldView,
    },
    Choice {
        label: String,
        options: Vec<String>,
    },
    YesNo {
        label: String,
    },
    /// Something the app offers to do, with the one control that does it.
    Offer {
        label: String,
        yes: &'static str,
    },
    Conflicts {
        files: Vec<ConflictFile>,
        picks: Vec<Option<Side>>,
    },
    Error(ErrorView),
}

// What the user typed, and what a screen says about their games and files, stays out of
// `Debug` output: lengths and counts only.

impl std::fmt::Debug for FieldView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldView")
            .field("bytes", &self.text.len())
            .field("shown_bytes", &self.shown.len())
            .field("selection", &self.selection)
            .field("caret", &self.caret)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Ask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ask::Confirm => f.write_str("Confirm"),
            Ask::Typed { want, typed } => f
                .debug_struct("Typed")
                .field("want_bytes", &want.len())
                .field("typed", typed)
                .finish(),
            Ask::Hold(hold) => f.debug_tuple("Hold").field(hold).finish(),
        }
    }
}

impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Content::Planning { confirming } => f
                .debug_struct("Planning")
                .field("confirming", confirming)
                .finish(),
            Content::Plan { changed, ask, .. } => f
                .debug_struct("Plan")
                .field("changed", changed)
                .field("ask", ask)
                .finish_non_exhaustive(),
            Content::Secret {
                label,
                field,
                sign_in,
            } => f
                .debug_struct("Secret")
                .field("label_bytes", &label.len())
                .field("field", field)
                .field("sign_in", sign_in)
                .finish(),
            Content::Text { label, value } => f
                .debug_struct("Text")
                .field("label_bytes", &label.len())
                .field("value", value)
                .finish(),
            Content::Choice { label, options } => f
                .debug_struct("Choice")
                .field("label_bytes", &label.len())
                .field("options", &options.len())
                .finish(),
            Content::YesNo { label } => f
                .debug_struct("YesNo")
                .field("label_bytes", &label.len())
                .finish(),
            Content::Offer { label, yes } => f
                .debug_struct("Offer")
                .field("label_bytes", &label.len())
                .field("yes", yes)
                .finish(),
            Content::Conflicts { files, picks } => f
                .debug_struct("Conflicts")
                .field("files", &files.len())
                .field("picks", &picks.len())
                .finish(),
            Content::Error(_) => f.write_str("Error"),
        }
    }
}

/// A dialog laid out over a window: while it is up only it takes input.
pub(crate) struct Dialog {
    tree: Tree,
    laid: Laid,
    ticket: Ticket,
    kind: LayerKind,
    initial: Option<&'static str>,
    /// What each control does, by its action number.
    identity: Vec<u64>,
    /// Everything there is to read is on the card or on one of its pages.
    complete: bool,
    error: bool,
    page: usize,
    pages: usize,
}

fn sides(v: f32) -> Sides<f32> {
    Sides {
        top: v,
        right: v,
        bottom: v,
        left: v,
    }
}

/// `text` with places to wrap a long run in: only whole words wrap, and what does not
/// wrap would be cut off.
fn breakable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    for c in text.chars() {
        run = if c.is_whitespace() { 0 } else { run + 1 };
        out.push(c);
        if run >= UNBROKEN {
            out.push('\u{200b}');
            run = 0;
        }
    }
    out
}

fn words(text: &str, size: f32, color: Color) -> Node {
    let style = TextStyle {
        size,
        color,
        wrap: true,
        ..TextStyle::default()
    };
    let shown = breakable(text);
    let mut node = Node::new(Kind::Text {
        text: shown.clone(),
        style,
    });
    // Read as written.
    if shown != text {
        node.a11y.name = Some(text.to_owned());
    }
    node
}

fn column(gap: f32) -> Node {
    let mut node = Node::new(Kind::Box);
    node.layout.direction = Direction::Column;
    node.layout.row_gap = gap;
    node
}

fn row(justify: Justify) -> Node {
    let mut node = Node::new(Kind::Box);
    node.layout.direction = Direction::Row;
    node.layout.wrap = true;
    node.layout.column_gap = GAP;
    node.layout.row_gap = GAP;
    node.layout.justify = Some(justify);
    node.layout.align_items = Some(Align::Center);
    node
}

fn size_words(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn step_words(step: &PlanStep) -> String {
    let kind = match step.kind {
        StepKind::Download => "Download",
        StepKind::Replace => "Replace",
        StepKind::Delete => "Delete",
        StepKind::Snapshot => "Snapshot",
        StepKind::Move => "Move",
        StepKind::Info => "Note",
    };
    match step.bytes {
        Some(bytes) => format!("{kind}: {} ({})", step.text, size_words(bytes)),
        None => format!("{kind}: {}", step.text),
    }
}

fn totals(plan: &Plan) -> Option<String> {
    let download = plan
        .download
        .map(|b| format!("To download: {}", size_words(b)));
    let disk = plan.disk_delta.map(|d| {
        let sign = if d < 0 { "-" } else { "+" };
        format!("Disk use: {sign}{}", size_words(d.unsigned_abs()))
    });
    let free = plan.free.map(|b| format!("Free space: {}", size_words(b)));
    let parts: Vec<String> = download.into_iter().chain(disk).chain(free).collect();
    (!parts.is_empty()).then(|| parts.join(". "))
}

/// The verb for a button.
fn verb(plan: &Plan) -> &str {
    if plan.verb.is_empty() {
        "Confirm"
    } else {
        &plan.verb
    }
}

/// How a button looks and whether it can be pressed.
#[derive(Clone, Copy)]
struct Look {
    fill: Color,
    enabled: bool,
    checked: Option<bool>,
}

const PLAIN: Look = Look {
    fill: BUTTON,
    enabled: true,
    checked: None,
};
const MAIN: Look = Look {
    fill: PRIMARY,
    ..PLAIN
};
const GRAVE: Look = Look {
    fill: DANGER,
    ..PLAIN
};

fn enabled_if(enabled: bool, look: Look) -> Look {
    Look { enabled, ..look }
}

/// The card being built.
struct Builder {
    tree: Tree,
    ticket: Ticket,
    identity: Vec<u64>,
}

impl Builder {
    /// A control the app drew, with its own action number so that a press or a screen
    /// reader's request holds only for the same control of the same screen.
    fn control(&mut self, node: &mut Node, key: &str) {
        let mut hasher = DefaultHasher::new();
        (self.ticket, key).hash(&mut hasher);
        let index = u32::try_from(self.identity.len()).unwrap_or(u32::MAX);
        self.identity.push(hasher.finish());
        node.action = Some(ActionRef::new(index));
        node.drawn_by = DrawnBy::App(AppPart {
            widget: WIDGET,
            part: None,
        });
    }

    fn add(&mut self, parent: NodeId, key: &str, node: Node) -> Result<NodeId, PanelError> {
        Ok(self.tree.add_child(parent, key, node)?)
    }

    fn text(
        &mut self,
        parent: NodeId,
        key: &str,
        text: &str,
        color: Color,
    ) -> Result<(), PanelError> {
        self.add(parent, key, words(text, 14.0, color))?;
        Ok(())
    }

    fn status(&mut self, parent: NodeId, text: &str, color: Color) -> Result<(), PanelError> {
        let mut node = words(text, 14.0, color);
        node.a11y.role = Role::Status;
        self.add(parent, "dialog/status", node)?;
        Ok(())
    }

    fn button(
        &mut self,
        parent: NodeId,
        key: &str,
        label: &str,
        look: Look,
    ) -> Result<(), PanelError> {
        let mut node = Node::new(Kind::Box);
        node.layout.padding = sides(10.0);
        node.layout.min_height = Dim::Px(40.0);
        node.layout.align_items = Some(Align::Center);
        node.layout.justify = Some(Justify::Center);
        let fill = if look.enabled { look.fill } else { CARD };
        let mut paint = Paint::solid(fill);
        paint.radius = [6.0; 4];
        if !look.enabled {
            paint.border = [Some(Stroke {
                width: 1.0,
                color: BUTTON,
            }); 4];
        }
        if look.checked == Some(true) {
            paint.border = [Some(Stroke {
                width: 2.0,
                color: TEXT,
            }); 4];
        }
        node.paint = paint.clone();
        paint.ring = Some(Stroke {
            width: 2.0,
            color: RING,
        });
        node.focus_paint = Some(paint);
        node.a11y = A11y::control(Role::Button, label);
        node.a11y.disabled = !look.enabled;
        node.a11y.checked = look.checked;
        self.control(&mut node, key);
        let id = self.add(parent, key, node)?;
        let colour = if look.enabled { TEXT } else { DIM };
        self.text(id, &format!("{key}/text"), label, colour)
    }

    /// A one-line field.
    fn field(&mut self, parent: NodeId, name: &str, field: &FieldView) -> Result<(), PanelError> {
        let mut node = Node::new(Kind::Box);
        node.layout.padding = sides(10.0);
        node.layout.min_height = Dim::Px(40.0);
        let mut paint = Paint::solid(FIELD_FILL);
        paint.radius = [6.0; 4];
        paint.border = [Some(Stroke {
            width: 1.0,
            color: EDGE,
        }); 4];
        node.paint = paint.clone();
        paint.ring = Some(Stroke {
            width: 2.0,
            color: RING,
        });
        node.focus_paint = Some(paint);
        node.a11y = A11y::control(Role::TextInput, name);
        node.a11y.value = Some(field.text.clone());
        node.a11y.text_selection = Some(field.ends);
        self.control(&mut node, FIELD);
        let id = self.add(parent, FIELD, node)?;
        let text = field_text_node(field, "Type here");
        self.add(id, FIELD_TEXT, text)?;
        Ok(())
    }
}

/// The text of a field as it is drawn: what is typed, or `placeholder` while there is
/// nothing, with the caret, the selection and a composition.
pub(crate) fn field_text_node(field: &FieldView, placeholder: &str) -> Node {
    let empty = field.shown.is_empty();
    let (shown, color) = if empty {
        (placeholder, DIM)
    } else {
        (field.shown.as_str(), TEXT)
    };
    let mut text = Node::new(Kind::Text {
        text: shown.to_owned(),
        style: field_style(color),
    });
    // As wide as the field, however long the text is.
    text.layout.min_width = Dim::Px(0.0);
    text.marks = Some(Box::new(TextMarks {
        caret: field.caret.map(|at| if empty { 0 } else { at }),
        selection: if empty { 0..0 } else { field.selection.clone() },
        preedit: field.preedit.clone(),
        scroll: field.scroll,
        caret_color: TEXT,
        selection_color: SELECTED,
    }));
    text
}

/// A laid-out field: the keys of its box and of its text.
pub(crate) struct FieldAt<'a> {
    pub(crate) tree: &'a Tree,
    pub(crate) laid: &'a Laid,
    pub(crate) field: &'a str,
    pub(crate) text: &'a str,
}

impl<'a> FieldAt<'a> {
    /// The text node, with its layout and how far it is moved left.
    fn placed(&self) -> Option<(&'a Placed, &'a TextLayout, f32)> {
        let node = self.tree.by_key(self.text)?;
        let marks = self.tree.get(node)?.marks.as_deref()?;
        let placed = self.laid.get(node)?;
        let layout = self.laid.text(node)?;
        let width = (placed.content.x1 - placed.content.x0) as f32;
        let offset = marks.caret.unwrap_or(marks.selection.end);
        Some((
            placed,
            layout,
            layout.scroll_for(marks.scroll, offset, width),
        ))
    }

    /// How far the text is moved left, to carry on from in the next frame.
    pub(crate) fn scroll(&self) -> Option<f32> {
        self.placed().map(|(.., scroll)| scroll)
    }

    /// Where the caret is in the window, for an input method.
    pub(crate) fn caret(&self) -> Option<TextRect> {
        let (placed, layout, scroll) = self.placed()?;
        let marks = self
            .tree
            .get(self.tree.by_key(self.text)?)?
            .marks
            .as_deref()?;
        let at = layout.caret_rect(marks.preedit.as_ref().map_or_else(
            || marks.caret.unwrap_or(marks.selection.end),
            |preedit| preedit.start,
        ));
        let (x, y) = (placed.content.x0 as f32 - scroll, placed.content.y0 as f32);
        Some(TextRect {
            x0: x + at.x0,
            y0: y + at.y0,
            x1: x + at.x1,
            y1: y + at.y1,
        })
    }

    /// A point from the corner of the field's box as one in its text.
    pub(crate) fn point(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let field = self.laid.rect(self.tree.by_key(self.field)?)?;
        let (placed, _, scroll) = self.placed()?;
        Some((
            (field.x0 + x - placed.content.x0) as f32 + scroll,
            (field.y0 + y - placed.content.y0) as f32,
        ))
    }
}

/// The colour of what is typed.
pub(crate) const FIELD_TEXT_COLOR: Color = TEXT;

/// How a field's text is shaped: by the drawing and by the edit buffer alike.
pub(crate) fn field_style(color: Color) -> TextStyle {
    TextStyle {
        size: 14.0,
        color,
        ..TextStyle::default()
    }
}

/// What the dialog is called, and how it takes input.
fn heading(content: &Content) -> (String, LayerKind, Option<&'static str>) {
    match content {
        Content::Planning { confirming } => {
            let words = if *confirming {
                "Checking the plan again…"
            } else {
                "Working out what to do…"
            };
            (words.to_owned(), LayerKind::Dialog, None)
        }
        Content::Plan { plan, ask, .. } => {
            let title = if plan.title.is_empty() {
                verb(plan).to_owned()
            } else {
                plan.title.clone()
            };
            match ask {
                // On nothing: Enter is the card's confirm.
                Ask::Confirm => (title, LayerKind::Dialog, None),
                // The default answer is no.
                Ask::Typed { .. } | Ask::Hold(_) => (title, LayerKind::Alert, Some(CANCEL)),
            }
        }
        Content::Secret { sign_in, .. } => {
            let title = if *sign_in {
                "Sign in to GOG"
            } else {
                "A secret is needed"
            };
            (title.to_owned(), LayerKind::Dialog, Some(CANCEL))
        }
        Content::Text { .. } => (
            "A value is needed".to_owned(),
            LayerKind::Dialog,
            Some(CANCEL),
        ),
        Content::Choice { .. } => ("Choose one".to_owned(), LayerKind::Dialog, Some(CANCEL)),
        Content::YesNo { .. } => ("A question".to_owned(), LayerKind::Dialog, Some(CANCEL)),
        Content::Offer { .. } => (
            "Check before you go on".to_owned(),
            LayerKind::Dialog,
            Some(CANCEL),
        ),
        Content::Conflicts { .. } => (
            "Saves changed on both sides".to_owned(),
            LayerKind::Dialog,
            Some(CANCEL),
        ),
        Content::Error(error) => (error.title.clone(), LayerKind::Alert, Some(CLOSE)),
    }
}

/// How many entries of `content` can be split over pages.
fn items(content: &Content) -> usize {
    match content {
        Content::Plan { plan, .. } => plan.steps.len(),
        Content::Choice { options, .. } => options.len(),
        Content::Conflicts { files, .. } => files.len(),
        Content::Error(error) => 1 + usize::from(!error.hint.is_empty()),
        _ => 0,
    }
}

/// What is above the list.
fn above(b: &mut Builder, card: NodeId, content: &Content) -> Result<(), PanelError> {
    match content {
        Content::Plan { plan, changed, .. } => {
            if !plan.subject.is_empty() {
                b.text(card, "dialog/subject", &plan.subject, DIM)?;
            }
            if *changed {
                b.status(card, "The plan changed. Check it again.", WARN)?;
            }
        }
        Content::Secret { label, .. }
        | Content::Text { label, .. }
        | Content::Choice { label, .. }
        | Content::YesNo { label }
        | Content::Offer { label, .. } => b.text(card, "dialog/label", label, TEXT)?,
        Content::Conflicts { .. } => b.text(
            card,
            "dialog/label",
            "Pick which copy of each file to keep. Nothing changes until all are picked.",
            TEXT,
        )?,
        Content::Planning { .. } | Content::Error(_) => {}
    }
    Ok(())
}

/// Entry `i` of the list.
fn item(b: &mut Builder, list: NodeId, content: &Content, i: usize) -> Result<(), PanelError> {
    let key = format!("dialog/item/{i}");
    match content {
        Content::Plan { plan, .. } => {
            if let Some(step) = plan.steps.get(i) {
                b.text(list, &key, &step_words(step), TEXT)?;
            }
        }
        Content::Choice { options, .. } => {
            if let Some(option) = options.get(i) {
                let at = b.add(list, &key, column(0.0))?;
                b.button(at, &format!("{OPTION}{i}"), option, PLAIN)?;
            }
        }
        Content::Conflicts { files, picks } => {
            if let Some(file) = files.get(i) {
                let at = b.add(list, &key, column(4.0))?;
                b.text(at, &format!("{key}/name"), &file.name, TEXT)?;
                let both = format!("This computer: {}. Cloud: {}.", file.local, file.cloud);
                b.text(at, &format!("{key}/copies"), &both, DIM)?;
                let buttons = b.add(at, &format!("{key}/buttons"), row(Justify::Start))?;
                let pick = picks.get(i).copied().flatten();
                for (prefix, side, label) in [
                    (LOCAL, Side::Local, "Keep this computer's"),
                    (CLOUD, Side::Cloud, "Keep the cloud's"),
                ] {
                    let look = Look {
                        checked: Some(pick == Some(side)),
                        ..PLAIN
                    };
                    let label = format!("{label} {}", file.name);
                    b.button(buttons, &format!("{prefix}{i}"), &label, look)?;
                }
            }
        }
        Content::Error(error) => {
            if i == 0 {
                // The class word is always shown.
                let first = format!("{}: {}", error.class, error.message);
                b.text(list, &key, &first, TEXT)?;
            } else {
                b.text(list, &key, &error.hint, TEXT)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// What is below the list, down to the buttons.
fn below(b: &mut Builder, card: NodeId, content: &Content) -> Result<(), PanelError> {
    match content {
        Content::Plan { plan, ask, .. } => {
            if let Some(totals) = totals(plan) {
                b.text(card, "dialog/totals", &totals, TEXT)?;
            }
            for (i, warning) in plan.warnings.iter().enumerate() {
                let line = format!("Warning: {warning}");
                b.text(card, &format!("dialog/warning/{i}"), &line, WARN)?;
            }
            let undo = if plan.reversible {
                "This can be undone."
            } else {
                "This can't be undone."
            };
            b.text(card, "dialog/reversible", undo, TEXT)?;
            b.text(
                card,
                "dialog/footnote",
                "Nothing changes until you confirm.",
                DIM,
            )?;
            match ask {
                Ask::Confirm => {}
                Ask::Typed { want, typed } => {
                    let ask = format!("Type \u{201c}{want}\u{201d} to confirm.");
                    b.text(card, "dialog/ask", &ask, TEXT)?;
                    b.field(card, &format!("Type the title: {want}"), typed)?;
                }
                Ask::Hold(hold) => {
                    b.text(
                        card,
                        "dialog/ask",
                        "Press and hold the button for one second to confirm.",
                        TEXT,
                    )?;
                    // Always there, so that nothing moves while the button is held.
                    let mut track = Node::new(Kind::Box);
                    track.layout.height = Dim::Px(6.0);
                    track.paint = Paint::solid(FIELD_FILL);
                    let track = b.add(card, "dialog/track", track)?;
                    let part = match hold {
                        Hold::Holding(part) => part.clamp(0.0, 1.0),
                        Hold::Idle | Hold::Armed => 0.0,
                    };
                    let mut done = Node::new(Kind::Box);
                    done.layout.height = Dim::Px(6.0);
                    done.layout.width = Dim::Pct(part * 100.0);
                    done.paint = Paint::solid(RING);
                    b.add(track, "dialog/done", done)?;
                    let state = match hold {
                        Hold::Idle => "Not held.",
                        Hold::Armed => "Activate again to start holding.",
                        Hold::Holding(_) => "Holding…",
                    };
                    b.status(card, state, DIM)?;
                }
            }
        }
        Content::Secret { field, sign_in, .. } => {
            let colour = match field {
                Field::Rejected | Field::NothingToPaste => BAD,
                Field::Empty | Field::Pasted => TEXT,
            };
            b.status(card, field.words(*sign_in), colour)?;
        }
        Content::Text { label, value } => b.field(card, label, value)?,
        _ => {}
    }
    Ok(())
}

fn buttons(
    b: &mut Builder,
    at: NodeId,
    content: &Content,
    complete: bool,
) -> Result<(), PanelError> {
    if let Content::Error(_) = content {
        return b.button(at, CLOSE, "Close", MAIN);
    }
    b.button(at, CANCEL, "Cancel", PLAIN)?;
    match content {
        Content::Plan { plan, ask, .. } => match ask {
            Ask::Confirm => b.button(at, CONFIRM, verb(plan), enabled_if(complete, MAIN))?,
            Ask::Typed { want, typed } => {
                let matches = complete && typed.text.trim() == want;
                b.button(at, TYPED, verb(plan), enabled_if(matches, GRAVE))?;
            }
            Ask::Hold(_) => {
                let label = format!("Hold to {}", verb(plan));
                b.button(at, HOLD_BUTTON, &label, enabled_if(complete, GRAVE))?;
            }
        },
        // The rest only offer what can be read.
        _ if !complete => {}
        Content::Secret { field, sign_in, .. } => {
            if *sign_in {
                b.button(at, OPEN, "Open the page again", PLAIN)?;
            }
            if *field == Field::Pasted {
                b.button(at, CLEAR, "Clear", PLAIN)?;
            }
            b.button(at, PASTE, "Paste", PLAIN)?;
            if *field == Field::Pasted {
                let send = if *sign_in { "Sign in" } else { "Send" };
                b.button(at, SUBMIT, send, MAIN)?;
            }
        }
        Content::Text { .. } => b.button(at, OK, "OK", MAIN)?,
        Content::Offer { yes, .. } => b.button(at, YES, yes, MAIN)?,
        Content::YesNo { .. } => {
            b.button(at, NO, "No", PLAIN)?;
            b.button(at, YES, "Yes", MAIN)?;
        }
        Content::Conflicts { files, picks } => {
            let all = picks.len() == files.len() && picks.iter().all(Option::is_some);
            b.button(at, APPLY, "Apply", enabled_if(all, MAIN))?;
        }
        Content::Planning { .. } | Content::Choice { .. } | Content::Error(_) => {}
    }
    Ok(())
}

/// What to build: some of the list, or only what says the window is too small.
struct Pick {
    from: usize,
    to: usize,
    /// The page and how many there are, when there is more than one.
    pager: Option<(usize, usize)>,
    fits: bool,
}

struct Built {
    tree: Tree,
    laid: Laid,
    identity: Vec<u64>,
}

impl Built {
    /// Whether anything would be cut off: the card past the window, or something past
    /// the card's sides.
    fn cut_off(&self, room: f32) -> bool {
        let Some(card) = self
            .tree
            .by_key("dialog/card")
            .and_then(|id| Some((id, self.laid.rect(id)?)))
        else {
            return true;
        };
        let (id, card) = card;
        let past_a_side = self.tree.iter().any(|node| {
            let mut above = self.tree.parent(node);
            while let Some(parent) = above.filter(|&p| p != id) {
                above = self.tree.parent(parent);
            }
            above.is_some()
                && self
                    .laid
                    .rect(node)
                    .is_some_and(|r| r.x0 < card.x0 - 0.5 || r.x1 > card.x1 + 0.5)
        });
        past_a_side || (card.y1 - card.y0) as f32 > room
    }

    fn height(&self, key: &str) -> Option<f32> {
        let rect = self.laid.rect(self.tree.by_key(key)?)?;
        Some((rect.y1 - rect.y0) as f32)
    }
}

fn assemble(
    fonts: &mut FontSet,
    (width, height): (f32, f32),
    ticket: Ticket,
    content: &Content,
    what: &Pick,
) -> Result<Built, PanelError> {
    let (title, ..) = heading(content);
    let mut root = Node::new(Kind::Box);
    root.layout.width = Dim::Px(width);
    root.layout.height = Dim::Px(height);
    root.layout.direction = Direction::Column;
    root.layout.align_items = Some(Align::Center);
    root.layout.justify = Some(Justify::Center);
    root.paint = Paint::solid(SCRIM);
    root.a11y.name = Some(title.clone());
    let mut b = Builder {
        tree: Tree::new("dialog", root),
        ticket,
        identity: Vec::new(),
    };
    let root = b.tree.root();

    let mut card = column(GAP);
    card.layout.width = Dim::Px((width - 32.0).clamp(0.0, CARD_WIDTH));
    card.layout.padding = sides(PADDING);
    // Its height is what its content takes, so that too much of it can be seen.
    card.layout.shrink = 0.0;
    card.paint = Paint::solid(CARD);
    card.paint.radius = [10.0; 4];
    card.paint.border = [Some(Stroke {
        width: 1.0,
        color: EDGE,
    }); 4];
    let card = b.add(root, "dialog/card", card)?;

    let mut head = words(&title, 18.0, TEXT);
    head.a11y.role = Role::Heading;
    b.add(card, "dialog/heading", head)?;

    if what.fits {
        above(&mut b, card, content)?;
        if what.from < what.to {
            let list = b.add(card, "dialog/list", column(GAP))?;
            for i in what.from..what.to {
                item(&mut b, list, content, i)?;
            }
        }
        if let Some((page, pages)) = what.pager {
            let pager = b.add(card, "dialog/pager", row(Justify::Center))?;
            b.button(pager, PREVIOUS, "Previous", enabled_if(page > 0, PLAIN))?;
            let of = format!("Page {} of {pages}", page + 1);
            let mut status = words(&of, 14.0, DIM);
            status.a11y.role = Role::Status;
            b.add(pager, "dialog/page", status)?;
            b.button(pager, NEXT, "Next", enabled_if(page + 1 < pages, PLAIN))?;
        }
        below(&mut b, card, content)?;
    } else {
        b.status(card, "Make the window larger to see all of this.", WARN)?;
    }
    let at = b.add(card, "dialog/buttons", row(Justify::End))?;
    buttons(&mut b, at, content, what.fits)?;

    let laid = layout(
        &b.tree,
        fonts,
        Viewport::new(width, height),
        &Scroll::none(),
    )?;
    Ok(Built {
        tree: b.tree,
        laid,
        identity: b.identity,
    })
}

/// Splits entries of these heights into pages of at most `room`; `None` when one alone
/// is taller.
fn paginate(heights: &[f32], room: f32) -> Option<Vec<(usize, usize)>> {
    let mut pages = Vec::new();
    let (mut from, mut used) = (0, 0.0);
    for (i, &h) in heights.iter().enumerate() {
        if h > room {
            return None;
        }
        if i > from && used + GAP + h > room {
            pages.push((from, i));
            (from, used) = (i, h);
        } else {
            used += if i > from { GAP + h } else { h };
        }
    }
    pages.push((from, heights.len()));
    Some(pages)
}

impl Dialog {
    /// `page` is which page of a long list to show; one past the last shows the last.
    pub(crate) fn new(
        fonts: &mut FontSet,
        size: (f32, f32),
        ticket: Ticket,
        content: &Content,
        page: usize,
    ) -> Result<Dialog, PanelError> {
        let count = items(content);
        let room = size.1 - MARGIN;

        let all = Pick {
            from: 0,
            to: count,
            pager: None,
            fits: true,
        };
        let mut built = assemble(fonts, size, ticket, content, &all)?;
        let (mut page_at, mut pages, mut complete) = (0, 1, true);
        if built.cut_off(room) {
            complete = false;
            if count > 1 {
                // With the pager, to see how much room the rest leaves for the list.
                let paged = Pick {
                    pager: Some((1, 3)),
                    ..all
                };
                let whole = assemble(fonts, size, ticket, content, &paged)?;
                let heights: Vec<f32> = (0..count)
                    .map(|i| {
                        whole
                            .height(&format!("dialog/item/{i}"))
                            .unwrap_or(f32::MAX)
                    })
                    .collect();
                let card = whole.height("dialog/card").unwrap_or(f32::MAX);
                let list = whole.height("dialog/list").unwrap_or(0.0);
                if let Some(split) = paginate(&heights, room - (card - list)) {
                    let at = page.min(split.len() - 1);
                    let (from, to) = split[at];
                    let what = Pick {
                        from,
                        to,
                        pager: Some((at, split.len())),
                        fits: true,
                    };
                    let one = assemble(fonts, size, ticket, content, &what)?;
                    if !one.cut_off(room) {
                        built = one;
                        (page_at, pages, complete) = (at, split.len(), true);
                    }
                }
            }
            if !complete {
                let nothing = Pick {
                    from: 0,
                    to: 0,
                    pager: None,
                    fits: false,
                };
                built = assemble(fonts, size, ticket, content, &nothing)?;
            }
        }
        let (_, kind, initial) = heading(content);
        Ok(Dialog {
            tree: built.tree,
            laid: built.laid,
            ticket,
            kind,
            initial,
            identity: built.identity,
            complete,
            error: matches!(content, Content::Error(_)),
            page: page_at,
            pages,
        })
    }

    pub(crate) fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// Whether all of it could be shown. If not, nothing may answer it, and the model
    /// is not told it was presented.
    pub(crate) fn answerable(&self) -> bool {
        self.complete
    }

    /// Whether it is an error alert, which is closed only by a press made once it has
    /// had time to be read.
    pub(crate) fn is_error(&self) -> bool {
        self.error
    }

    /// The page on screen, and how many there are.
    pub(crate) fn page(&self) -> (usize, usize) {
        (self.page, self.pages)
    }

    /// One screen of the safety flow is one layer: what is typed, pasted or turned to
    /// does not make it another.
    pub(crate) fn layer(&self) -> Layer<'_> {
        let mut hasher = DefaultHasher::new();
        ("dialog", self.ticket).hash(&mut hasher);
        let id = LayerId(hasher.finish());
        let layer = Layer::new(id, self.kind, &self.tree, &self.laid)
            .identity(&self.identity)
            .hold(HOLDS);
        match self.initial {
            Some(initial) => layer.initial_focus(initial),
            None => layer,
        }
    }

    fn field(&self) -> FieldAt<'_> {
        FieldAt {
            tree: &self.tree,
            laid: &self.laid,
            field: FIELD,
            text: FIELD_TEXT,
        }
    }

    /// How far the field's text is moved left, to carry on from in the next frame.
    pub(crate) fn field_scroll(&self) -> Option<f32> {
        self.field().scroll()
    }

    /// Where the field's caret is in the window, for an input method.
    pub(crate) fn field_caret(&self) -> Option<TextRect> {
        self.field().caret()
    }

    /// A point from the corner of the field's box as one in its text.
    pub(crate) fn field_point(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        self.field().point(x, y)
    }

    /// Adds the dialog's drawing to `list`, with the ring on the control `focus` names.
    pub(crate) fn draw(&self, list: &mut DisplayList, focus: Option<&str>) {
        let state = match focus.and_then(|key| self.tree.by_key(key)) {
            Some(node) => SceneState::new().with_focus(node),
            None => SceneState::new(),
        };
        // The dialog has no images, so no image ids can clash.
        list.ops.extend(build(&self.tree, &self.laid, &state).ops);
    }

    /// The tree, for tests.
    #[cfg(test)]
    pub(crate) fn tree(&self) -> &Tree {
        &self.tree
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_of_a_screen_holds_no_text() {
        let typed = FieldView {
            text: "hush".to_owned(),
            shown: "hushed".to_owned(),
            ..FieldView::default()
        };
        let screens = [
            Content::Text {
                label: "private".to_owned(),
                value: typed.clone(),
            },
            Content::Plan {
                plan: Plan::default(),
                changed: false,
                ask: Ask::Typed {
                    want: "private".to_owned(),
                    typed,
                },
            },
        ];
        let printed = format!("{screens:?}");
        assert!(printed.contains("bytes: 4"), "{printed}");
        assert!(
            !printed.contains("hush") && !printed.contains("private"),
            "{printed}"
        );
    }

    #[test]
    fn pages_fill_the_room_and_no_entry_is_lost() {
        let heights = [20.0; 10];
        let pages = paginate(&heights, 80.0).expect("fits");
        assert_eq!(pages, [(0, 3), (3, 6), (6, 9), (9, 10)]);
        assert_eq!(paginate(&heights, 1000.0), Some(vec![(0, 10)]));
        assert_eq!(paginate(&[20.0, 90.0], 80.0), None);
        assert_eq!(paginate(&heights, -1.0), None);
    }

    #[test]
    fn sizes_and_long_runs() {
        assert_eq!(size_words(999), "999 B");
        assert_eq!(size_words(1_500), "1.5 KB");
        assert_eq!(size_words(1_999_999), "2.0 MB");
        assert_eq!(size_words(52_300_000_000), "52.3 GB");
        let path = "a".repeat(70);
        assert_eq!(breakable(&path).matches('\u{200b}').count(), 2);
        assert_eq!(breakable("short words only"), "short words only");
    }
}

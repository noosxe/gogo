//! The app's own dialogs (design 26 §5, depth A): the sign-in code dialog and the error
//! screen (S9). The app builds them itself; nothing of the skin is in them, and no skin
//! can read them (S1).
//!
//! The sign-in dialog takes the code by pasting only. It says whether a code is there and
//! never shows it: no characters, no dots and no length, on screen or to assistive
//! technology.
//!
//! Fixed colours for now; styling them with a skin's tokens is a follow-up.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};

use gogo_gui_model::Ticket;
use gogo_ui_interact::{Layer, LayerId, LayerKind};
use gogo_ui_ir::{
    A11y, Align, AppPart, Color, Dim, Direction, DrawnBy, Justify, Kind, Node, NodeId, Paint, Role,
    Sides, Stroke, TextStyle, Tree, WidgetId,
};
use gogo_ui_layout::{Laid, Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, SceneState, build};
use gogo_ui_text::FontSet;

use crate::panel::PanelError;

/// The keys of the controls.
pub(crate) const PASTE: &str = "dialog/paste";
pub(crate) const CLEAR: &str = "dialog/clear";
pub(crate) const OPEN: &str = "dialog/open";
pub(crate) const SUBMIT: &str = "dialog/submit";
pub(crate) const CANCEL: &str = "dialog/cancel";
pub(crate) const CLOSE: &str = "dialog/close";

/// The widget id of these controls. The binder numbers a skin's widgets from 0, and
/// these are in a layer of their own.
const WIDGET: WidgetId = WidgetId::new(u32::MAX);

const SCRIM: Color = Color([0x00, 0x00, 0x00, 0xb0]);
const CARD: Color = Color([0x1c, 0x1f, 0x26, 0xff]);
const EDGE: Color = Color([0x8a, 0x92, 0xa6, 0xff]);
const BUTTON: Color = Color([0x2c, 0x31, 0x3d, 0xff]);
const PRIMARY: Color = Color([0x2b, 0x50, 0xa8, 0xff]);
const RING: Color = Color([0x9d, 0xba, 0xff, 0xff]);
const TEXT: Color = Color([0xf5, 0xf5, 0xf5, 0xff]);
const BAD: Color = Color([0xff, 0xa5, 0x9d, 0xff]);

/// What the sign-in dialog's field holds, in words.
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
    pub(crate) fn words(self) -> &'static str {
        match self {
            Field::Empty => "No code pasted yet.",
            Field::Pasted => "A code is pasted.",
            Field::Rejected => "That was not a GOG sign-in address or code.",
            Field::NothingToPaste => "There was no text to paste.",
        }
    }
}

/// What a dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Content {
    /// `label` says what to paste; it holds no secret.
    SignIn { label: String, field: Field },
    Error {
        class: String,
        title: String,
        message: String,
        hint: String,
    },
}

/// A dialog laid out over a window: while it is up only it takes input.
pub(crate) struct Dialog {
    tree: Tree,
    laid: Laid,
    ticket: Ticket,
    kind: LayerKind,
    initial: &'static str,
}

fn sides(v: f32) -> Sides<f32> {
    Sides {
        top: v,
        right: v,
        bottom: v,
        left: v,
    }
}

fn words(text: &str, size: f32, color: Color) -> Node {
    let style = TextStyle {
        size,
        color,
        wrap: true,
        max_lines: 8,
        ..TextStyle::default()
    };
    Node::new(Kind::Text {
        text: text.to_owned(),
        style,
    })
}

fn button(
    tree: &mut Tree,
    row: NodeId,
    key: &str,
    label: &str,
    fill: Color,
) -> Result<(), PanelError> {
    let mut node = Node::new(Kind::Box);
    node.layout.padding = sides(10.0);
    node.layout.min_height = Dim::Px(40.0);
    node.layout.align_items = Some(Align::Center);
    node.layout.justify = Some(Justify::Center);
    let mut paint = Paint::solid(fill);
    paint.radius = [6.0; 4];
    node.paint = paint.clone();
    paint.ring = Some(Stroke {
        width: 2.0,
        color: RING,
    });
    node.focus_paint = Some(paint);
    node.a11y = A11y::control(Role::Button, label);
    node.drawn_by = DrawnBy::App(AppPart {
        widget: WIDGET,
        part: None,
    });
    let id = tree.add_child(row, key, node)?;
    tree.add_child(id, format!("{key}/text"), words(label, 14.0, TEXT))?;
    Ok(())
}

impl Dialog {
    pub(crate) fn new(
        fonts: &mut FontSet,
        (width, height): (f32, f32),
        ticket: Ticket,
        content: &Content,
    ) -> Result<Dialog, PanelError> {
        let (heading, kind, initial) = match content {
            Content::SignIn { .. } => ("Sign in to GOG", LayerKind::Dialog, CANCEL),
            Content::Error { title, .. } => (title.as_str(), LayerKind::Alert, CLOSE),
        };
        let mut root = Node::new(Kind::Box);
        root.layout.width = Dim::Px(width);
        root.layout.height = Dim::Px(height);
        root.layout.direction = Direction::Column;
        root.layout.align_items = Some(Align::Center);
        root.layout.justify = Some(Justify::Center);
        root.paint = Paint::solid(SCRIM);
        root.a11y.name = Some(heading.to_owned());
        let mut tree = Tree::new("dialog", root);
        let root = tree.root();

        let mut card = Node::new(Kind::Box);
        card.layout.direction = Direction::Column;
        card.layout.width = Dim::Px((width - 32.0).clamp(0.0, 560.0));
        card.layout.row_gap = 12.0;
        card.layout.padding = sides(20.0);
        card.paint = Paint::solid(CARD);
        card.paint.radius = [10.0; 4];
        card.paint.border = [Some(Stroke {
            width: 1.0,
            color: EDGE,
        }); 4];
        let card = tree.add_child(root, "dialog/card", card)?;

        let mut head = words(heading, 18.0, TEXT);
        head.a11y.role = Role::Heading;
        tree.add_child(card, "dialog/heading", head)?;

        let mut row = Node::new(Kind::Box);
        row.layout.direction = Direction::Row;
        row.layout.wrap = true;
        row.layout.column_gap = 8.0;
        row.layout.row_gap = 8.0;
        row.layout.justify = Some(Justify::End);

        match content {
            Content::SignIn { label, field } => {
                tree.add_child(card, "dialog/label", words(label, 14.0, TEXT))?;
                let colour = match field {
                    Field::Rejected | Field::NothingToPaste => BAD,
                    Field::Empty | Field::Pasted => TEXT,
                };
                let mut status = words(field.words(), 14.0, colour);
                status.a11y.role = Role::Status;
                tree.add_child(card, "dialog/status", status)?;
                let row = tree.add_child(card, "dialog/buttons", row)?;
                button(&mut tree, row, CANCEL, "Cancel", BUTTON)?;
                button(&mut tree, row, OPEN, "Open the page again", BUTTON)?;
                if *field == Field::Pasted {
                    button(&mut tree, row, CLEAR, "Clear", BUTTON)?;
                }
                button(&mut tree, row, PASTE, "Paste", BUTTON)?;
                if *field == Field::Pasted {
                    button(&mut tree, row, SUBMIT, "Sign in", PRIMARY)?;
                }
            }
            Content::Error {
                class,
                message,
                hint,
                ..
            } => {
                let first = format!("{class}: {message}");
                tree.add_child(card, "dialog/message", words(&first, 14.0, TEXT))?;
                if !hint.is_empty() {
                    tree.add_child(card, "dialog/hint", words(hint, 14.0, TEXT))?;
                }
                let row = tree.add_child(card, "dialog/buttons", row)?;
                button(&mut tree, row, CLOSE, "Close", PRIMARY)?;
            }
        }

        let laid = layout(&tree, fonts, Viewport::new(width, height), &Scroll::none())?;
        Ok(Dialog {
            tree,
            laid,
            ticket,
            kind,
            initial,
        })
    }

    pub(crate) fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// One screen of the safety flow is one layer: what is pasted does not make it
    /// another.
    pub(crate) fn layer(&self) -> Layer<'_> {
        let mut hasher = DefaultHasher::new();
        ("dialog", self.ticket).hash(&mut hasher);
        let id = LayerId(hasher.finish());
        Layer::new(id, self.kind, &self.tree, &self.laid).initial_focus(self.initial)
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

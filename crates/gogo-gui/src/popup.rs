//! The app's popups (design 25 §4): the menus and select lists a skin's controls open,
//! drawn from the binder's [`PopupSlot`]s as a popup layer by the control, and the
//! parts the command palette ([`crate::palette`]) is drawn with.
//!
//! A popup is built again for every frame from the screen under it, and what it holds is
//! what was shown: choosing an entry runs that call, through the same
//! [`Model::invoke`](gogo_gui_model::Model::invoke) as a skin button.
//!
//! Nothing scrolls yet: a list that does not fit shows a window of rows, which follows
//! focus, with a button at an end that has more.
//!
//! Fixed colours for now, as the app's dialogs have (#640).

use std::hash::{DefaultHasher, Hasher as _};

use gogo_gui_model::Invocation;
use gogo_skin::{ActionCall, PopupContent, PopupSlot, Value};
use gogo_ui_interact::{Layer, LayerId, LayerKind, place};
use gogo_ui_ir::{
    A11y, ActionRef, Align, AppPart, Color, Dim, Direction, DrawnBy, Kind, Node, NodeId, Paint,
    Position, Role, Sides, Stroke, TextStyle, Tree, WidgetId,
};
use gogo_ui_layout::{Laid, Rect, Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, SceneState, build};
use gogo_ui_text::FontSet;

use crate::dialog::FieldAt;
use crate::page::Screen;
use crate::palette;
use crate::panel::PanelError;

/// Followed by an entry's index in the whole list.
pub(crate) const ITEM: &str = "popup/item/";
/// Shows the entries before the first shown.
pub(crate) const EARLIER: &str = "popup/earlier";
/// Shows the entries after the last shown.
pub(crate) const LATER: &str = "popup/later";

/// The widget id of these controls: not a skin's, and not the dialogs'.
const WIDGET: WidgetId = WidgetId::new(u32::MAX - 1);

pub(crate) const ROW: f32 = 36.0;
pub(crate) const PADDING: f32 = 6.0;
const NARROWEST: f64 = 220.0;
const WIDEST: f64 = 360.0;
/// Between the control and its popup.
const GAP: f64 = 4.0;
/// Left free at the window's edges.
const MARGIN: f64 = 8.0;

pub(crate) const CARD: Color = Color([0x1c, 0x1f, 0x26, 0xff]);
pub(crate) const EDGE: Color = Color([0x8a, 0x92, 0xa6, 0xff]);
pub(crate) const CHOSEN: Color = Color([0x2b, 0x50, 0xa8, 0xff]);
pub(crate) const RING: Color = Color([0x9d, 0xba, 0xff, 0xff]);
pub(crate) const TEXT: Color = Color([0xf5, 0xf5, 0xf5, 0xff]);
pub(crate) const DIM: Color = Color([0xa6, 0xad, 0xbd, 0xff]);

/// A popup that is open: which control of which screen it hangs from.
#[derive(Debug, Clone)]
pub(crate) struct Open {
    /// The page layer the control is on.
    pub(crate) page: LayerId,
    /// The control's tree key.
    pub(crate) opener: String,
    /// The window it was opened in: a popup does not follow a resize.
    pub(crate) size: (f32, f32),
    /// What it listed when it opened; one that came to list something else closes.
    listed: Option<u64>,
    /// The first entry shown.
    first: usize,
    /// How many entries the last build showed.
    rows: usize,
}

impl Open {
    pub(crate) fn new(page: LayerId, opener: String, size: (f32, f32)) -> Open {
        Open {
            page,
            opener,
            size,
            listed: None,
            first: 0,
            rows: 0,
        }
    }

    /// The button at an end of the window was pressed.
    pub(crate) fn turn(&mut self, key: &str) {
        let step = self.rows.max(1);
        if key == EARLIER {
            self.first = self.first.saturating_sub(step);
        } else {
            // The build keeps it inside the list.
            self.first += step;
        }
    }
}

/// What choosing a row does.
#[derive(Debug, Clone)]
pub(crate) enum Choice {
    /// A skin's call, of a menu item or a select.
    Call {
        call: Box<ActionCall>,
        /// `{value}`, for a select.
        value: Option<Value>,
    },
    /// One of the app's own, from the palette.
    Command(Invocation),
}

/// One entry of the list.
struct Entry {
    label: String,
    name: String,
    icon: Option<String>,
    hint: Option<String>,
    /// Marked as the select's value.
    current: Option<bool>,
    choice: Option<Choice>,
}

/// What the control of `slot` lists.
fn entries(screen: &Screen, slot: &PopupSlot) -> Vec<Entry> {
    let call = |action: Option<ActionRef>| action.and_then(|a| screen.actions.get(a)).cloned();
    match &slot.content {
        PopupContent::Menu(items) => items
            .iter()
            .map(|item| Entry {
                label: item.label.clone(),
                name: item.name.clone(),
                icon: item.icon.clone(),
                hint: item.key.clone(),
                current: None,
                choice: call(item.action).map(|call| Choice::Call {
                    call: Box::new(call),
                    value: None,
                }),
            })
            .collect(),
        PopupContent::Select(options) => {
            let own = screen.tree.get(slot.node).and_then(|n| n.action);
            let own = call(own);
            options
                .iter()
                .map(|option| Entry {
                    label: option.label.clone(),
                    name: option.label.clone(),
                    icon: None,
                    hint: None,
                    current: Some(option.current),
                    choice: own.clone().map(|call| Choice::Call {
                        call: Box::new(call),
                        value: Some(option.value.clone()),
                    }),
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

pub(crate) fn hash(value: impl std::hash::Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// What a row does, as a value that changes when it would do something else.
fn identity(entry: &Entry) -> u64 {
    let does = entry.choice.as_ref().map(|c| match c {
        Choice::Call { call, value } => {
            format!("{} {:?} {value:?}", call.spec.name, call.args)
        }
        Choice::Command(invocation) => format!("{invocation:?}"),
    });
    hash((&entry.name, does))
}

/// A popup laid out for one frame.
pub(crate) struct Drawn {
    tree: Tree,
    laid: Laid,
    pub(crate) id: LayerId,
    identity: Vec<u64>,
    initial: Option<String>,
    /// What Enter in the palette's field activates.
    submit: Option<String>,
    /// What each row of the list does, by its index.
    choices: Vec<Option<Choice>>,
    /// The palette's [`revision`](crate::palette::Open::revision) it shows; 0 for the
    /// others.
    pub(crate) revision: u64,
}

/// The box being filled.
pub(crate) struct Builder {
    tree: Tree,
    identity: Vec<u64>,
    id: LayerId,
}

impl Builder {
    /// A popup under the id `id`, in a window of `size`.
    pub(crate) fn new(id: LayerId, size: (f32, f32), mut root: Node) -> Builder {
        root.layout.width = Dim::Px(size.0);
        root.layout.height = Dim::Px(size.1);
        Builder {
            tree: Tree::new("popup", root),
            identity: Vec::new(),
            id,
        }
    }

    pub(crate) fn root(&self) -> NodeId {
        self.tree.root()
    }

    /// The tree and what each of its controls does, for a layer that is not a popup.
    pub(crate) fn into_parts(self) -> (Tree, Vec<u64>) {
        (self.tree, self.identity)
    }

    /// Lays it out.
    pub(crate) fn finish(
        self,
        fonts: &mut FontSet,
        (width, height): (f32, f32),
        (initial, submit): (Option<String>, Option<String>),
        choices: Vec<Option<Choice>>,
        revision: u64,
    ) -> Result<Drawn, PanelError> {
        let laid = layout(
            &self.tree,
            fonts,
            Viewport::new(width, height),
            &Scroll::none(),
        )?;
        Ok(Drawn {
            tree: self.tree,
            laid,
            id: self.id,
            identity: self.identity,
            initial,
            submit,
            choices,
            revision,
        })
    }

    /// A button at an end of a window of rows.
    pub(crate) fn end(&mut self, parent: NodeId, key: &str, label: &str) -> Result<(), PanelError> {
        let mut node = row(CARD);
        node.a11y = A11y::control(Role::Button, label);
        self.control(&mut node, hash(key));
        let id = self.add(parent, key, node)?;
        self.text(id, &format!("{key}/text"), label, DIM, 1.0)
    }

    /// Makes `node` a control the app drew, which does `what`.
    pub(crate) fn control(&mut self, node: &mut Node, what: u64) {
        let index = u32::try_from(self.identity.len()).unwrap_or(u32::MAX);
        self.identity.push(hash((self.id.0, what)));
        node.action = Some(ActionRef::new(index));
        node.drawn_by = DrawnBy::App(AppPart {
            widget: WIDGET,
            part: None,
        });
    }

    pub(crate) fn add(
        &mut self,
        parent: NodeId,
        key: &str,
        node: Node,
    ) -> Result<NodeId, PanelError> {
        Ok(self.tree.add_child(parent, key, node)?)
    }

    pub(crate) fn text(
        &mut self,
        parent: NodeId,
        key: &str,
        text: &str,
        color: Color,
        grow: f32,
    ) -> Result<(), PanelError> {
        let style = TextStyle {
            size: 14.0,
            color,
            max_lines: 1,
            ..TextStyle::default()
        };
        let mut node = Node::new(Kind::Text {
            text: text.to_owned(),
            style,
        });
        node.layout.grow = grow;
        self.add(parent, key, node)?;
        Ok(())
    }
}

/// A row's box: also how a focused one looks.
pub(crate) fn row(fill: Color) -> Node {
    let mut node = Node::new(Kind::Box);
    node.layout.direction = Direction::Row;
    node.layout.align_items = Some(Align::Center);
    node.layout.column_gap = 8.0;
    node.layout.height = Dim::Px(ROW);
    node.layout.shrink = 0.0;
    node.layout.padding = Sides {
        top: 0.0,
        right: 10.0,
        bottom: 0.0,
        left: 10.0,
    };
    let mut paint = Paint::solid(fill);
    paint.radius = [6.0; 4];
    node.paint = paint.clone();
    paint.ring = Some(Stroke {
        width: 2.0,
        color: RING,
    });
    node.focus_paint = Some(paint);
    node
}

/// What a list shows of itself.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shown {
    /// The box's width.
    width: f64,
    /// How many entries, from which one.
    rows: usize,
    first: usize,
    /// How tall each is.
    row: f32,
    /// Whether there are buttons at the ends that have more.
    buttons: bool,
}

/// The fewest entries a long list shows, so that one is beside the focused one on either
/// side.
const FEWEST: usize = 3;
/// How short a row may get in a window too small for [`FEWEST`] full ones (S7's least
/// target).
const SHORTEST: f32 = 24.0;

/// What a list of `total` entries by `anchor` shows. `focused` is the entry the window
/// of rows keeps in sight, with one more beside it where there is one, so Up and Down
/// always have somewhere to go.
///
/// It stays beside the control while that holds [`FEWEST`] entries and the two buttons;
/// otherwise it may cover the control and use the window's height, and in a window too
/// small even for that it drops the buttons and shortens its rows.
fn window_of(open: &Open, total: usize, anchor: Rect, focused: Option<usize>) -> Shown {
    let (window_width, window_height) = (f64::from(open.size.0), f64::from(open.size.1));
    let width = (anchor.x1 - anchor.x0).clamp(NARROWEST, WIDEST);
    let padding = 2.0 * f64::from(PADDING);
    let fit = |room: f64, row: f32| ((room - padding) / f64::from(row)).floor().max(0.0) as usize;
    let beside = place(
        anchor,
        (width, total as f64 * f64::from(ROW) + padding),
        (window_width, window_height),
        GAP,
        MARGIN,
    )
    .max_height;
    let whole = (window_height - 2.0 * MARGIN).max(0.0);
    let mut shown = Shown {
        width,
        rows: total,
        first: 0,
        row: ROW,
        buttons: false,
    };
    let mut slots = fit(beside, ROW);
    if slots < total && slots < FEWEST + 2 {
        slots = fit(whole, ROW);
    }
    if slots >= total {
        return shown;
    }
    if slots >= FEWEST + 2 {
        // Two rows go to the buttons at the ends.
        (shown.rows, shown.buttons) = (slots - 2, true);
    } else {
        let each = ((whole - padding) / FEWEST as f64).floor() as f32;
        shown.row = each.clamp(SHORTEST, ROW);
        shown.rows = fit(whole, shown.row).clamp(1, total);
    }
    let rows = shown.rows;
    let mut first = open.first.min(total - rows);
    if let Some(at) = focused {
        let margin = usize::from(rows > 2);
        first = first.min(at.saturating_sub(margin));
        first = first
            .max((at + margin + 1).saturating_sub(rows))
            .min(total - rows);
    }
    shown.first = first;
    shown
}

/// Where the box of `size` goes: by `anchor`, and wholly in the window.
fn corner(anchor: Rect, (width, height): (f64, f64), window: (f32, f32)) -> (f64, f64) {
    let window = (f64::from(window.0), f64::from(window.1));
    let at = place(anchor, (width, height), window, GAP, MARGIN);
    let lowest = (window.1 - MARGIN - height).max(0.0);
    (at.x, at.y.min(lowest).max(MARGIN.min(lowest)))
}

/// The key of the focused entry, as its index.
pub(crate) fn index_of(key: &str) -> Option<usize> {
    key.strip_prefix(ITEM)?.parse().ok()
}

impl Drawn {
    /// `open` over `screen`, with `focused` the key focus is on in this popup. `None`
    /// when the control is gone, lists nothing or came to list something else: the popup
    /// closes.
    pub(crate) fn new(
        fonts: &mut FontSet,
        open: &mut Open,
        id: LayerId,
        screen: &Screen,
        focused: Option<&str>,
    ) -> Result<Option<Drawn>, PanelError> {
        if screen.layer().id != open.page {
            return Ok(None);
        }
        let Some(node) = screen.tree.by_key(&open.opener) else {
            return Ok(None);
        };
        let (Some(slot), Some(anchor)) = (
            screen.popups.iter().find(|p| p.node == node),
            screen.laid.rect(node),
        ) else {
            return Ok(None);
        };
        let entries = entries(screen, slot);
        let listed = hash(entries.iter().map(identity).collect::<Vec<_>>());
        if entries.is_empty() || open.listed.is_some_and(|was| was != listed) {
            return Ok(None);
        }
        let current = entries.iter().position(|e| e.current == Some(true));
        let focus_at = focused.and_then(index_of).or_else(|| {
            // The first build shows the select's value.
            open.listed.is_none().then_some(current).flatten()
        });
        let shown = window_of(open, entries.len(), anchor, focus_at);
        let Shown {
            width, rows, first, ..
        } = shown;
        open.listed = Some(listed);
        open.first = first;
        open.rows = rows;

        let name = screen
            .tree
            .get(node)
            .and_then(|n| n.a11y.name.clone())
            .unwrap_or_default();
        let select = matches!(slot.content, PopupContent::Select(_));

        let mut b = Builder::new(id, open.size, Node::new(Kind::Box));

        let earlier = shown.buttons && first > 0;
        let later = shown.buttons && first + rows < entries.len();
        let ends = usize::from(earlier) + usize::from(later);
        let height = rows as f32 * shown.row + ends as f32 * ROW + 2.0 * PADDING;
        let (x, y) = corner(anchor, (width, f64::from(height)), open.size);

        let mut card = Node::new(Kind::Box);
        card.layout.position = Position::Absolute;
        card.layout.inset = Sides {
            top: Dim::Px(y as f32),
            left: Dim::Px(x as f32),
            right: Dim::Auto,
            bottom: Dim::Auto,
        };
        card.layout.width = Dim::Px(width as f32);
        card.layout.direction = Direction::Column;
        card.layout.padding = Sides {
            top: PADDING,
            right: PADDING,
            bottom: PADDING,
            left: PADDING,
        };
        card.paint = Paint::solid(CARD);
        card.paint.radius = [8.0; 4];
        card.paint.border = [Some(Stroke {
            width: 1.0,
            color: EDGE,
        }); 4];
        card.a11y.role = if select { Role::ListBox } else { Role::Menu };
        card.a11y.name = Some(name);
        let top = b.root();
        let card = b.add(top, "popup/box", card)?;

        if earlier {
            b.end(card, EARLIER, "More above")?;
        }
        let mut initial = None;
        let mut fallback = None;
        for (i, entry) in entries.iter().enumerate().skip(first).take(rows) {
            let key = format!("{ITEM}{i}");
            let chosen = entry.current == Some(true);
            let mut node = row(if chosen { CHOSEN } else { CARD });
            node.layout.height = Dim::Px(shown.row);
            let role = if select {
                Role::ListOption
            } else {
                Role::MenuItem
            };
            node.a11y = A11y::control(role, entry.name.clone());
            node.a11y.selected = entry.current;
            let enabled = entry.choice.is_some();
            if enabled {
                b.control(&mut node, identity(entry));
                fallback.get_or_insert_with(|| key.clone());
                if chosen {
                    initial = Some(key.clone());
                }
            } else {
                node.a11y.disabled = true;
            }
            let id = b.add(card, &key, node)?;
            let color = if enabled { TEXT } else { DIM };
            if let Some(icon) = &entry.icon {
                let mut mark = Node::new(Kind::Icon(icon.as_str().into()));
                mark.layout.width = Dim::Px(12.0);
                mark.layout.height = Dim::Px(12.0);
                mark.paint = Paint::solid(color);
                b.add(id, &format!("{key}/icon"), mark)?;
            }
            b.text(id, &format!("{key}/text"), &entry.label, color, 1.0)?;
            if let Some(hint) = &entry.hint {
                b.text(id, &format!("{key}/hint"), hint, DIM, 0.0)?;
            }
        }
        if later {
            b.end(card, LATER, "More below")?;
        }

        let choices = entries.into_iter().map(|e| e.choice).collect();
        let focus = (initial.or(fallback), None);
        b.finish(fonts, open.size, focus, choices, 0).map(Some)
    }

    pub(crate) fn layer(&self) -> Layer<'_> {
        let layer =
            Layer::new(self.id, LayerKind::Popup, &self.tree, &self.laid).identity(&self.identity);
        let layer = match &self.initial {
            Some(key) => layer.initial_focus(key),
            None => layer,
        };
        match &self.submit {
            Some(key) => layer.submit(key),
            None => layer,
        }
    }

    /// What the row with this key does.
    pub(crate) fn choice(&self, key: &str) -> Option<&Choice> {
        self.choices.get(index_of(key)?)?.as_ref()
    }

    /// The palette's field, as laid out.
    pub(crate) fn field(&self) -> FieldAt<'_> {
        FieldAt {
            tree: &self.tree,
            laid: &self.laid,
            field: palette::FIELD,
            text: palette::FIELD_TEXT,
        }
    }

    /// Adds the popup's drawing to `list`, with the ring on the control `focus` names.
    pub(crate) fn draw(&self, list: &mut DisplayList, focus: Option<&str>) {
        let state = match focus.and_then(|key| self.tree.by_key(key)) {
            Some(node) => SceneState::new().with_focus(node),
            None => SceneState::new(),
        };
        // A popup has no images, so no image ids can clash.
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

    fn anchor(y: f64) -> Rect {
        Rect {
            x0: 100.0,
            y0: y,
            x1: 200.0,
            y1: y + 30.0,
        }
    }

    #[test]
    fn a_long_list_shows_a_window_that_follows_focus() {
        let mut open = Open::new(LayerId(1), "more".to_owned(), (800.0, 400.0));
        let few = window_of(&open, 3, anchor(40.0), None);
        assert_eq!((few.width, few.rows, few.first), (NARROWEST, 3, 0));
        let Shown { rows, first, .. } = window_of(&open, 40, anchor(40.0), None);
        assert!(rows > 2 && rows < 40);
        assert_eq!(first, 0);
        // The last row but one in sight brings the next one in.
        let moved = window_of(&open, 40, anchor(40.0), Some(rows - 1)).first;
        assert_eq!(moved, 1);
        let end = window_of(&open, 40, anchor(40.0), Some(39)).first;
        assert_eq!(end, 40 - rows);
        open.first = moved;
        open.rows = rows;
        open.turn(LATER);
        assert_eq!(open.first, 1 + rows);
        open.turn(EARLIER);
        open.turn(EARLIER);
        assert_eq!(open.first, 0);
    }

    #[test]
    fn a_small_window_keeps_the_popup_inside_it() {
        // No room by the control for even one row and the two buttons.
        for height in [100.0, 160.0, 240.0] {
            let open = Open::new(LayerId(1), "more".to_owned(), (800.0, height));
            let shown = window_of(&open, 40, anchor(20.0), Some(5));
            // One entry on either side of the focused one, for Up and Down.
            assert!(shown.rows >= FEWEST, "{height}: {shown:?}");
            assert!((shown.first + 1..shown.first + shown.rows - 1).contains(&5));
            assert!(shown.row >= SHORTEST);
            let ends = if shown.buttons { 2.0 } else { 0.0 };
            let tall = shown.rows as f32 * shown.row + ends * ROW + 2.0 * PADDING;
            let (_, y) = corner(anchor(20.0), (shown.width, f64::from(tall)), open.size);
            assert!(y >= 0.0, "{height}: {y}");
            assert!(
                y + f64::from(tall) <= f64::from(height),
                "{height}: {y} {tall}"
            );
        }
    }
}

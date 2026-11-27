//! The menus and select lists a skin's controls open (design 25 §4), drawn by the app
//! from the binder's [`PopupSlot`]s as a popup layer by the control.
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

use gogo_skin::{ActionCall, PopupContent, PopupSlot, Value};
use gogo_ui_interact::{Layer, LayerId, LayerKind, place};
use gogo_ui_ir::{
    A11y, ActionRef, Align, AppPart, Color, Dim, Direction, DrawnBy, Kind, Node, NodeId, Paint,
    Position, Role, Sides, Stroke, TextStyle, Tree, WidgetId,
};
use gogo_ui_layout::{Laid, Rect, Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, SceneState, build};
use gogo_ui_text::FontSet;

use crate::page::Screen;
use crate::panel::PanelError;

/// Followed by an entry's index in the whole list.
const ITEM: &str = "popup/item/";
/// Shows the entries before the first shown.
pub(crate) const EARLIER: &str = "popup/earlier";
/// Shows the entries after the last shown.
pub(crate) const LATER: &str = "popup/later";

/// The widget id of these controls: not a skin's, and not the dialogs'.
const WIDGET: WidgetId = WidgetId::new(u32::MAX - 1);

const ROW: f32 = 36.0;
const PADDING: f32 = 6.0;
const NARROWEST: f64 = 220.0;
const WIDEST: f64 = 360.0;
/// Between the control and its popup.
const GAP: f64 = 4.0;
/// Left free at the window's edges.
const MARGIN: f64 = 8.0;

const CARD: Color = Color([0x1c, 0x1f, 0x26, 0xff]);
const EDGE: Color = Color([0x8a, 0x92, 0xa6, 0xff]);
const CHOSEN: Color = Color([0x2b, 0x50, 0xa8, 0xff]);
const RING: Color = Color([0x9d, 0xba, 0xff, 0xff]);
const TEXT: Color = Color([0xf5, 0xf5, 0xf5, 0xff]);
const DIM: Color = Color([0xa6, 0xad, 0xbd, 0xff]);

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
pub(crate) struct Choice {
    pub(crate) call: ActionCall,
    /// `{value}`, for a select.
    pub(crate) value: Option<Value>,
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
                choice: call(item.action).map(|call| Choice { call, value: None }),
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
                    choice: own.clone().map(|call| Choice {
                        call,
                        value: Some(option.value.clone()),
                    }),
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn hash(value: impl std::hash::Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// What a row does, as a value that changes when it would do something else.
fn identity(entry: &Entry) -> u64 {
    let does = entry
        .choice
        .as_ref()
        .map(|c| format!("{} {:?} {:?}", c.call.spec.name, c.call.args, c.value));
    hash((&entry.name, does))
}

/// A popup laid out for one frame.
pub(crate) struct Drawn {
    tree: Tree,
    laid: Laid,
    pub(crate) id: LayerId,
    identity: Vec<u64>,
    initial: Option<String>,
    /// What each row of the list does, by its index.
    choices: Vec<Option<Choice>>,
}

/// The box being filled.
struct Builder {
    tree: Tree,
    identity: Vec<u64>,
    id: LayerId,
}

impl Builder {
    /// Makes `node` a control the app drew, which does `what`.
    fn control(&mut self, node: &mut Node, what: u64) {
        let index = u32::try_from(self.identity.len()).unwrap_or(u32::MAX);
        self.identity.push(hash((self.id.0, what)));
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
fn row(fill: Color) -> Node {
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

/// How wide the box is, how many entries it shows and where the first of them is, for a
/// list of `total` by `anchor`. `focused` is the entry the window keeps in sight, with
/// one more beside it where there is one, so Up and Down always have somewhere to go.
fn window_of(
    open: &Open,
    total: usize,
    anchor: Rect,
    focused: Option<usize>,
) -> (f64, usize, usize) {
    let (window_width, window_height) = (f64::from(open.size.0), f64::from(open.size.1));
    let width = (anchor.x1 - anchor.x0).clamp(NARROWEST, WIDEST);
    let tall = |rows: usize| (rows as f32 * ROW + 2.0 * PADDING) as f64;
    let room = place(
        anchor,
        (width, tall(total)),
        (window_width, window_height),
        GAP,
        MARGIN,
    )
    .max_height;
    let fit = (((room - 2.0 * f64::from(PADDING)) / f64::from(ROW))
        .floor()
        .max(0.0)) as usize;
    if fit >= total {
        return (width, total, 0);
    }
    // Two rows go to the buttons at the ends.
    let rows = fit.saturating_sub(2).max(1);
    let mut first = open.first.min(total - rows);
    if let Some(at) = focused {
        let margin = usize::from(rows > 2);
        first = first.min(at.saturating_sub(margin));
        first = first
            .max((at + margin + 1).saturating_sub(rows))
            .min(total - rows);
    }
    (width, rows, first)
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
        let (width, rows, first) = window_of(open, entries.len(), anchor, focus_at);
        open.listed = Some(listed);
        open.first = first;
        open.rows = rows;

        let name = screen
            .tree
            .get(node)
            .and_then(|n| n.a11y.name.clone())
            .unwrap_or_default();
        let select = matches!(slot.content, PopupContent::Select(_));

        let mut root = Node::new(Kind::Box);
        root.layout.width = Dim::Px(open.size.0);
        root.layout.height = Dim::Px(open.size.1);
        let mut b = Builder {
            tree: Tree::new("popup", root),
            identity: Vec::new(),
            id,
        };

        let earlier = first > 0;
        let later = first + rows < entries.len();
        let count = rows + usize::from(earlier) + usize::from(later);
        let height = count as f32 * ROW + 2.0 * PADDING;
        let at = place(
            anchor,
            (width, f64::from(height)),
            (f64::from(open.size.0), f64::from(open.size.1)),
            GAP,
            MARGIN,
        );

        let mut card = Node::new(Kind::Box);
        card.layout.position = Position::Absolute;
        card.layout.inset = Sides {
            top: Dim::Px(at.y as f32),
            left: Dim::Px(at.x as f32),
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
        let top = b.tree.root();
        let card = b.add(top, "popup/box", card)?;

        let end = |b: &mut Builder, key: &str, label: &str| -> Result<(), PanelError> {
            let mut node = row(CARD);
            node.a11y = A11y::control(Role::Button, label);
            b.control(&mut node, hash(key));
            let id = b.add(card, key, node)?;
            b.text(id, &format!("{key}/text"), label, DIM, 1.0)
        };
        if earlier {
            end(&mut b, EARLIER, "More above")?;
        }
        let mut initial = None;
        let mut fallback = None;
        for (i, entry) in entries.iter().enumerate().skip(first).take(rows) {
            let key = format!("{ITEM}{i}");
            let chosen = entry.current == Some(true);
            let mut node = row(if chosen { CHOSEN } else { CARD });
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
            end(&mut b, LATER, "More below")?;
        }

        let laid = layout(
            &b.tree,
            fonts,
            Viewport::new(open.size.0, open.size.1),
            &Scroll::none(),
        )?;
        Ok(Some(Drawn {
            tree: b.tree,
            laid,
            id,
            identity: b.identity,
            initial: initial.or(fallback),
            choices: entries.into_iter().map(|e| e.choice).collect(),
        }))
    }

    pub(crate) fn layer(&self) -> Layer<'_> {
        let layer =
            Layer::new(self.id, LayerKind::Popup, &self.tree, &self.laid).identity(&self.identity);
        match &self.initial {
            Some(key) => layer.initial_focus(key),
            None => layer,
        }
    }

    /// What the row with this key does.
    pub(crate) fn choice(&self, key: &str) -> Option<&Choice> {
        self.choices.get(index_of(key)?)?.as_ref()
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
        let (width, rows, first) = window_of(&open, 3, anchor(40.0), None);
        assert_eq!((width, rows, first), (NARROWEST, 3, 0));
        let (_, rows, first) = window_of(&open, 40, anchor(40.0), None);
        assert!(rows > 2 && rows < 40);
        assert_eq!(first, 0);
        // The last row but one in sight brings the next one in.
        let (_, _, moved) = window_of(&open, 40, anchor(40.0), Some(rows - 1));
        assert_eq!(moved, 1);
        let (_, _, end) = window_of(&open, 40, anchor(40.0), Some(39));
        assert_eq!(end, 40 - rows);
        open.first = moved;
        open.rows = rows;
        open.turn(LATER);
        assert_eq!(open.first, 1 + rows);
        open.turn(EARLIER);
        open.turn(EARLIER);
        assert_eq!(open.first, 0);
    }
}

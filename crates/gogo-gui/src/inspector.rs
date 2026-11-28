//! The skin dev mode's inspector (design 25 §8): the app's own panel beside the page,
//! saying where the selected node comes from, what its expressions gave, and what is
//! around it.
//!
//! It exists only with `--dev`. The page is laid out in the width the panel leaves, so
//! the panel covers nothing of it. It reads the page's own traced bind and nothing else:
//! not the app's dialogs, not what was typed. What it shows goes to the display and to
//! assistive technology, never to stdout, the log or a file.

use gogo_skin::{Origin, Provenance};
use gogo_ui_interact::{Layer, LayerId, LayerKind};
use gogo_ui_ir::{
    A11y, Color, Dim, Direction, Kind, Node, NodeId, Position, Role, Sides, TextStyle, Tree,
};
use gogo_ui_layout::{Laid, Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, SceneState, build};
use gogo_ui_text::FontSet;

use crate::page::Screen;
use crate::panel::PanelError;
use crate::popup::{Builder, CARD, DIM, EDGE, TEXT, hash, row};

/// The most the panel takes of the window, in logical px, and never more than half.
const WIDEST: f32 = 360.0;
const PADDING: f32 = 10.0;
/// The size of the monospaced text, and how wide one of its characters is.
const SIZE: f32 = 12.0;
const ADVANCE: f32 = 0.6 * SIZE;
const FAMILY: &str = "JetBrains Mono";
const LINE: f32 = 18.0;
const PICK: f32 = 28.0;

const HEAD: Color = Color([0x9d, 0xba, 0xff, 0xff]);

const EARLIER: &str = "inspector-earlier";
const LATER: &str = "inspector-later";
const ITEM: &str = "inspector-pick:";

/// The inspector while it is open.
pub(crate) struct Inspector {
    /// The tree key of the selected node.
    selected: String,
    /// The page the key was chosen on: another route starts at its root again.
    on: Option<LayerId>,
    /// The first item of the list shown.
    first: usize,
    /// How many the last build showed.
    shown: usize,
}

/// One item of the panel's list.
#[derive(Debug, PartialEq)]
enum Item {
    Head(String),
    Line(String),
    Dim(String),
    /// A button that selects the node with `key`.
    Pick {
        label: String,
        key: String,
    },
}

impl Item {
    fn height(&self) -> f32 {
        match self {
            Item::Pick { .. } => PICK,
            _ => LINE,
        }
    }
}

/// The panel laid out for one frame.
pub(crate) struct Drawn {
    tree: Tree,
    laid: Laid,
    pub(crate) id: LayerId,
    identity: Vec<u64>,
    /// What each pick button selects, by the number in its key.
    picks: Vec<String>,
}

/// How wide the panel is in a window `window` px wide.
pub(crate) fn width(window: f32) -> f32 {
    (window / 2.0).min(WIDEST).floor()
}

fn id() -> LayerId {
    LayerId(hash("inspector"))
}

impl Inspector {
    pub(crate) fn new() -> Inspector {
        Inspector {
            selected: "root".to_owned(),
            on: None,
            first: 0,
            shown: 0,
        }
    }

    /// The tree key of the selected node.
    pub(crate) fn selected(&self) -> &str {
        &self.selected
    }

    fn select(&mut self, key: String) {
        self.selected = key;
        self.first = 0;
    }

    /// A button of `drawn` was activated. The key it selected, if it did.
    pub(crate) fn press(&mut self, drawn: &Drawn, key: &str) -> Option<&str> {
        let step = self.shown.max(1);
        match key {
            EARLIER => self.first = self.first.saturating_sub(step),
            LATER => self.first += step,
            _ => {
                let at: usize = key.strip_prefix(ITEM)?.parse().ok()?;
                self.select(drawn.picks.get(at)?.clone());
                return Some(&self.selected);
            }
        }
        None
    }

    /// The node of `screen` the selection stands for. One that is gone gives way to the
    /// nearest node around where it was.
    fn node(&mut self, screen: &Screen) -> NodeId {
        let page = screen.layer().id;
        if self.on.replace(page).is_some_and(|before| before != page) {
            self.select("root".to_owned());
        }
        let mut key = self.selected.as_str();
        let found = loop {
            if let Some(node) = screen.tree.by_key(key) {
                break node;
            }
            match key.rsplit_once('/') {
                Some((around, _)) => key = around,
                None => break screen.tree.root(),
            }
        };
        if screen.tree.key(found) != Some(self.selected.as_str()) {
            self.select(screen.tree.key(found).unwrap_or("root").to_owned());
        }
        found
    }

    /// The panel along the right edge of a window of `size`, about `screen`.
    pub(crate) fn draw(
        &mut self,
        fonts: &mut FontSet,
        (window_width, window_height): (f32, f32),
        screen: Option<&Screen>,
    ) -> Result<Drawn, PanelError> {
        let wide = width(window_width);
        let columns = ((wide - 2.0 * PADDING) / ADVANCE).floor().max(8.0) as usize;
        let items = match screen {
            Some(screen) => {
                let node = self.node(screen);
                items(screen, node, columns)
            }
            None => vec![Item::Dim("no screen yet".to_owned())],
        };

        // The title and the two buttons take their rows; the rest is the list's.
        let room = window_height - 2.0 * PADDING - LINE - 2.0 * PICK;
        self.first = self.first.min(items.len().saturating_sub(1));
        let mut used = 0.0;
        let count = items[self.first..]
            .iter()
            .take_while(|item| {
                used += item.height();
                used <= room
            })
            .count()
            .max(1);
        self.shown = count;

        let mut root = Node::new(Kind::Box);
        root.a11y.name = Some("Inspector".to_owned());
        let mut builder = Builder::new(id(), (window_width, window_height), root);
        let mut side = Node::new(Kind::Box);
        side.layout.position = Position::Absolute;
        side.layout.inset = Sides {
            top: Dim::Px(0.0),
            right: Dim::Px(0.0),
            bottom: Dim::Px(0.0),
            left: Dim::Auto,
        };
        side.layout.width = Dim::Px(wide);
        side.layout.direction = Direction::Column;
        side.layout.padding = Sides {
            top: PADDING,
            right: PADDING,
            bottom: PADDING,
            left: PADDING,
        };
        side.paint.fill = Some(gogo_ui_ir::Fill::Solid(CARD));
        side.paint.border = [
            None,
            None,
            None,
            Some(gogo_ui_ir::Stroke {
                width: 1.0,
                color: EDGE,
            }),
        ];
        side.a11y.role = Role::Region;
        side.a11y.name = Some("Inspector".to_owned());
        let at = builder.root();
        let side = builder.add(at, "side", side)?;
        line(&mut builder, side, "title", "Inspector · F12 closes", DIM)?;

        let more_before = self.first > 0;
        let more_after = self.first + count < items.len();
        if more_before {
            builder.end(side, EARLIER, "Earlier")?;
        }
        let mut picks = Vec::new();
        for (i, item) in items.into_iter().skip(self.first).take(count).enumerate() {
            let key = format!("item:{i}");
            match item {
                Item::Head(text) => line(&mut builder, side, &key, &text, HEAD)?,
                Item::Line(text) => line(&mut builder, side, &key, &text, TEXT)?,
                Item::Dim(text) => line(&mut builder, side, &key, &text, DIM)?,
                Item::Pick { label, key: target } => {
                    let key = format!("{ITEM}{}", picks.len());
                    let mut node = row(CARD);
                    node.layout.height = Dim::Px(PICK);
                    node.a11y = A11y::control(Role::Button, label.as_str());
                    builder.control(&mut node, hash(("pick", &target)));
                    let button = builder.add(side, &key, node)?;
                    line(
                        &mut builder,
                        button,
                        &format!("{key}/text"),
                        &clip(&label, columns.saturating_sub(4)),
                        TEXT,
                    )?;
                    picks.push(target);
                }
            }
        }
        if more_after {
            builder.end(side, LATER, "Later")?;
        }

        let (tree, identity) = builder.into_parts();
        let laid = layout(
            &tree,
            fonts,
            Viewport::new(window_width, window_height),
            &Scroll::none(),
        )?;
        Ok(Drawn {
            tree,
            laid,
            id: id(),
            identity,
            picks,
        })
    }
}

impl Drawn {
    /// Drawn beside the page; a press on it never reaches the page.
    pub(crate) fn layer(&self) -> Layer<'_> {
        Layer::new(self.id, LayerKind::Overlay, &self.tree, &self.laid).identity(&self.identity)
    }

    /// Draws it over `list`, with the focus ring on `focus`.
    pub(crate) fn draw(&self, list: &mut DisplayList, focus: Option<&str>) {
        let state = match focus.and_then(|key| self.tree.by_key(key)) {
            Some(node) => SceneState::new().with_focus(node),
            None => SceneState::new(),
        };
        // The panel has no images, so no image ids can clash.
        list.ops.extend(build(&self.tree, &self.laid, &state).ops);
    }

    /// The tree, for tests.
    #[cfg(test)]
    pub(crate) fn tree(&self) -> &Tree {
        &self.tree
    }

    /// The keys its buttons select.
    #[cfg(test)]
    pub(crate) fn picks(&self) -> &[String] {
        &self.picks
    }

    /// The middle of the button that selects `key`.
    #[cfg(test)]
    pub(crate) fn button(&self, key: &str) -> Option<(f64, f64)> {
        let at = self.picks.iter().position(|k| k == key)?;
        let node = self.tree.by_key(&format!("{ITEM}{at}"))?;
        let r = self.laid.rect(node)?;
        Some(((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0))
    }
}

fn line(
    builder: &mut Builder,
    parent: NodeId,
    key: &str,
    text: &str,
    color: Color,
) -> Result<(), PanelError> {
    let mut node = Node::new(Kind::Text {
        text: text.to_owned(),
        style: TextStyle {
            family: FAMILY.into(),
            size: SIZE,
            color,
            max_lines: 1,
            ..TextStyle::default()
        },
    });
    node.layout.height = Dim::Px(LINE);
    node.layout.shrink = 0.0;
    node.a11y.role = Role::Label;
    builder.add(parent, key, node)?;
    Ok(())
}

/// `text` on one line of at most `columns` characters.
fn clip(text: &str, columns: usize) -> String {
    let mut chars = text.chars().map(|c| if c.is_control() { ' ' } else { c });
    let mut out: String = chars.by_ref().take(columns).collect();
    if chars.next().is_some() {
        out.pop();
        out.push('…');
    }
    out
}

/// Cuts `text` into lines of `columns` characters, the ones after the first indented.
fn wrap(items: &mut Vec<Item>, text: &str, columns: usize, style: fn(String) -> Item) {
    let chars: Vec<char> = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let indent = text.chars().take_while(|&c| c == ' ').count() + 2;
    let indent = indent.min(columns / 2);
    let (first, mut rest) = chars.split_at(chars.len().min(columns));
    items.push(style(first.iter().collect()));
    while !rest.is_empty() {
        let (next, after) = rest.split_at(rest.len().min(columns - indent));
        let next: String = next.iter().collect();
        items.push(style(format!("{:indent$}{next}", "")));
        rest = after;
    }
}

/// The last part of a node's key, and its `id=`.
fn label(tree: &Tree, node: NodeId) -> String {
    let key = tree.key(node).unwrap_or_default();
    let last = key.rsplit('/').next().unwrap_or(key);
    match tree.get(node).and_then(|n| n.name.as_deref()) {
        Some(name) => format!("{last}  #{name}"),
        None => last.to_owned(),
    }
}

/// What is said of `node`.
fn items(screen: &Screen, node: NodeId, columns: usize) -> Vec<Item> {
    let tree = &screen.tree;
    let mut out = Vec::new();
    wrap(&mut out, &label(tree, node), columns, Item::Head);
    if let Some(key) = tree.key(node) {
        wrap(&mut out, key, columns, Item::Dim);
    }
    if let Some(placed) = screen.laid.get(node) {
        let r = placed.rect;
        let size = format!(
            "box {:.0},{:.0}  {:.0} × {:.0}",
            r.x0,
            r.y0,
            r.x1 - r.x0,
            r.y1 - r.y0
        );
        wrap(&mut out, &size, columns, Item::Line);
        if placed.clip.intersect(&r) != r {
            wrap(&mut out, "partly or wholly clipped", columns, Item::Line);
        }
    }
    if let Some(provenance) = &screen.provenance {
        origin_items(&mut out, tree, provenance, node, columns);
    }
    let mut above = tree.parent(node);
    if above.is_some() {
        out.push(Item::Head("PARENTS".to_owned()));
    }
    while let Some(parent) = above {
        push_pick(&mut out, tree, parent);
        above = tree.parent(parent);
    }
    if !tree.children(node).is_empty() {
        out.push(Item::Head("CHILDREN".to_owned()));
    }
    for &child in tree.children(node) {
        push_pick(&mut out, tree, child);
    }
    out
}

fn push_pick(out: &mut Vec<Item>, tree: &Tree, node: NodeId) {
    if let Some(key) = tree.key(node) {
        out.push(Item::Pick {
            label: label(tree, node),
            key: key.to_owned(),
        });
    }
}

/// What the bind recorded of `node`, or of the element that made it.
fn origin_items(
    out: &mut Vec<Item>,
    tree: &Tree,
    provenance: &Provenance,
    node: NodeId,
    columns: usize,
) {
    let mut at = Some(node);
    let origin = loop {
        let Some(n) = at else { break None };
        if let Some(origin) = provenance.get(n) {
            break Some(origin);
        }
        at = tree.parent(n);
    };
    if let Some(origin) = origin {
        if at != Some(node) {
            wrap(out, "made by the element around it:", columns, Item::Dim);
        }
        element_items(out, origin, columns);
    }
    let left = provenance.left_out(node);
    if !left.is_empty() {
        out.push(Item::Head("LEFT OUT".to_owned()));
    }
    for l in left {
        let head = format!("{}  {}", l.element, l.location);
        wrap(out, &head, columns, Item::Line);
        wrap(out, &format!("  {}", l.why), columns, Item::Dim);
    }
}

fn element_items(out: &mut Vec<Item>, origin: &Origin, columns: usize) {
    let head = format!("{}  {}", origin.element, origin.location);
    wrap(out, &head, columns, Item::Line);
    if origin.default_skin {
        wrap(out, "from the default skin", columns, Item::Dim);
    }
    for within in &origin.within {
        wrap(out, &format!("in {within}"), columns, Item::Dim);
    }
    if !origin.styles.is_empty() {
        let styles = format!("style {}", origin.styles.join(" "));
        wrap(out, &styles, columns, Item::Line);
    }
    if !origin.bindings.is_empty() {
        out.push(Item::Head("BINDINGS".to_owned()));
    }
    for b in &origin.bindings {
        wrap(out, &format!("{} {}", b.name, b.source), columns, Item::Dim);
        wrap(out, &format!("  → {}", b.value), columns, Item::Line);
    }
    if !origin.locals.is_empty() {
        out.push(Item::Head("NAMES IN SCOPE".to_owned()));
    }
    for (name, value) in &origin.locals {
        wrap(out, &format!("{name} = {value}"), columns, Item::Line);
    }
    if let Some(action) = &origin.action {
        out.push(Item::Head("ACTION".to_owned()));
        wrap(out, action.name, columns, Item::Line);
        for (name, value) in &action.args {
            wrap(out, &format!("  {name} = {value}"), columns, Item::Line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_lines_are_wrapped_and_nothing_is_lost() {
        let mut items = Vec::new();
        wrap(&mut items, "  → abcdefghij\nklmnop", 10, Item::Line);
        let lines: Vec<&str> = items
            .iter()
            .map(|i| match i {
                Item::Line(l) => l.as_str(),
                _ => panic!("{i:?}"),
            })
            .collect();
        assert!(lines.iter().all(|l| l.chars().count() <= 10), "{lines:?}");
        let joined: String = lines.iter().map(|l| l.trim_start()).collect();
        assert_eq!(joined.replace(' ', ""), "→abcdefghijklmnop");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abcd", 4), "abcd");
    }

    #[test]
    fn the_panel_takes_at_most_half_the_window() {
        assert_eq!(width(1280.0), 360.0);
        assert_eq!(width(640.0), 320.0);
    }
}

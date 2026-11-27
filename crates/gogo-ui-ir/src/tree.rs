//! The tree: an arena of nodes with unique keys (design 24 §2).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{A11y, Color, Effect, Fill, GradientStop, LayoutStyle, Paint, TextStyle, Track};

/// An opaque id for "activating this node does something".
///
/// The engine never looks inside: it reports the id of the node that was activated, and
/// the binder that built the tree maps it back to a resolved action and its arguments
/// (design 24 §2.1, rule 6).
///
/// An id is valid for one tree only. The binder owns the side table that maps it to the
/// action spec and its arguments (design 26 §4, §5); the table and the tree are built
/// together and dropped together, so an id from an older tree must never be looked up
/// in a newer table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ActionRef(u32);

impl ActionRef {
    /// An id with this index. Only the binder that owns the side table makes these.
    pub const fn new(index: u32) -> Self {
        ActionRef(index)
    }

    /// The index the binder gave this id.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// [`ActionRef::get`] as a `usize`, for indexing a side table.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// An opaque id for an app-drawn widget, assigned by the binder. The engine treats
/// two nodes with the same id as parts of one widget and nothing more. Valid for one tree
/// only; the binder owns the table behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct WidgetId(u32);

impl WidgetId {
    /// An id with this index.
    pub const fn new(index: u32) -> Self {
        WidgetId(index)
    }

    /// The index the binder gave this id.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// An opaque id for one named part of an app-drawn widget, assigned by the binder. Valid
/// for one tree only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PartId(u32);

impl PartId {
    /// An id with this index.
    pub const fn new(index: u32) -> Self {
        PartId(index)
    }

    /// The index the binder gave this id.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// An app widget's part as it appears in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AppPart {
    /// The widget this node belongs to.
    pub widget: WidgetId,
    /// The part of the widget this node is. `None`: the widget's root node.
    pub part: Option<PartId>,
}

/// Who drew a node: the skin, or an app widget the skin only styled (design 24 §2,
/// design 27 §5.5). The part's look is in the node's own [`LayoutStyle`] and [`Paint`],
/// already merged from the skin's part styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum DrawnBy {
    /// Drawn from the skin's own elements. The default.
    #[default]
    Skin,
    /// Drawn from an app widget's template. App nodes are appended after the skin's
    /// tree so they stay on top (design 28 §1).
    App(AppPart),
}

/// How an image fills its box (design 27 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Fit {
    /// Scale to cover the box, cropping the overflow. The default.
    #[default]
    Cover,
    /// Scale to fit inside the box, keeping the ratio.
    Contain,
    /// Stretch to the box.
    Fill,
}

/// Where an image's pixels come from. Never a URL or a file system path.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum ImageSource {
    /// A handle into the image store the app fills (design 25 §7).
    Handle(Arc<str>),
    /// A file inside the skin package, by its package-relative path.
    Asset(Arc<str>),
}

/// What a node is.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Kind {
    /// A container. The default; the only kind that may have children.
    Box,
    /// One text run, a leaf.
    Text {
        /// The text to draw.
        text: String,
        /// How to draw it.
        style: TextStyle,
    },
    /// An image, a leaf.
    Image {
        /// Where the pixels come from.
        source: ImageSource,
        /// How the image fills the box.
        fit: Fit,
    },
    /// A vector icon the engine ships, by name, a leaf. Drawn filled in
    /// [`Paint::fill`]'s colour when it is a solid fill. The scene crate owns the list
    /// of names; an unknown name draws nothing and is not an error.
    Icon(Arc<str>),
}

impl Kind {
    /// Whether a node of this kind may have children.
    pub fn is_container(&self) -> bool {
        matches!(self, Kind::Box)
    }
}

/// The width of a scrollbar, in logical px, when nothing says otherwise.
pub const SCROLLBAR_WIDTH: f32 = 8.0;

/// What makes a box a vertical scroll container (design 27 §5.1): its content is clipped
/// to the inside of its border, moves by an offset that lives outside the IR, and has a
/// gutter at the right edge, always reserved, where the scrollbar is drawn.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ScrollSpec {
    /// The gutter's width, 0 to 64. At 0 no scrollbar is drawn.
    pub width: f32,
    /// What the whole gutter is filled with. `None`: nothing.
    pub track: Option<Fill>,
    /// What the thumb is filled with.
    pub thumb: Fill,
    /// The thumb's corner radius.
    pub radius: f32,
    /// The container holds a route or is part of one, so it keeps an offset per route.
    /// Otherwise every route shares it. Default `false`.
    pub per_route: bool,
}

impl Default for ScrollSpec {
    fn default() -> Self {
        ScrollSpec {
            width: SCROLLBAR_WIDTH,
            track: None,
            thumb: Fill::Solid(Color([128, 128, 128, 140])),
            radius: SCROLLBAR_WIDTH / 2.0,
            per_route: false,
        }
    }
}

/// One resolved element. The tree owns the node's key, parent and children; see [`Tree`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Node {
    /// The skin's own `id=` name, for tests, the inspector and focus restore across
    /// re-binds. Not unique across repeats. `None`: unnamed.
    pub name: Option<Arc<str>>,
    /// What the node is.
    pub kind: Kind,
    /// How the node and its children are laid out.
    pub layout: LayoutStyle,
    /// How the node's box looks.
    pub paint: Paint,
    /// What [`Paint`] becomes while keyboard focus is on this node (`:focus`). `None`:
    /// the focus state changes nothing.
    pub focus_paint: Option<Paint>,
    /// What assistive technology is told.
    pub a11y: A11y,
    /// The node is a scroll container. Only a [`Kind::Box`] may be one. `None`: it isn't.
    pub scroll: Option<ScrollSpec>,
    /// What activating the node does. `None`: nothing.
    pub action: Option<ActionRef>,
    /// Who drew the node. Default [`DrawnBy::Skin`].
    pub drawn_by: DrawnBy,
    /// For the text run of a field being edited: its caret and selection. `None`: plain
    /// text.
    pub marks: Option<Box<crate::TextMarks>>,
    /// For a bar's fill: the fraction done, 0 to 1, as a share of its track. A live
    /// update finds the node by this and changes the value without a re-bind (design 27
    /// §5.6). `None`: not a progress fill.
    pub progress: Option<f32>,
}

impl Node {
    /// A node of `kind` with default layout, paint and accessibility.
    pub fn new(kind: Kind) -> Self {
        Node {
            name: None,
            kind,
            layout: LayoutStyle::default(),
            paint: Paint::default(),
            focus_paint: None,
            a11y: A11y::default(),
            scroll: None,
            action: None,
            drawn_by: DrawnBy::Skin,
            progress: None,
            marks: None,
        }
    }

    /// An empty container.
    pub fn container() -> Self {
        Self::new(Kind::Box)
    }

    /// A text run with a default style.
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(Kind::Text {
            text: text.into(),
            style: TextStyle::default(),
        })
    }

    /// Sets the skin name.
    pub fn with_name(mut self, name: impl Into<Arc<str>>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the layout style.
    pub fn with_layout(mut self, layout: LayoutStyle) -> Self {
        self.layout = layout;
        self
    }

    /// Sets the paint.
    pub fn with_paint(mut self, paint: Paint) -> Self {
        self.paint = paint;
        self
    }

    /// Sets the accessibility facts.
    pub fn with_a11y(mut self, a11y: A11y) -> Self {
        self.a11y = a11y;
        self
    }

    /// Sets the action.
    pub fn with_action(mut self, action: ActionRef) -> Self {
        self.action = Some(action);
        self
    }

    /// Sets who drew the node.
    pub fn drawn_by(mut self, drawn_by: DrawnBy) -> Self {
        self.drawn_by = drawn_by;
        self
    }
}

impl Default for Node {
    fn default() -> Self {
        Self::container()
    }
}

/// An index into a [`Tree`]. Valid only for the tree that made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NodeId(u32);

impl NodeId {
    /// The position in insertion order: the order nodes were added, root first (0). It
    /// is not document order, since a child can be added to an earlier node later.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Why a tree operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TreeError {
    /// The key is already used by another node.
    #[error("node key `{0}` is already used in this tree")]
    DuplicateKey(String),
    /// The id is not a node of this tree.
    #[error("no such node in this tree")]
    NoSuchNode,
    /// A text, image or icon was given children; carries the node's key.
    #[error("node `{0}` is a text, image or icon and cannot have children")]
    LeafParent(String),
    /// The tree would need more than `u32::MAX` nodes.
    #[error("the tree has too many nodes")]
    TooManyNodes,
    /// A value is outside its documented range ([`Tree::validate`]).
    #[error("node `{key}`: {field} is out of range")]
    OutOfRange {
        /// The node's key.
        key: String,
        /// What is out of range, in words.
        field: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct Slot {
    key: Arc<str>,
    node: Node,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

/// A resolved tree: one root, every node reachable from it, parents before children.
///
/// Each node has a **key**, a stable path id such as `root/column.0/row.1/button.2`
/// (`…/each.0#3` for a repeat) that the binder makes up. Keys are unique within a tree
/// and the same node gets the same key when the route is bound again, so focus and
/// scroll state can survive a re-bind.
///
/// Invariants, kept by construction: keys are unique; every node but the root has a
/// valid parent that was added before it; only [`Kind::Box`] nodes have children.
/// [`Tree::validate`] re-checks the last one after edits through [`Tree::node_mut`].
#[derive(Debug, Clone, PartialEq)]
pub struct Tree {
    slots: Vec<Slot>,
    by_key: HashMap<Arc<str>, NodeId>,
    stamp: Stamp,
}

/// Which build of a tree this is. It takes no part in comparing trees.
#[derive(Debug, Clone, Copy)]
struct Stamp(u64);

impl Stamp {
    fn next() -> Stamp {
        static LAST: AtomicU64 = AtomicU64::new(0);
        Stamp(LAST.fetch_add(1, Ordering::Relaxed))
    }
}

impl PartialEq for Stamp {
    fn eq(&self, _: &Stamp) -> bool {
        true
    }
}

impl Tree {
    /// A tree with only a root.
    pub fn new(root_key: impl Into<Arc<str>>, root: Node) -> Self {
        let key: Arc<str> = root_key.into();
        let mut by_key = HashMap::new();
        by_key.insert(Arc::clone(&key), NodeId(0));
        Tree {
            slots: vec![Slot {
                key,
                node: root,
                parent: None,
                children: Vec::new(),
            }],
            by_key,
            stamp: Stamp::next(),
        }
    }

    /// A number that is new for every tree built and after every change to one: two
    /// equal stamps mean the same content. A clone keeps it; comparing trees ignores it.
    pub fn stamp(&self) -> u64 {
        self.stamp.0
    }

    /// Adds `node` as the last child of `parent` and returns its id.
    ///
    /// # Errors
    /// [`TreeError::DuplicateKey`] if `key` is taken, [`TreeError::NoSuchNode`] if
    /// `parent` is not in this tree, [`TreeError::LeafParent`] if `parent` is a text,
    /// image or icon, [`TreeError::TooManyNodes`] past `u32::MAX` nodes.
    pub fn add_child(
        &mut self,
        parent: NodeId,
        key: impl Into<Arc<str>>,
        node: Node,
    ) -> Result<NodeId, TreeError> {
        let key: Arc<str> = key.into();
        let p = self
            .slots
            .get(parent.index())
            .ok_or(TreeError::NoSuchNode)?;
        if !p.node.kind.is_container() {
            return Err(TreeError::LeafParent(p.key.to_string()));
        }
        if self.by_key.contains_key(&key) {
            return Err(TreeError::DuplicateKey(key.to_string()));
        }
        let id = NodeId(u32::try_from(self.slots.len()).map_err(|_| TreeError::TooManyNodes)?);
        self.by_key.insert(Arc::clone(&key), id);
        self.slots.push(Slot {
            key,
            node,
            parent: Some(parent),
            children: Vec::new(),
        });
        self.slots[parent.index()].children.push(id);
        self.stamp = Stamp::next();
        Ok(id)
    }

    /// The root.
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// The number of nodes, at least 1.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Always `false`: a tree has a root.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The node, or `None` for an id from another tree.
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.slots.get(id.index()).map(|s| &s.node)
    }

    /// Edits a node in place. Do not turn a container with children into a leaf kind;
    /// [`Tree::validate`] reports it.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.stamp = Stamp::next();
        self.slots.get_mut(id.index()).map(|s| &mut s.node)
    }

    /// The node's key.
    pub fn key(&self, id: NodeId) -> Option<&str> {
        self.slots.get(id.index()).map(|s| &*s.key)
    }

    /// The node's parent; `None` for the root and for a missing node.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.slots.get(id.index()).and_then(|s| s.parent)
    }

    /// The children in order; empty for a missing node.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.slots
            .get(id.index())
            .map_or(&[], |s| s.children.as_slice())
    }

    /// The node with this key.
    pub fn by_key(&self, key: &str) -> Option<NodeId> {
        self.by_key.get(key).copied()
    }

    /// The first node, in document order, with this skin name.
    pub fn by_name(&self, name: &str) -> Option<NodeId> {
        self.iter().find(|&id| {
            self.get(id)
                .is_some_and(|n| n.name.as_deref() == Some(name))
        })
    }

    /// Every node in document order (pre-order: a parent, then its children in order).
    pub fn iter(&self) -> impl Iterator<Item = NodeId> + '_ {
        let mut stack = vec![self.root()];
        std::iter::from_fn(move || {
            let id = stack.pop()?;
            stack.extend(self.children(id).iter().rev());
            Some(id)
        })
    }

    /// Nodes with an action, in document order. Disabled and unfocusable nodes are
    /// included; see [`Tree::focus_order`] for the keyboard order.
    pub fn action_nodes(&self) -> Vec<NodeId> {
        self.iter()
            .filter(|&id| self.get(id).is_some_and(|n| n.action.is_some()))
            .collect()
    }

    /// The nodes the keyboard visits, in document order: [`A11y::focusable`] and not
    /// [`A11y::disabled`].
    pub fn focus_order(&self) -> Vec<NodeId> {
        self.iter()
            .filter(|&id| {
                self.get(id)
                    .is_some_and(|n| n.a11y.focusable && !n.a11y.disabled)
            })
            .collect()
    }

    /// Checks what [`Tree::node_mut`] and the public fields can break: only containers
    /// have children, and every value is inside its documented range (gradients have 2
    /// to 8 stops with offsets 0 to 1 in ascending order, a glow 1 to 4 layers, opacity,
    /// progress, noise amount and effect alphas are 0 to 1, a scanline period is at
    /// least 1, text size, weight, width and tracking are in their ranges, a column span
    /// is at least 1, a grid `fr` track is above 0). NaN and infinity are out of range.
    /// Returns the first offender in insertion order.
    ///
    /// # Errors
    /// [`TreeError::LeafParent`] or [`TreeError::OutOfRange`] naming the node's key.
    pub fn validate(&self) -> Result<(), TreeError> {
        for s in &self.slots {
            if !s.children.is_empty() && !s.node.kind.is_container() {
                return Err(TreeError::LeafParent(s.key.to_string()));
            }
            if let Err(field) = check_node(&s.node) {
                return Err(TreeError::OutOfRange {
                    key: s.key.to_string(),
                    field,
                });
            }
        }
        Ok(())
    }
}

fn unit(v: f32) -> bool {
    (0.0..=1.0).contains(&v)
}

fn check_node(n: &Node) -> Result<(), &'static str> {
    check_paint(&n.paint)?;
    if let Some(p) = &n.focus_paint {
        check_paint(p)?;
    }
    if n.progress.is_some_and(|p| !unit(p)) {
        return Err("progress");
    }
    if let Some(s) = &n.scroll {
        if !n.kind.is_container() {
            return Err("scroll on a leaf");
        }
        if !(0.0..=64.0).contains(&s.width) || !(s.radius.is_finite() && s.radius >= 0.0) {
            return Err("scrollbar width or radius");
        }
        for f in s.track.iter().chain([&s.thumb]) {
            if let Fill::Linear { stops, .. } | Fill::Radial { stops } = f {
                check_stops(stops)?;
            }
        }
    }
    if n.layout.col_span < 1 {
        return Err("column span");
    }
    if n.layout
        .columns
        .iter()
        .chain(&n.layout.rows)
        .any(|t| matches!(t, Track::Fr(f) if !(f.is_finite() && *f > 0.0)))
    {
        return Err("grid fr track");
    }
    if let Kind::Text { style, .. } = &n.kind {
        let ok = (1.0..=1024.0).contains(&style.size)
            && (1.0..=1000.0).contains(&style.weight)
            && (25.0..=200.0).contains(&style.width)
            && (-5.0..=5.0).contains(&style.tracking);
        if !ok {
            return Err("text size, weight, width or tracking");
        }
    }
    Ok(())
}

fn check_stops(stops: &[GradientStop]) -> Result<(), &'static str> {
    if !(2..=8).contains(&stops.len()) {
        return Err("gradient stop count");
    }
    if stops.iter().any(|s| !unit(s.offset)) || stops.windows(2).any(|w| w[0].offset > w[1].offset)
    {
        return Err("gradient stop offset");
    }
    Ok(())
}

fn check_paint(p: &Paint) -> Result<(), &'static str> {
    if !unit(p.opacity) {
        return Err("opacity");
    }
    if let Some(Fill::Linear { stops, .. } | Fill::Radial { stops }) = &p.fill {
        check_stops(stops)?;
    }
    for e in &p.effects {
        match e {
            Effect::Glow { layers } if !(1..=4).contains(&layers.len()) => {
                return Err("glow layer count");
            }
            Effect::Noise { amount, .. } if !unit(*amount) => return Err("noise amount"),
            Effect::Scanlines { period, alpha } => {
                if !(period.is_finite() && *period >= 1.0) {
                    return Err("scanline period");
                }
                if !unit(*alpha) {
                    return Err("scanline alpha");
                }
            }
            Effect::Reflection { alpha, .. } if !unit(*alpha) => return Err("reflection alpha"),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(feature = "serde")]
mod ser {
    use super::{NodeId, Tree};
    use serde::ser::{Serialize, SerializeStruct, Serializer};

    struct View<'a>(&'a Tree, NodeId);

    impl Serialize for View<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let (t, id) = (self.0, self.1);
            let mut st = s.serialize_struct("Node", 3)?;
            st.serialize_field("key", &t.key(id))?;
            st.serialize_field("node", &t.get(id))?;
            let kids: Vec<View<'_>> = t.children(id).iter().map(|&c| View(t, c)).collect();
            st.serialize_field("children", &kids)?;
            st.end()
        }
    }

    /// Serializes as nested nodes: `{key, node, children: [...]}`.
    impl Serialize for Tree {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            View(self, self.root()).serialize(s)
        }
    }
}

//! The tree: an arena of nodes with unique keys (design 24 §2).

use std::collections::HashMap;

use crate::{A11y, LayoutStyle, Paint, TextStyle};

/// An opaque id for "activating this node does something".
///
/// The engine never looks inside: it reports the id of the node that was activated, and
/// the binder that built the tree maps it back to a resolved action and its arguments
/// (design 24 §2.1, rule 6). Ids are only meaningful for the tree that carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ActionRef(pub u32);

/// An opaque id for an app-drawn widget, assigned by the binder. The engine treats
/// two nodes with the same id as parts of one widget and nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct WidgetId(pub u32);

/// An opaque id for one named part of an app-drawn widget, assigned by the binder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PartId(pub u32);

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
pub enum ImageSource {
    /// A handle into the image store the app fills (design 25 §7).
    Handle(String),
    /// A file inside the skin package, by its package-relative path.
    Asset(String),
}

/// What a node is.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Kind {
    /// A container. The default; the only kind that may have children.
    Box,
    /// One text run, a leaf.
    Text { text: String, style: TextStyle },
    /// An image, a leaf.
    Image { source: ImageSource, fit: Fit },
    /// A vector icon the engine ships, by name, a leaf. Drawn filled in
    /// [`Paint::fill`]'s colour when it is a solid fill.
    Icon(String),
}

impl Kind {
    /// Whether a node of this kind may have children.
    pub fn is_container(&self) -> bool {
        matches!(self, Kind::Box)
    }
}

/// One resolved element. The tree owns the node's key, parent and children; see [`Tree`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Node {
    /// The skin's own `id=` name, for tests, the inspector and focus restore across
    /// re-binds. Not unique across repeats. `None`: unnamed.
    pub name: Option<String>,
    pub kind: Kind,
    pub layout: LayoutStyle,
    pub paint: Paint,
    /// What [`Paint`] becomes while keyboard focus is on this node (`:focus`). `None`:
    /// the focus state changes nothing.
    pub focus_paint: Option<Paint>,
    pub a11y: A11y,
    /// What activating the node does. `None`: nothing.
    pub action: Option<ActionRef>,
    pub drawn_by: DrawnBy,
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
            action: None,
            drawn_by: DrawnBy::Skin,
            progress: None,
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

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_layout(mut self, layout: LayoutStyle) -> Self {
        self.layout = layout;
        self
    }

    pub fn with_paint(mut self, paint: Paint) -> Self {
        self.paint = paint;
        self
    }

    pub fn with_a11y(mut self, a11y: A11y) -> Self {
        self.a11y = a11y;
        self
    }

    pub fn with_action(mut self, action: ActionRef) -> Self {
        self.action = Some(action);
        self
    }

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
    /// The position in document order of creation (the root is 0).
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Why a tree operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    #[error("node key `{0}` is already used in this tree")]
    DuplicateKey(String),
    #[error("no such node in this tree")]
    NoSuchNode,
    #[error("node `{0}` is a text, image or icon and cannot have children")]
    LeafParent(String),
}

#[derive(Debug, Clone, PartialEq)]
struct Slot {
    key: String,
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
    by_key: HashMap<String, NodeId>,
}

impl Tree {
    /// A tree with only a root.
    pub fn new(root_key: impl Into<String>, root: Node) -> Self {
        let key = root_key.into();
        let mut by_key = HashMap::new();
        by_key.insert(key.clone(), NodeId(0));
        Tree {
            slots: vec![Slot {
                key,
                node: root,
                parent: None,
                children: Vec::new(),
            }],
            by_key,
        }
    }

    /// Adds `node` as the last child of `parent` and returns its id.
    ///
    /// # Errors
    /// [`TreeError::DuplicateKey`] if `key` is taken, [`TreeError::NoSuchNode`] if
    /// `parent` is not in this tree, [`TreeError::LeafParent`] if `parent` is a text,
    /// image or icon.
    pub fn add_child(
        &mut self,
        parent: NodeId,
        key: impl Into<String>,
        node: Node,
    ) -> Result<NodeId, TreeError> {
        let key = key.into();
        let p = self
            .slots
            .get(parent.index())
            .ok_or(TreeError::NoSuchNode)?;
        if !p.node.kind.is_container() {
            return Err(TreeError::LeafParent(p.key.clone()));
        }
        if self.by_key.contains_key(&key) {
            return Err(TreeError::DuplicateKey(key));
        }
        let id = NodeId(u32::try_from(self.slots.len()).map_err(|_| TreeError::NoSuchNode)?);
        self.by_key.insert(key.clone(), id);
        self.slots.push(Slot {
            key,
            node,
            parent: Some(parent),
            children: Vec::new(),
        });
        self.slots[parent.index()].children.push(id);
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

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.slots.get(id.index()).map(|s| &s.node)
    }

    /// Edits a node in place. Do not turn a container with children into a leaf kind;
    /// [`Tree::validate`] reports it.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.slots.get_mut(id.index()).map(|s| &mut s.node)
    }

    pub fn key(&self, id: NodeId) -> Option<&str> {
        self.slots.get(id.index()).map(|s| s.key.as_str())
    }

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

    /// Nodes with an action, in document (= focus) order.
    pub fn action_nodes(&self) -> Vec<NodeId> {
        self.iter()
            .filter(|&id| self.get(id).is_some_and(|n| n.action.is_some()))
            .collect()
    }

    /// Checks the invariants that [`Tree::node_mut`] can break: only containers have
    /// children. Returns the first offender.
    ///
    /// # Errors
    /// [`TreeError::LeafParent`] naming the node's key.
    pub fn validate(&self) -> Result<(), TreeError> {
        for s in &self.slots {
            if !s.children.is_empty() && !s.node.kind.is_container() {
                return Err(TreeError::LeafParent(s.key.clone()));
            }
        }
        Ok(())
    }
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

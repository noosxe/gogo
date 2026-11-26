//! What is on screen: a stack of laid-out trees.

use gogo_ui_ir::{DrawnBy, Node, NodeId, Role, Tree};
use gogo_ui_layout::Laid;

/// Names a layer across frames. Keep one id while a page or a dialog stays the same
/// screen, however often its tree is rebuilt; use a new one for another route and for
/// every new dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(pub u64);

/// What a control's action does, as far as the app has said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Identity {
    /// The app's value from [`Layer::identity`].
    Known(u64),
    /// The app gave none, so nothing outlives a rebuild: this is the tree's
    /// [stamp](Tree::stamp).
    Unknown(u64),
}

/// How a layer takes part in input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayerKind {
    /// The skin's page. Skin shortcuts work here.
    Page,
    /// Something drawn over the page that does not block it (a toast). The empty area of
    /// its root passes pointer input through. Above a dialog it is drawn but takes no
    /// input, and what it covers can't be clicked.
    Overlay,
    /// A modal dialog: while it is the topmost one, nothing else takes input.
    Dialog,
    /// A modal dialog about an error or a destructive step: as [`LayerKind::Dialog`],
    /// and announced as an alert.
    Alert,
}

impl LayerKind {
    /// Whether the layer blocks the layers below it.
    pub fn is_modal(self) -> bool {
        matches!(self, LayerKind::Dialog | LayerKind::Alert)
    }
}

/// One laid-out tree of the stack.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Layer<'a> {
    /// The screen this tree draws.
    pub id: LayerId,
    /// How it takes input.
    pub kind: LayerKind,
    /// The tree.
    pub tree: &'a Tree,
    /// Its layout, in logical px. Pointer coordinates are in the same space.
    pub laid: &'a Laid,
    /// What each action does, indexed by [`ActionRef::index`](gogo_ui_ir::ActionRef):
    /// a value (a hash of the call) that changes when the same control would now do
    /// something else. A press is dropped if it changed before the release. **Every
    /// action needs an entry** (debug builds assert it): a row of a repeat is keyed by
    /// its index, so without one a reordered list can't be seen. A missing entry fails
    /// closed: a press on that control does not outlive a rebuild of the tree.
    pub identity: &'a [u64],
    /// Keys of the press-and-hold controls.
    pub hold: &'a [&'a str],
    /// The key of the control a dialog starts on; `None` starts on nothing.
    pub initial_focus: Option<&'a str>,
}

impl<'a> Layer<'a> {
    /// A layer with no identities, no hold controls and no initial focus.
    pub fn new(id: LayerId, kind: LayerKind, tree: &'a Tree, laid: &'a Laid) -> Self {
        Layer {
            id,
            kind,
            tree,
            laid,
            identity: &[],
            hold: &[],
            initial_focus: None,
        }
    }

    /// Sets [`Layer::identity`].
    pub fn identity(mut self, identity: &'a [u64]) -> Self {
        self.identity = identity;
        self
    }

    /// Sets [`Layer::hold`].
    pub fn hold(mut self, hold: &'a [&'a str]) -> Self {
        self.hold = hold;
        self
    }

    /// Sets [`Layer::initial_focus`].
    pub fn initial_focus(mut self, key: &'a str) -> Self {
        self.initial_focus = Some(key);
        self
    }

    /// The identity of a node's action; `None` for a node without one.
    pub fn action_identity(&self, node: NodeId) -> Option<Identity> {
        let action = self.tree.get(node)?.action?;
        Some(match self.identity.get(action.index()) {
            Some(&id) => Identity::Known(id),
            None => Identity::Unknown(self.tree.stamp()),
        })
    }

    pub(crate) fn identifies_every_action(&self) -> bool {
        self.tree.iter().all(|id| {
            self.tree
                .get(id)
                .and_then(|n| n.action)
                .is_none_or(|a| a.index() < self.identity.len())
        })
    }

    /// The target for a node of this layer; `None` for an id of another tree.
    pub fn target(&self, node: NodeId) -> Option<Target> {
        Some(Target {
            layer: self.id,
            key: self.tree.key(node)?.to_owned(),
            node,
        })
    }
}

/// The screen: layers from the bottom up.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Bottom first: the page, then whatever is drawn over it.
    pub layers: &'a [Layer<'a>],
}

impl<'a> Frame<'a> {
    /// The layers that take input: the topmost modal one alone, or all of them when
    /// there is none. What is drawn over a dialog takes none.
    pub fn interactive(&self) -> &'a [Layer<'a>] {
        match self.layers.iter().rposition(|l| l.kind.is_modal()) {
            Some(i) => &self.layers[i..=i],
            None => self.layers,
        }
    }

    /// The layers drawn over the topmost modal one.
    pub(crate) fn over_modal(&self) -> &'a [Layer<'a>] {
        match self.layers.iter().rposition(|l| l.kind.is_modal()) {
            Some(i) => &self.layers[i + 1..],
            None => &[],
        }
    }

    /// The topmost modal layer.
    pub fn modal(&self) -> Option<&'a Layer<'a>> {
        self.layers.iter().rev().find(|l| l.kind.is_modal())
    }

    /// The interactive layer with this id.
    pub fn layer(&self, id: LayerId) -> Option<&'a Layer<'a>> {
        self.interactive().iter().find(|l| l.id == id)
    }
}

/// A node on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Its layer.
    pub layer: LayerId,
    /// Its tree key, stable across rebuilds of the layer.
    pub key: String,
    /// Its id in the tree of the frame it came from.
    pub node: NodeId,
}

impl Target {
    /// Whether both name the same node of the same screen.
    pub fn same(&self, other: &Target) -> bool {
        self.layer == other.layer && self.key == other.key
    }
}

/// Whether activating the node does something. A skin node needs an action; an app node
/// (a control the app drew) needs to be focusable.
pub(crate) fn activatable(node: &Node) -> bool {
    if node.a11y.disabled || node.a11y.role == Role::ComboBox {
        return false;
    }
    match node.drawn_by {
        DrawnBy::Skin => node.action.is_some(),
        DrawnBy::App(_) => node.a11y.focusable,
        _ => false,
    }
}

/// Whether the node is a control: something input can land on.
pub(crate) fn control(node: &Node) -> bool {
    !node.a11y.disabled && (node.a11y.focusable || activatable(node))
}

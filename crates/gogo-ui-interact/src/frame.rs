//! What is on screen: a stack of laid-out trees.

use gogo_ui_ir::{DrawnBy, Node, NodeId, Role, Tree};
use gogo_ui_layout::Laid;

/// Names a layer across frames. Keep one id while a page or a dialog stays the same
/// screen, however often its tree is rebuilt; use a new one for another route and for
/// every new dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(pub u64);

/// How a layer takes part in input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayerKind {
    /// The skin's page. Skin shortcuts work here.
    Page,
    /// Something drawn over the page that does not block it (a toast). The empty area of
    /// its root passes pointer input through.
    Overlay,
    /// A modal dialog: nothing below it takes input.
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
    /// something else. A press is dropped if it changed before the release. Without an
    /// entry the action's index stands in, which can't see a reordered list.
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
    /// The layers that take input: from the topmost modal one up, or all of them.
    pub fn interactive(&self) -> &'a [Layer<'a>] {
        let from = self
            .layers
            .iter()
            .rposition(|l| l.kind.is_modal())
            .unwrap_or(0);
        &self.layers[from..]
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

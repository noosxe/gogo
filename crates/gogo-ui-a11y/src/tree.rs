//! Building the tree, and mapping requests back.

use std::collections::HashMap;

use accesskit::{
    Action, ActionData, ActionRequest, Affine, HasPopup, Live, Node, NodeId, Rect, Role,
    ScrollUnit, TextDirection, TextPosition, TextSelection, Toggled, TreeId, TreeInfo, TreeUpdate,
};
use gogo_ui_interact::{
    Frame, Identity, Interaction, Layer, LayerId, LayerKind, Request, RequestKind, Target,
    activatable,
};
use gogo_ui_ir::{Kind, NodeId as IrId, Role as IrRole};

/// The description of a hold control.
pub const HOLD_HINT: &str = "Press and hold to confirm";
/// The description of a hold control activated once by a screen reader.
pub const HOLD_ARMED_HINT: &str = "Activate again to start holding";
/// The description of a hold control being held.
pub const HOLDING_HINT: &str = "Holding to confirm. Press Escape to cancel";

const ROOT: NodeId = NodeId(0);

/// What an id stands for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Name {
    layer: LayerId,
    key: String,
    identity: Option<Identity>,
}

/// Builds AccessKit updates and remembers which id is which node.
#[derive(Debug)]
pub struct A11yTree {
    window: String,
    ids: HashMap<Name, NodeId>,
    names: HashMap<NodeId, Name>,
    runs: HashMap<NodeId, Run>,
    last: u64,
}

/// The most characters in one text run: AccessKit counts them in a byte.
const RUN: usize = 255;

/// A stretch of a text field's value.
#[derive(Debug)]
struct Run {
    /// The field.
    field: NodeId,
    /// The byte offset in the value before each character, and after the last.
    offsets: Vec<usize>,
}

/// One pass over a frame.
struct Build<'a> {
    old: &'a HashMap<Name, NodeId>,
    ids: HashMap<Name, NodeId>,
    nodes: Vec<(NodeId, Node)>,
    runs: HashMap<NodeId, Run>,
    last: u64,
    input: &'a Interaction,
}

fn role_of(node: &gogo_ui_ir::Node) -> Option<Role> {
    Some(match node.a11y.role {
        IrRole::Group => Role::Group,
        IrRole::Region => Role::Region,
        IrRole::Heading => Role::Heading,
        IrRole::Label => Role::Label,
        IrRole::Image => Role::Image,
        IrRole::Button | IrRole::MenuButton => Role::Button,
        IrRole::Switch => Role::Switch,
        IrRole::ComboBox => Role::ComboBox,
        IrRole::TabList => Role::TabList,
        IrRole::Tab => Role::Tab,
        IrRole::Table => Role::Table,
        IrRole::Row => Role::Row,
        IrRole::Cell => Role::Cell,
        IrRole::Status => Role::Status,
        IrRole::ProgressBar => Role::ProgressIndicator,
        IrRole::TextInput => Role::TextInput,
        IrRole::List => Role::List,
        IrRole::ListItem => Role::ListItem,
        IrRole::Menu => Role::Menu,
        IrRole::MenuItem => Role::MenuItem,
        IrRole::ListBox => Role::ListBox,
        IrRole::ListOption => Role::ListBoxOption,
        // A generic node matters when it can be operated or was named.
        _ if node.a11y.focusable || activatable(node) => Role::Button,
        _ if matches!(node.kind, Kind::Text { .. }) => Role::Label,
        _ if node.a11y.name.is_some() => Role::Group,
        _ => return None,
    })
}

/// Whether the node is read as one thing.
fn whole(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::Switch
            | Role::ComboBox
            | Role::Tab
            | Role::Image
            | Role::Heading
            | Role::Label
            | Role::Status
            | Role::ProgressIndicator
            | Role::TextInput
            | Role::MenuItem
            | Role::ListBoxOption
    )
}

/// Whether something under `id` can be operated, so it must stay its own node.
///
/// This walks the subtree of every node that could be read as one, so a whole update is
/// O(nodes × depth) at worst. The skin limits (design 27 §8) bound both.
fn holds_controls(layer: &Layer<'_>, id: IrId) -> bool {
    layer.tree.children(id).iter().any(|&child| {
        layer
            .tree
            .get(child)
            .is_some_and(|n| n.a11y.focusable || activatable(n))
            || holds_controls(layer, child)
    })
}

/// All the text at and under `id`, in order.
fn text_of(layer: &Layer<'_>, id: IrId, out: &mut Vec<String>) {
    let Some(node) = layer.tree.get(id) else {
        return;
    };
    if let Some(name) = &node.a11y.name {
        out.push(name.clone());
        return;
    }
    if let Kind::Text { text, .. } = &node.kind {
        out.push(text.clone());
    }
    for &child in layer.tree.children(id) {
        text_of(layer, child, out);
    }
}

impl Build<'_> {
    fn id(&mut self, name: Name) -> NodeId {
        let id = self.old.get(&name).copied().unwrap_or_else(|| {
            self.last += 1;
            NodeId(self.last)
        });
        self.ids.insert(name, id);
        id
    }

    /// The text runs of a field's value, and its selection in them. Returns the runs'
    /// ids.
    fn text_runs(
        &mut self,
        layer: &Layer<'_>,
        key: &str,
        field: (NodeId, &mut Node),
        value: &str,
        (anchor, focus): (usize, usize),
    ) -> Vec<NodeId> {
        let (field, out) = field;
        let chars: Vec<(usize, char)> = value.char_indices().collect();
        // An empty value still has a run, for the caret to be in.
        let count = chars.len().div_ceil(RUN).max(1);
        let mut runs = Vec::with_capacity(count);
        let mut space = true;
        for n in 0..count {
            let part = &chars[(n * RUN).min(chars.len())..((n + 1) * RUN).min(chars.len())];
            let start = part.first().map_or(value.len(), |&(at, _)| at);
            let end = part.last().map_or(start, |&(at, c)| at + c.len_utf8());
            let mut node = Node::new(Role::TextRun);
            node.set_value(value.get(start..end).unwrap_or_default());
            node.set_character_lengths(
                part.iter()
                    .map(|&(_, c)| c.len_utf8() as u8)
                    .collect::<Vec<_>>(),
            );
            let mut words = Vec::new();
            for (i, &(_, c)) in part.iter().enumerate() {
                if space && !c.is_whitespace() {
                    words.push(i as u8);
                }
                space = c.is_whitespace();
            }
            node.set_word_starts(words);
            node.set_text_direction(TextDirection::LeftToRight);
            if let Some(bounds) = out.bounds() {
                node.set_bounds(bounds);
            }
            // A tree key holds no NUL, so this can't be one.
            let id = self.id(Name {
                layer: layer.id,
                key: format!("{key}\0{n}"),
                identity: None,
            });
            let offsets = part.iter().map(|&(at, _)| at).chain([end]).collect();
            self.runs.insert(id, Run { field, offsets });
            self.nodes.push((id, node));
            runs.push(id);
        }
        let position = |at: usize| {
            // At a seam the later run has it; at the very end, the last.
            let found = runs.iter().rev().find_map(|id| {
                let offsets = &self.runs.get(id)?.offsets;
                (*offsets.first()? <= at).then(|| TextPosition {
                    node: *id,
                    character_index: offsets.partition_point(|&o| o < at).min(offsets.len() - 1),
                })
            });
            found.unwrap_or(TextPosition {
                node: runs[0],
                character_index: 0,
            })
        };
        out.set_text_selection(TextSelection {
            anchor: position(anchor),
            focus: position(focus),
        });
        runs
    }

    /// Adds the nodes at and under `at`; returns those that become children of the
    /// caller.
    /// `scrolled` is whether a scroll container is above `at`.
    fn walk(&mut self, layer: &Layer<'_>, at: IrId, scrolled: bool) -> Vec<NodeId> {
        let (Some(node), Some(target)) = (layer.tree.get(at), layer.target(at)) else {
            return Vec::new();
        };
        let is_root = at == layer.tree.root();
        // A popup that takes text is read as a modal dialog; a menu or list popup is
        // read as just that.
        let popup_dialog = layer.kind == LayerKind::Popup && layer.submit.is_some();
        let modal = is_root && (layer.kind.is_modal() || popup_dialog);
        let role = if modal {
            Some(if layer.kind == LayerKind::Alert {
                Role::AlertDialog
            } else {
                Role::Dialog
            })
        } else if node.scroll.is_some() {
            Some(Role::ScrollView)
        } else {
            let group = is_root && layer.kind != LayerKind::Popup;
            role_of(node).or(group.then_some(Role::Group))
        };
        let inside = scrolled || node.scroll.is_some();
        let own_text = match &node.kind {
            Kind::Text { text, .. } => Some(text.clone()),
            _ => None,
        };
        let leaf = role.is_some_and(whole) && !holds_controls(layer, at);
        let label = node.a11y.name.clone().or_else(|| {
            if leaf {
                let mut parts = Vec::new();
                text_of(layer, at, &mut parts);
                (!parts.is_empty()).then(|| parts.join(" "))
            } else {
                own_text
            }
        });
        // An unnamed picture or empty text is decoration.
        let decoration = matches!(role, Some(Role::Image | Role::Label))
            && label.as_deref().is_none_or(str::is_empty)
            && !node.a11y.focusable;
        let role = role.filter(|_| !decoration);

        let children: Vec<NodeId> = if leaf {
            Vec::new()
        } else {
            layer
                .tree
                .children(at)
                .iter()
                .flat_map(|&child| self.walk(layer, child, inside))
                .collect()
        };
        let Some(role) = role else {
            return children;
        };

        let id = self.id(Name {
            layer: layer.id,
            key: target.key.clone(),
            identity: layer.action_identity(at),
        });
        let mut out = Node::new(role);
        if let Some(rect) = layer.laid.rect(at) {
            out.set_bounds(Rect {
                x0: rect.x0,
                y0: rect.y0,
                x1: rect.x1,
                y1: rect.y1,
            });
        }
        if let Some(label) = label {
            // AccessKit reads static text from the value.
            if role == Role::Label {
                out.set_value(label);
            } else {
                out.set_label(label);
            }
        }
        if let Some(value) = &node.a11y.value {
            out.set_value(value.clone());
        }
        if node.a11y.disabled {
            out.set_disabled();
        }
        if let Some(checked) = node.a11y.checked {
            out.set_toggled(Toggled::from(checked));
        }
        if let Some(selected) = node.a11y.selected {
            out.set_selected(selected);
        }
        if let Some(key) = &node.a11y.key {
            out.set_keyboard_shortcut(key.clone());
        }
        match node.a11y.role {
            IrRole::MenuButton => out.set_has_popup(HasPopup::Menu),
            IrRole::ComboBox => out.set_has_popup(HasPopup::Listbox),
            _ => {}
        }
        if let Some(progress) = node.progress.filter(|_| role == Role::ProgressIndicator) {
            out.set_numeric_value(f64::from(progress) * 100.0);
            out.set_min_numeric_value(0.0);
            out.set_max_numeric_value(100.0);
        }
        if role == Role::Status {
            out.set_live(Live::Polite);
        }
        if modal {
            out.set_modal();
        }
        if let Some(shown) = layer.laid.scroll_box(at) {
            out.set_clips_children();
            out.set_scroll_y(shown.offset);
            out.set_scroll_y_min(0.0);
            out.set_scroll_y_max(shown.max);
            if shown.max > 0.0 {
                out.add_action(Action::ScrollUp);
                out.add_action(Action::ScrollDown);
                out.add_action(Action::SetScrollOffset);
            }
        }
        // A virtualised list says how long it is, though only a part of it is here. A
        // table's rows are in a body of their own.
        let list = node.list.as_ref().or_else(|| {
            let kids = layer.tree.children(at).iter();
            kids.filter_map(|&c| layer.tree.get(c)?.list.as_ref())
                .next()
        });
        if let Some(list) = list.filter(|_| matches!(role, Role::List | Role::Table)) {
            out.set_size_of_set(list.count as usize);
            if role == Role::Table {
                out.set_row_count(list.count as usize);
            }
        }
        if let Some(item) = node.item {
            out.set_position_in_set(item as usize);
            if role == Role::Row {
                out.set_row_index(item as usize);
            }
        }
        if scrolled {
            out.add_action(Action::ScrollIntoView);
        }
        if !node.a11y.disabled {
            if node.a11y.focusable {
                out.add_action(Action::Focus);
            }
            if activatable(node) {
                out.add_action(Action::Click);
            }
            if node.a11y.role == IrRole::TextInput {
                out.add_action(Action::SetValue);
            }
        }
        if layer.hold.contains(&target.key.as_str()) {
            let is = |t: Option<&Target>| t.is_some_and(|t| t.same(&target));
            let hint = if is(self.input.armed()) {
                HOLD_ARMED_HINT
            } else if is(self.input.holding()) {
                HOLDING_HINT
            } else {
                HOLD_HINT
            };
            out.set_description(hint);
            // The change of hint is announced.
            out.set_live(Live::Assertive);
        }
        let mut children = children;
        if let (IrRole::TextInput, Some(value), Some(selection)) =
            (node.a11y.role, &node.a11y.value, node.a11y.text_selection)
        {
            children = self.text_runs(layer, &target.key, (id, &mut out), value, selection);
            if !node.a11y.disabled {
                out.add_action(Action::SetTextSelection);
                out.add_action(Action::ReplaceSelectedText);
            }
        }
        out.set_children(children);
        self.nodes.push((id, out));
        vec![id]
    }
}

impl A11yTree {
    /// `window` is the name of the window, the root of the tree.
    pub fn new(window: impl Into<String>) -> Self {
        A11yTree {
            window: window.into(),
            ids: HashMap::new(),
            names: HashMap::new(),
            runs: HashMap::new(),
            last: ROOT.0,
        }
    }

    /// The whole tree of a frame, with focus following `input`. Call it after
    /// [`Interaction::handle`] or [`Interaction::sync`] on the same frame. `scale` is
    /// the number of physical px to a logical one.
    pub fn update(&mut self, frame: &Frame<'_>, input: &Interaction, scale: f64) -> TreeUpdate {
        let mut build = Build {
            old: &self.ids,
            ids: HashMap::new(),
            nodes: Vec::new(),
            runs: HashMap::new(),
            last: self.last,
            input,
        };
        let mut children = Vec::new();
        for layer in frame.interactive() {
            children.extend(build.walk(layer, layer.tree.root(), false));
        }

        let mut window = Node::new(Role::Window);
        window.set_label(self.window.clone());
        window.set_transform(Affine::scale(scale));
        if let Some(bottom) = frame.layers.first() {
            let (width, height) = bottom.laid.size();
            window.set_bounds(Rect {
                x0: 0.0,
                y0: 0.0,
                x1: width.into(),
                y1: height.into(),
            });
        }
        window.set_children(children);

        let Build {
            ids,
            mut nodes,
            runs,
            last,
            ..
        } = build;
        self.names = ids.iter().map(|(name, &id)| (id, name.clone())).collect();
        self.ids = ids;
        self.runs = runs;
        self.last = last;

        let of = |layer: LayerId, key: &str| {
            self.ids
                .iter()
                .find(|(name, _)| name.layer == layer && name.key == key)
                .map(|(_, &id)| id)
        };
        let focus = input
            .focus()
            .and_then(|t| of(t.layer, &t.key))
            .or_else(|| {
                let dialog = frame.modal()?;
                of(dialog.id, dialog.tree.key(dialog.tree.root())?)
            })
            .unwrap_or(ROOT);
        // A field over a list points at the row Enter would activate.
        let highlighted = frame.modal().and_then(|layer| {
            let focused = input.focus().filter(|t| t.layer == layer.id)?;
            let node = layer.tree.get(layer.tree.by_key(&focused.key)?)?;
            (node.a11y.role == IrRole::TextInput)
                .then(|| of(layer.id, layer.submit?))
                .flatten()
        });
        if let Some(row) = highlighted
            && let Some((_, field)) = nodes.iter_mut().find(|(id, _)| *id == focus)
        {
            field.set_active_descendant(row);
        }
        nodes.push((ROOT, window));
        TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(ROOT)),
            tree_id: TreeId::ROOT,
            focus,
        }
    }

    /// The input a request stands for; `None` for an action that is not honoured or a
    /// node that is no longer there.
    pub fn map_request(&self, request: &ActionRequest) -> Option<Request> {
        if request.target_tree != TreeId::ROOT {
            return None;
        }
        let name = self.names.get(&request.target_node)?;
        let kind = match (request.action, &request.data) {
            (Action::Click, _) => RequestKind::Activate,
            (Action::Focus, _) => RequestKind::Focus,
            (Action::ScrollIntoView, _) => RequestKind::Reveal,
            (Action::ScrollUp | Action::ScrollDown, data) => RequestKind::ScrollBy {
                pages: matches!(data, Some(ActionData::ScrollUnit(ScrollUnit::Page))),
                back: request.action == Action::ScrollUp,
            },
            (Action::SetScrollOffset, Some(ActionData::SetScrollOffset(to))) => {
                // `as` saturates, and a NaN is the top.
                RequestKind::ScrollTo(to.y.max(0.0).round() as u32)
            }
            (Action::SetValue, Some(ActionData::Value(value))) => {
                RequestKind::SetValue(value.to_string())
            }
            (Action::ReplaceSelectedText, Some(ActionData::Value(value))) => {
                RequestKind::ReplaceSelected(value.to_string())
            }
            (Action::SetTextSelection, Some(ActionData::SetTextSelection(selection))) => {
                let byte = |position: TextPosition| {
                    let run = self.runs.get(&position.node)?;
                    (run.field == request.target_node)
                        .then(|| run.offsets.get(position.character_index).copied())?
                };
                RequestKind::SetSelection {
                    anchor: byte(selection.anchor)?,
                    focus: byte(selection.focus)?,
                }
            }
            _ => return None,
        };
        Some(Request {
            layer: name.layer,
            key: name.key.clone(),
            identity: name.identity,
            kind,
        })
    }
}

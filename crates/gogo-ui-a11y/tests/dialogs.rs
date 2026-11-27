//! A screen reader can read and answer the app's dialogs, through the same numbered input
//! as everything else (S4, S19).

use std::time::Duration;

use accesskit_consumer::{NodeRef, Tree};
use gogo_contract::{Value, actions};
use gogo_gui_model::fake::FakeBackend;
use gogo_gui_model::{DWELL, HOLD, InputEvent, Invocation, Modal, Model, Outcome, Press};
use gogo_ui_a11y::accesskit::{Action, ActionData, ActionRequest, NodeId, Role, TreeId};
use gogo_ui_a11y::{A11yTree, HOLD_ARMED_HINT, HOLD_HINT, HOLDING_HINT};
use gogo_ui_interact::{
    Config, Frame, HOLD_ARM, Input, InputProvenance, Intent, Interaction, Layer, LayerId,
    LayerKind, RequestKind, TextEdit,
};
use gogo_ui_ir::{
    A11y, ActionRef, AppPart, Dim, Direction, DrawnBy, LayoutStyle, Node, PartId, Role as IrRole,
    Tree as IrTree, WidgetId,
};
use gogo_ui_layout::{Laid, Scroll, Viewport, layout};
use gogo_ui_text::FontSet;

const PAGE: LayerId = LayerId(1);
const DIALOG: LayerId = LayerId(2);

fn sized(w: f32, h: f32) -> LayoutStyle {
    let mut s = LayoutStyle::default();
    s.width = Dim::Px(w);
    s.height = Dim::Px(h);
    s
}

struct Screen {
    tree: IrTree,
    laid: Laid,
}

/// The identity of the one action these screens have.
const IDENTITY: &[u64] = &[1];

impl Screen {
    fn new(name: &str, nodes: Vec<(&str, Node)>) -> Screen {
        let mut style = sized(800.0, 600.0);
        style.direction = Direction::Column;
        let root = Node::container().with_layout(style).with_a11y(A11y {
            role: IrRole::Group,
            name: Some(name.to_owned()),
            ..A11y::default()
        });
        let mut tree = IrTree::new("root", root);
        let id = tree.root();
        for (key, node) in nodes {
            tree.add_child(id, key, node).expect("add");
        }
        let laid = layout(
            &tree,
            &mut FontSet::bundled(),
            Viewport::new(800.0, 600.0),
            &Scroll::none(),
        )
        .expect("layout");
        Screen { tree, laid }
    }

    fn layer(&self, id: LayerId, kind: LayerKind) -> Layer<'_> {
        Layer::new(id, kind, &self.tree, &self.laid).identity(IDENTITY)
    }
}

fn control(role: IrRole, name: &str) -> Node {
    Node::container()
        .with_layout(sized(200.0, 40.0))
        .with_a11y(A11y::control(role, name))
}

fn app(role: IrRole, name: &str, part: u32) -> Node {
    control(role, name).drawn_by(DrawnBy::App(AppPart {
        widget: WidgetId::new(0),
        part: Some(PartId::new(part)),
    }))
}

fn page() -> Screen {
    Screen::new(
        "Skin",
        vec![(
            "play",
            control(IrRole::Button, "Play").with_action(ActionRef::new(0)),
        )],
    )
}

fn dialog() -> Screen {
    Screen::new(
        "Remove the game?",
        vec![
            ("text", Node::text("Its files are deleted.")),
            ("title", app(IrRole::TextInput, "Game title", 0)),
            ("confirm", app(IrRole::Button, "Remove", 1)),
            ("cancel", app(IrRole::Button, "Cancel", 2)),
        ],
    )
}

/// A screen reader on top of the engine.
struct Reader {
    a11y: A11yTree,
    input: Interaction,
    now: Duration,
}

impl Reader {
    fn new() -> Reader {
        Reader {
            a11y: A11yTree::new("window"),
            input: Interaction::new(Config::default()),
            now: Duration::from_secs(1),
        }
    }

    fn read(&mut self, frame: &Frame<'_>) -> Tree {
        self.input.sync(frame);
        Tree::new(self.a11y.update(frame, &self.input, 1.0), true)
    }

    /// The id of the node a screen reader would call `name`.
    fn find(&mut self, frame: &Frame<'_>, name: &str) -> NodeId {
        let tree = self.read(frame);
        walk(tree.state().root())
            .into_iter()
            .find(|n| n.label().as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no `{name}`"))
            .locate()
            .0
    }

    fn request(
        &mut self,
        frame: &Frame<'_>,
        action: Action,
        node: NodeId,
        data: Option<ActionData>,
    ) -> Vec<Intent> {
        self.now += Duration::from_millis(10);
        let request = ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: node,
            data,
        };
        match self.a11y.map_request(&request) {
            Some(r) => {
                self.input
                    .handle(Input::Accessibility(r), self.now, frame)
                    .intents
            }
            None => Vec::new(),
        }
    }

    fn click(&mut self, frame: &Frame<'_>, name: &str) -> Vec<Intent> {
        let node = self.find(frame, name);
        self.request(frame, Action::Click, node, None)
    }
}

fn walk(node: NodeRef<'_>) -> Vec<NodeRef<'_>> {
    let mut out = vec![node];
    for child in node.children() {
        out.extend(walk(child));
    }
    out
}

fn names(tree: &Tree) -> Vec<String> {
    walk(tree.state().root())
        .into_iter()
        .filter_map(|n| n.label().or_else(|| n.value()))
        .collect()
}

#[test]
fn under_a_dialog_only_the_dialog_is_read() {
    let (p, d) = (page(), dialog());
    let layers = [
        p.layer(PAGE, LayerKind::Page),
        d.layer(DIALOG, LayerKind::Alert),
    ];
    let frame = Frame { layers: &layers };
    let tree = Reader::new().read(&frame);
    assert_eq!(
        names(&tree),
        [
            "window",
            "Remove the game?",
            "Its files are deleted.",
            "Game title",
            "Remove",
            "Cancel"
        ]
    );
    let focus = tree.state().focus().expect("focus");
    assert_eq!(focus.role(), Role::AlertDialog, "nothing is focused yet");
    assert!(focus.is_modal() && focus.is_dialog());
}

#[test]
fn s4_a_screen_reader_click_is_one_numbered_press() {
    let p = page();
    let layers = [p.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut r = Reader::new();
    let play = r.find(&frame, "Play");
    assert!(r.request(&frame, Action::Focus, play, None).is_empty());
    assert_eq!(r.input.focus().map(|t| t.key.as_str()), Some("play"));
    for _ in 0..2 {
        let out = r.request(&frame, Action::Click, play, None);
        let [Intent::Invoke { provenance, .. }] = &out[..] else {
            panic!("{out:?}");
        };
        assert_eq!(provenance.event(), r.input.latest());
    }
    assert_eq!(r.input.latest().get(), 2);
    for action in [Action::Blur, Action::Increment, Action::ScrollIntoView] {
        assert!(r.request(&frame, action, play, None).is_empty());
    }
    assert!(
        r.request(&frame, Action::Click, NodeId(999), None)
            .is_empty()
    );
}

#[test]
fn a_late_request_for_a_changed_control_reaches_nothing() {
    let p = page();
    let (one, other) = ([1], [2]);
    let before = [p.layer(PAGE, LayerKind::Page).identity(&one)];
    let after = [p.layer(PAGE, LayerKind::Page).identity(&other)];
    let (before, after) = (Frame { layers: &before }, Frame { layers: &after });
    let mut r = Reader::new();
    let old = r.find(&before, "Play");
    assert_eq!(r.find(&before, "Play"), old, "stable while unchanged");
    assert_ne!(r.find(&after, "Play"), old);
    assert!(r.request(&after, Action::Click, old, None).is_empty());
}

#[test]
fn a_request_for_the_page_under_a_dialog_is_refused() {
    let (p, d) = (page(), dialog());
    let alone = [p.layer(PAGE, LayerKind::Page)];
    let with = [
        p.layer(PAGE, LayerKind::Page),
        d.layer(DIALOG, LayerKind::Dialog),
    ];
    let (alone, with) = (Frame { layers: &alone }, Frame { layers: &with });
    let mut r = Reader::new();
    let play = r.find(&alone, "Play");
    // The dialog is up, but the reader has not had the new tree yet.
    assert!(r.request(&with, Action::Click, play, None).is_empty());
}

// --- with the app's safety flow ---

/// `gogo-gui`'s translation, for the intents these dialogs give.
fn answer(model: &mut Model<FakeBackend>, intents: &[Intent], typed: &str) -> Vec<Outcome> {
    let press = |p: &InputProvenance| Press {
        event: InputEvent(p.event().get()),
        at: p.at(),
        repeat: p.repeat(),
    };
    intents
        .iter()
        .map(|intent| {
            let ticket = model.modal().expect("a dialog").ticket();
            let strong = matches!(model.modal(), Some(Modal::Strong { .. }));
            match intent {
                Intent::App {
                    target, provenance, ..
                } => match target.key.as_str() {
                    "cancel" => {
                        model.cancel(ticket);
                        Outcome::Accepted
                    }
                    _ if strong => model.type_name(ticket, typed, press(provenance)),
                    _ => model.confirm(ticket, press(provenance)),
                },
                Intent::HoldStart { provenance, .. } => model.hold_start(ticket, press(provenance)),
                other => panic!("{other:?}"),
            }
        })
        .collect()
}

/// A skin's call, with a real number.
fn open(r: &mut Reader, model: &mut Model<FakeBackend>, call: &Invocation) {
    let p = page();
    let layers = [p.layer(PAGE, LayerKind::Page)];
    let out = r.click(&Frame { layers: &layers }, "Play");
    let [Intent::Invoke { provenance, .. }] = &out[..] else {
        panic!("{out:?}");
    };
    let event = InputEvent(provenance.event().get());
    assert_eq!(model.invoke(call, event), Outcome::Accepted);
    model.pump();
}

fn present(r: &mut Reader, model: &mut Model<FakeBackend>) {
    let ticket = model.modal().expect("a dialog").ticket();
    model.presented(ticket, InputEvent(r.input.latest().get()), r.now);
    r.now += DWELL;
}

fn remove() -> Invocation {
    Invocation::new(actions::action("game.remove").expect("action"))
        .arg("id", Value::str("1"))
        .arg("keep_override", Value::Bool(false))
}

#[test]
fn s19_a_screen_reader_cancels_a_dialog() {
    let d = dialog();
    let layers = [d.layer(DIALOG, LayerKind::Dialog)];
    let frame = Frame { layers: &layers };
    let mut r = Reader::new();
    let mut model = Model::new(FakeBackend::new(), || {});
    open(&mut r, &mut model, &remove());
    let out = r.click(&frame, "Cancel");
    answer(&mut model, &out, "");
    assert!(model.modal().is_none());
    assert!(model.backend_mut().runs.is_empty());
}

#[test]
fn a_screen_reader_confirms_a_card_and_types_a_name() {
    let d = dialog();
    let card = [d.layer(DIALOG, LayerKind::Dialog)];
    let card = Frame { layers: &card };
    // The next screen is a new layer.
    let strong = [d.layer(LayerId(3), LayerKind::Alert)];
    let strong = Frame { layers: &strong };
    let mut r = Reader::new();
    let mut model = Model::new(FakeBackend::new(), || {});
    open(&mut r, &mut model, &remove());
    present(&mut r, &mut model);

    let out = r.click(&card, "Remove");
    assert_eq!(answer(&mut model, &out, ""), [Outcome::Accepted]);
    model.pump();
    assert!(matches!(model.modal(), Some(Modal::Strong { .. })));
    present(&mut r, &mut model);

    let field = r.find(&strong, "Game title");
    let data = Some(ActionData::Value("Example Game".into()));
    let set = r.request(&strong, Action::SetValue, field, data);
    let [Intent::SetValue { value, .. }] = &set[..] else {
        panic!("{set:?}");
    };
    assert!(
        model.backend_mut().runs.is_empty(),
        "typing answers nothing"
    );
    let out = r.click(&strong, "Remove");
    assert_eq!(answer(&mut model, &out, value), [Outcome::Accepted]);
    model.pump();
    assert_eq!(model.backend_mut().runs.len(), 1);
}

#[test]
fn a_screen_reader_hears_the_hold_and_completes_it() {
    let d = dialog();
    let card = [d.layer(DIALOG, LayerKind::Dialog)];
    let card = Frame { layers: &card };
    let hold = [d.layer(LayerId(3), LayerKind::Alert).hold(&["confirm"])];
    let hold = Frame { layers: &hold };
    let mut r = Reader::new();
    let mut model = Model::new(FakeBackend::new(), || {});
    let prune = Invocation::new(actions::action("apps.prune_orphaned").expect("action"))
        .arg("keep_override", Value::Bool(false));
    open(&mut r, &mut model, &prune);
    present(&mut r, &mut model);
    let out = r.click(&card, "Remove");
    answer(&mut model, &out, "");
    model.pump();
    present(&mut r, &mut model);

    let hint = |r: &mut Reader| {
        let tree = r.read(&hold);
        walk(tree.state().root())
            .into_iter()
            .find(|n| n.label().as_deref() == Some("Remove"))
            .and_then(|n| n.description())
            .expect("a hint")
    };
    assert_eq!(hint(&mut r), HOLD_HINT);
    let first = r.click(&hold, "Remove");
    assert!(matches!(first[..], [Intent::HoldArmed { .. }]), "{first:?}");
    assert_eq!(hint(&mut r), HOLD_ARMED_HINT);
    r.now += HOLD_ARM;
    let second = r.click(&hold, "Remove");
    assert_eq!(answer(&mut model, &second, ""), [Outcome::Accepted]);
    assert_eq!(hint(&mut r), HOLDING_HINT);
    model.tick(r.now + HOLD);
    model.pump();
    assert_eq!(model.backend_mut().runs.len(), 1);
}

#[test]
fn a_late_click_is_dropped_before_the_tree_is_updated() {
    let p = page();
    let (one, other) = ([1], [2]);
    let before = [p.layer(PAGE, LayerKind::Page).identity(&one)];
    let after = [p.layer(PAGE, LayerKind::Page).identity(&other)];
    let (before, after) = (Frame { layers: &before }, Frame { layers: &after });
    let mut r = Reader::new();
    let play = r.find(&before, "Play");
    // The control is re-bound; the adapter has not had a new tree.
    r.input.sync(&after);
    assert!(r.request(&after, Action::Click, play, None).is_empty());
    assert_eq!(r.input.latest().get(), 0);
}

#[test]
fn what_is_drawn_over_a_dialog_is_not_read_or_clicked() {
    let (p, d) = (page(), dialog());
    let toast = Screen::new("Toast", vec![("undo", app(IrRole::Button, "Undo", 3))]);
    let alone = [toast.layer(LayerId(3), LayerKind::Overlay)];
    let with = [
        p.layer(PAGE, LayerKind::Page),
        d.layer(DIALOG, LayerKind::Dialog),
        toast.layer(LayerId(3), LayerKind::Overlay),
    ];
    let (alone, with) = (Frame { layers: &alone }, Frame { layers: &with });
    let mut r = Reader::new();
    let undo = r.find(&alone, "Undo");
    // Even with the id from before the dialog.
    assert!(r.request(&with, Action::Click, undo, None).is_empty());
    let read = names(&r.read(&with));
    assert!(
        !read.iter().any(|n| n == "Undo" || n == "Toast"),
        "{read:?}"
    );
    assert!(r.request(&with, Action::Click, undo, None).is_empty());
}

fn typed_field(value: &str, selection: (usize, usize)) -> Screen {
    let mut field = app(IrRole::TextInput, "Game title", 0).with_action(ActionRef::new(0));
    field.a11y.value = Some(value.to_owned());
    field.a11y.text_selection = Some(selection);
    Screen::new("Name it", vec![("title", field)])
}

#[test]
fn a_field_is_read_as_text_with_its_selection() {
    for (value, selection, selected) in [
        ("héllo wörld".to_owned(), (0, 6), "héllo"),
        (String::new(), (0, 0), ""),
        // Longer than one run can count, selected across the seam.
        ("ab ".repeat(200), (254, 257), " ab"),
        ("x".repeat(255), (255, 255), ""),
    ] {
        let screen = typed_field(&value, selection);
        let layers = [screen.layer(DIALOG, LayerKind::Alert)];
        let frame = Frame { layers: &layers };
        let mut r = Reader::new();
        let tree = r.read(&frame);
        let id = r.find(&frame, "Game title");
        let node = walk(tree.state().root())
            .into_iter()
            .find(|n| n.locate().0 == id)
            .expect("the field");
        assert!(node.is_text_input());
        assert!(node.supports_text_ranges());
        assert_eq!(node.value().as_deref(), Some(value.as_str()));
        assert_eq!(node.document_range().text(), value);
        let range = node.text_selection().expect("a selection");
        assert_eq!(range.text(), selected, "{selection:?}");
        let focus = node.text_selection_focus().expect("a caret");
        assert_eq!(
            focus.to_global_usv_index(),
            value[..selection.1].chars().count()
        );

        // What it sends back is the same selection in bytes.
        let data = Some(ActionData::SetTextSelection(
            *node.data().text_selection().expect("raw"),
        ));
        let request = ActionRequest {
            action: Action::SetTextSelection,
            target_tree: TreeId::ROOT,
            target_node: id,
            data,
        };
        let mapped = r.a11y.map_request(&request).expect("a request");
        assert_eq!(
            mapped.kind,
            RequestKind::SetSelection {
                anchor: selection.0,
                focus: selection.1
            }
        );
    }
}

#[test]
fn a_screen_reader_replaces_the_selected_text() {
    let screen = typed_field("abc", (0, 1));
    let layers = [screen.layer(DIALOG, LayerKind::Alert)];
    let frame = Frame { layers: &layers };
    let mut r = Reader::new();
    let field = r.find(&frame, "Game title");
    let data = Some(ActionData::Value("x".into()));
    let out = r.request(&frame, Action::ReplaceSelectedText, field, data);
    let [Intent::Edit { edit, .. }] = &out[..] else {
        panic!("{out:?}");
    };
    assert_eq!(*edit, TextEdit::Replace("x".into()));
}

//! Hand-built layers and a driver for them.

#![allow(dead_code)]

use std::time::Duration;

use gogo_ui_interact::{
    Config, Frame, Input, Intent, Interaction, Key, Layer, LayerId, LayerKind, Modifiers,
    PointerButton, Request, RequestKind, Response,
};
use gogo_ui_ir::{
    A11y, ActionRef, AppPart, Dim, Direction, DrawnBy, LayoutStyle, Node, PartId, Role, Tree,
    WidgetId,
};
use gogo_ui_layout::{Laid, Scroll, Viewport, layout};
use gogo_ui_text::FontSet;

pub const PAGE: LayerId = LayerId(1);
pub const DIALOG: LayerId = LayerId(2);

pub const NONE: Modifiers = Modifiers {
    ctrl: false,
    shift: false,
    alt: false,
    meta: false,
};
pub const CTRL: Modifiers = Modifiers { ctrl: true, ..NONE };
pub const SHIFT: Modifiers = Modifiers {
    shift: true,
    ..NONE
};

fn sized(w: f32, h: f32) -> LayoutStyle {
    let mut s = LayoutStyle::default();
    s.width = Dim::Px(w);
    s.height = Dim::Px(h);
    s
}

fn column() -> Node {
    let mut s = sized(800.0, 600.0);
    s.direction = Direction::Column;
    Node::container().with_layout(s)
}

/// A skin button running action `action`.
pub fn button(name: &str, action: u32) -> Node {
    Node::container()
        .with_layout(sized(100.0, 40.0))
        .with_a11y(A11y::control(Role::Button, name))
        .with_action(ActionRef::new(action))
}

/// A control the app drew: part `part` of widget 0.
pub fn app_button(name: &str, part: u32) -> Node {
    Node::container()
        .with_layout(sized(100.0, 40.0))
        .with_a11y(A11y::control(Role::Button, name))
        .drawn_by(DrawnBy::App(part_of(part)))
}

pub fn part_of(part: u32) -> AppPart {
    AppPart {
        widget: WidgetId::new(0),
        part: Some(PartId::new(part)),
    }
}

pub struct Screen {
    pub tree: Tree,
    pub laid: Laid,
}

impl Screen {
    /// The nodes in a column, each under its key.
    pub fn new(nodes: Vec<(&str, Node)>) -> Screen {
        let mut tree = Tree::new("root", column());
        let root = tree.root();
        for (key, node) in nodes {
            tree.add_child(root, key, node).expect("add");
        }
        Screen::of(tree)
    }

    pub fn of(tree: Tree) -> Screen {
        let laid = layout(
            &tree,
            &mut FontSet::bundled(),
            Viewport::new(800.0, 600.0),
            &Scroll::none(),
        )
        .expect("layout");
        Screen { tree, laid }
    }

    pub fn layer(&self, id: LayerId, kind: LayerKind) -> Layer<'_> {
        Layer::new(id, kind, &self.tree, &self.laid)
    }

    pub fn center(&self, key: &str) -> (f64, f64) {
        let r = self
            .laid
            .rect(self.tree.by_key(key).expect(key))
            .expect("placed");
        (r.x0 + r.width() / 2.0, r.y0 + r.height() / 2.0)
    }
}

/// An interaction with a clock that moves 10 ms per input.
pub struct Driver {
    pub ix: Interaction,
    pub now: Duration,
}

impl Driver {
    pub fn new() -> Driver {
        Driver {
            ix: Interaction::new(Config::default()),
            now: Duration::from_secs(1),
        }
    }

    pub fn send(&mut self, frame: &Frame<'_>, input: Input) -> Response {
        self.now += Duration::from_millis(10);
        self.ix.handle(input, self.now, frame)
    }

    fn intents(&mut self, frame: &Frame<'_>, inputs: Vec<Input>) -> Vec<Intent> {
        inputs
            .into_iter()
            .flat_map(|i| self.send(frame, i).intents)
            .collect()
    }

    pub fn down(&mut self, frame: &Frame<'_>, key: Key, mods: Modifiers) -> Vec<Intent> {
        self.intents(
            frame,
            vec![Input::KeyDown {
                key,
                mods,
                repeat: false,
            }],
        )
    }

    pub fn repeat(&mut self, frame: &Frame<'_>, key: Key) -> Vec<Intent> {
        self.intents(
            frame,
            vec![Input::KeyDown {
                key,
                mods: NONE,
                repeat: true,
            }],
        )
    }

    pub fn up(&mut self, frame: &Frame<'_>, key: Key) -> Vec<Intent> {
        self.intents(frame, vec![Input::KeyUp { key }])
    }

    /// Down and up.
    pub fn tap(&mut self, frame: &Frame<'_>, key: Key) -> Vec<Intent> {
        let mut out = self.down(frame, key, NONE);
        out.extend(self.up(frame, key));
        out
    }

    pub fn tab(&mut self, frame: &Frame<'_>) -> String {
        self.tap(frame, Key::Tab);
        self.focused()
    }

    pub fn back_tab(&mut self, frame: &Frame<'_>) -> String {
        self.down(frame, Key::Tab, SHIFT);
        self.up(frame, Key::Tab);
        self.focused()
    }

    pub fn press_at(&mut self, frame: &Frame<'_>, (x, y): (f64, f64)) -> Vec<Intent> {
        let button = PointerButton::Primary;
        self.intents(frame, vec![Input::PointerDown { button, x, y }])
    }

    pub fn release_at(&mut self, frame: &Frame<'_>, (x, y): (f64, f64)) -> Vec<Intent> {
        let button = PointerButton::Primary;
        self.intents(frame, vec![Input::PointerUp { button, x, y }])
    }

    pub fn click(&mut self, frame: &Frame<'_>, at: (f64, f64)) -> Vec<Intent> {
        let mut out = self.press_at(frame, at);
        out.extend(self.release_at(frame, at));
        out
    }

    pub fn assist(
        &mut self,
        frame: &Frame<'_>,
        layer: LayerId,
        key: &str,
        kind: RequestKind,
    ) -> Vec<Intent> {
        let request = Request {
            layer,
            key: key.to_owned(),
            kind,
        };
        self.intents(frame, vec![Input::Accessibility(request)])
    }

    pub fn focused(&self) -> String {
        self.ix.focus().map_or_else(String::new, |t| t.key.clone())
    }
}

/// A short name per intent, for comparing.
pub fn names(intents: &[Intent]) -> Vec<String> {
    intents
        .iter()
        .map(|i| match i {
            Intent::Invoke { target, .. } => format!("invoke {}", target.key),
            Intent::App { target, .. } => format!("app {}", target.key),
            Intent::HoldStart { target, .. } => format!("hold {}", target.key),
            Intent::HoldEnd { target } => format!("end {}", target.key),
            Intent::HoldArmed { target } => format!("armed {}", target.key),
            Intent::SetValue { target, value } => format!("set {} {value}", target.key),
            Intent::Dismiss { .. } => "dismiss".to_owned(),
            Intent::Enter { .. } => "enter".to_owned(),
            Intent::Palette { .. } => "palette".to_owned(),
            other => format!("{other:?}"),
        })
        .collect()
}

//! The command palette (design 25 §4, design 26 §5): Ctrl K, or `palette.open`.
//!
//! The app alone builds what it lists, from its own table here and the app's data. It
//! reads nothing of the skin, so a skin can open it and nothing else: it can't add,
//! hide, reorder or relabel an entry, nor make the palette run anything.
//!
//! Choosing an entry goes through the same [`Model::invoke`](gogo_gui_model::Model::invoke)
//! as a skin button, with that press: the same checks, plan card and confirmation. It
//! lists what the engine can run today, and nothing gated or destructive.

use gogo_contract::{Value, actions};
use gogo_gui_model::{Invocation, SourceKey};
use gogo_skin::expr::Scope as _;
use gogo_ui_interact::{LayerId, Step};
use gogo_ui_ir::{A11y, Align, Color, Dim, Direction, Kind, Node, Paint, Role, Sides, Stroke};
use gogo_ui_text::{EditBuffer, EditOp, FontSet};

use crate::data::Data;
use crate::dialog::{self, FieldView};
use crate::panel::PanelError;
use crate::popup::{
    Builder, CARD, CHOSEN, Choice, DIM, Drawn, EARLIER, EDGE, ITEM, LATER, PADDING, RING, ROW,
    TEXT, hash, row,
};
use crate::skins::DEFAULT;

/// The text field's key.
pub(crate) const FIELD: &str = "palette/field";
/// The field's text.
pub(crate) const FIELD_TEXT: &str = "palette/field/text";
/// What the field's Enter names when nothing matches: no node.
const NOTHING: &str = "palette/nothing";

const NAME: &str = "Command palette";
/// The most characters searched for.
const LONGEST: usize = 128;
/// The most rows shown at once.
const ROWS: usize = 10;
const WIDTH: f32 = 560.0;
/// Between the window's top and the card, as a part of its height.
const TOP: f32 = 0.12;

const SCRIM: Color = Color([0x00, 0x00, 0x00, 0x90]);
const FIELD_FILL: Color = Color([0x10, 0x12, 0x17, 0xff]);

/// The routes the palette can go to: every one that needs no parameter, but the frame.
const PLACES: &[(&str, &str)] = &[
    ("library", "Go to Library"),
    ("queue", "Go to Queue"),
    ("updates", "Go to Updates"),
    ("saves", "Go to Cloud saves"),
    ("apps", "Go to Installed apps"),
    ("runtimes", "Go to Runtimes and renderers"),
    ("doctor", "Go to Doctor"),
    ("settings", "Go to Settings"),
    ("about", "Go to About"),
    ("sign-in", "Go to Sign in"),
];

/// One entry.
#[derive(Debug, Clone)]
pub(crate) struct Command {
    pub(crate) label: String,
    /// An action of the contract, or one of the app's own.
    pub(crate) choice: Choice,
}

/// An action by its name in the contract, with its arguments.
fn command(label: impl Into<String>, name: &str, args: &[(&str, &str)]) -> Option<Command> {
    let mut invocation = Invocation::new(actions::action(name)?);
    for (name, value) in args {
        invocation = invocation.arg(name, Value::str(*value));
    }
    Some(Command {
        label: label.into(),
        choice: Choice::Command(invocation),
    })
}

fn text_of<'a>(record: &'a Value, field: &str) -> Option<&'a Value> {
    match record {
        Value::Map(map) => map.get(field),
        _ => None,
    }
}

/// What the palette lists now: what the current game can do, then what the app can, then
/// where it can go. Sources it has not read yet are asked for, and their entries come
/// with them.
pub(crate) fn commands(data: &mut Data) -> Vec<Command> {
    // Again for what a first read asked for and already has.
    let (out, asked) = read(data);
    if data.watch(asked) {
        return read(data).0;
    }
    out
}

/// One pass over the data, and the sources it did not have.
fn read(data: &Data) -> (Vec<Command>, Vec<SourceKey>) {
    let scope = data.scope();
    let field = |record: Option<&Value>, name: &str| {
        record
            .and_then(|r| text_of(r, name))
            .map(Value::display)
            .unwrap_or_default()
    };
    let os = field(scope.var("app"), "os");
    let signed_out =
        scope.var("account").and_then(|a| text_of(a, "signed_in")) == Some(&Value::Bool(false));
    let mut out = Vec::new();

    let location = data.location();
    let game = (location.route == "game")
        .then(|| location.params.get("id").map(Value::display))
        .flatten();
    if let Some(id) = game
        && let Some(Ok(record)) = scope.call("game", &[&Value::str(id.as_str())])
    {
        let title = field(Some(&record), "title");
        let id = [("id", id.as_str())];
        out.extend(command(
            format!("Download files of {title}"),
            "game.download",
            &id,
        ));
        // The engine builds and sets up on Linux only.
        if os == "linux" {
            if field(Some(&record), "status") == "not_installed" {
                out.extend(command(
                    format!("Install {title}"),
                    "game.build",
                    &[id[0], ("platform", "linux")],
                ));
            }
            out.extend(command(format!("Set up {title}"), "game.setup", &id));
        }
    }

    out.extend(command("Refresh library", "library.refresh", &[]));
    // The app's own: no skin has an action for it.
    if data.imports_skins() {
        out.push(Command {
            label: "Import skin\u{2026}".to_owned(),
            choice: Choice::ImportSkin,
        });
    }
    // Always here, whatever the skin on screen draws: the way out of one that is unusable.
    if data.skin().is_some_and(|skin| skin != DEFAULT) {
        out.extend(command(
            "Use the default skin",
            "settings.set",
            &[("key", "gui.skin"), ("value", DEFAULT)],
        ));
    }
    if signed_out {
        out.extend(command("Sign in", "account.sign_in", &[]));
    }
    out.extend(
        PLACES
            .iter()
            .filter_map(|(route, label)| command(*label, "nav.go", &[("route", route)])),
    );
    if let Some(Value::List(games)) = scope.var("library") {
        for record in games {
            let (id, title) = (field(Some(record), "id"), field(Some(record), "title"));
            if id.is_empty() || title.is_empty() {
                continue;
            }
            let go = command(format!("Go to {title}"), "nav.go", &[("route", "game")]);
            out.extend(go.map(|mut c| {
                if let Choice::Command(invocation) = &mut c.choice {
                    *invocation = invocation.clone().route_arg("id", Value::str(id));
                }
                c
            }));
        }
    }
    (out, scope.asked())
}

/// The entries whose label holds `query`, ignoring case: first those where it starts a
/// word, each group in the order they came.
fn matching(commands: Vec<Command>, query: &str) -> Vec<Command> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return commands;
    }
    let mut starts = Vec::new();
    let mut holds = Vec::new();
    for command in commands {
        let label = command.label.to_lowercase();
        let word = label.match_indices(&query).any(|(at, _)| {
            label[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric())
        });
        if word {
            starts.push(command);
        } else if label.contains(&query) {
            holds.push(command);
        }
    }
    starts.append(&mut holds);
    starts
}

/// The open palette.
#[derive(Debug)]
pub(crate) struct Open {
    /// The page layer it was first drawn over: another route closes it.
    page: Option<LayerId>,
    /// The search field.
    field: EditBuffer,
    /// How far the field's text had moved left.
    pub(crate) scroll: f32,
    /// The row Enter activates.
    highlight: usize,
    first: usize,
    /// How many entries the last build showed.
    rows: usize,
    /// Changes with the query and the highlight: a press made on an earlier one does
    /// nothing.
    revision: u64,
}

impl Open {
    pub(crate) fn new(fonts: &mut FontSet) -> Open {
        let style = dialog::field_style(dialog::FIELD_TEXT_COLOR);
        Open {
            page: None,
            field: EditBuffer::new(fonts, &style, "", LONGEST),
            scroll: 0.0,
            highlight: 0,
            first: 0,
            rows: 0,
            revision: 0,
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// An edit of the field. A change to what is searched for starts the list over.
    pub(crate) fn edit(&mut self, fonts: &mut FontSet, op: EditOp<'_>) {
        let before = self.field.text();
        self.field.apply(fonts, op);
        if self.field.text() != before {
            (self.highlight, self.first) = (0, 0);
            self.revision += 1;
        }
    }

    /// What is selected in the field.
    pub(crate) fn selected(&self) -> Option<&str> {
        self.field.selected()
    }

    /// Up or Down; the build keeps it inside the list.
    pub(crate) fn step(&mut self, step: Step) {
        self.highlight = match step {
            Step::Prev => self.highlight.saturating_sub(1),
            Step::Next => self.highlight + 1,
        };
        self.revision += 1;
    }

    /// The button at an end of the window was pressed.
    pub(crate) fn turn(&mut self, key: &str) {
        let page = self.rows.max(1);
        self.highlight = if key == EARLIER {
            self.first.saturating_sub(page)
        } else {
            self.first + page
        };
        self.revision += 1;
    }

    /// The palette over the page `page`, listing `commands`, for a window of `size`.
    /// `None` when the route changed under it: it closes.
    pub(crate) fn draw(
        &mut self,
        fonts: &mut FontSet,
        id: LayerId,
        page: Option<LayerId>,
        size: (f32, f32),
        commands: Vec<Command>,
        focused: bool,
    ) -> Result<Option<Drawn>, PanelError> {
        if self.page.is_some() && self.page != page {
            return Ok(None);
        }
        self.page = page;
        let found = matching(commands, &self.field.text());
        let total = found.len();
        // The card's top, the field, the buttons at both ends and the margins.
        let used = size.1 * TOP + 3.0 * ROW + 4.0 * PADDING;
        let rows = (((size.1 - used) / ROW).floor().max(1.0) as usize).min(ROWS);
        self.highlight = self.highlight.min(total.saturating_sub(1));
        if self.highlight < self.first {
            self.first = self.highlight;
        } else if self.highlight >= self.first + rows {
            self.first = self.highlight + 1 - rows;
        }
        self.first = self.first.min(total.saturating_sub(rows));
        self.rows = rows;

        let mut root = Node::new(Kind::Box);
        root.layout.direction = Direction::Column;
        root.layout.align_items = Some(Align::Center);
        root.layout.padding = Sides {
            top: size.1 * TOP,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        };
        root.paint = Paint::solid(SCRIM);
        root.a11y.name = Some(NAME.to_owned());
        let mut b = Builder::new(id, size, root);

        let mut card = Node::new(Kind::Box);
        card.layout.width = Dim::Px((size.0 - 32.0).clamp(0.0, WIDTH));
        card.layout.direction = Direction::Column;
        card.layout.row_gap = PADDING;
        card.layout.padding = Sides {
            top: PADDING,
            right: PADDING,
            bottom: PADDING,
            left: PADDING,
        };
        card.paint = Paint::solid(CARD);
        card.paint.radius = [10.0; 4];
        card.paint.border = [Some(Stroke {
            width: 1.0,
            color: EDGE,
        }); 4];
        let top = b.root();
        let card = b.add(top, "palette/card", card)?;

        let mut field = row(FIELD_FILL);
        field.paint.border = [Some(Stroke {
            width: 1.0,
            color: EDGE,
        }); 4];
        field.focus_paint = Some(Paint {
            ring: Some(Stroke {
                width: 2.0,
                color: RING,
            }),
            ..field.paint.clone()
        });
        let view = FieldView::of(&self.field, focused, self.scroll);
        field.a11y = A11y::control(Role::TextInput, "Search");
        field.a11y.value = Some(view.text.clone());
        field.a11y.text_selection = Some(view.ends);
        b.control(&mut field, hash(FIELD));
        let field = b.add(card, FIELD, field)?;
        let text = dialog::field_text_node(&view, "Search commands and games");
        b.add(field, FIELD_TEXT, text)?;

        let mut list = Node::new(Kind::Box);
        list.layout.direction = Direction::Column;
        list.a11y.role = Role::ListBox;
        list.a11y.name = Some("Commands".to_owned());
        let list = b.add(card, "palette/list", list)?;
        if self.first > 0 {
            b.end(list, EARLIER, "More above")?;
        }
        for (i, command) in found.iter().enumerate().skip(self.first).take(rows) {
            let key = format!("{ITEM}{i}");
            let highlighted = i == self.highlight;
            let mut node = row(if highlighted { CHOSEN } else { CARD });
            node.a11y = A11y::control(Role::ListOption, command.label.clone());
            node.a11y.selected = Some(highlighted);
            b.control(
                &mut node,
                hash((&command.label, format!("{:?}", command.choice))),
            );
            let id = b.add(list, &key, node)?;
            b.text(id, &format!("{key}/text"), &command.label, TEXT, 1.0)?;
        }
        if self.first + rows < total {
            b.end(list, LATER, "More below")?;
        }
        if total == 0 {
            let mut node = row(CARD);
            node.a11y.role = Role::Status;
            node.a11y.name = Some("Nothing matches".to_owned());
            let id = b.add(card, "palette/empty", node)?;
            b.text(id, "palette/empty/text", "Nothing matches", DIM, 1.0)?;
        }

        let submit = if total == 0 {
            NOTHING.to_owned()
        } else {
            format!("{ITEM}{}", self.highlight)
        };
        let choices = found.into_iter().map(|c| Some(c.choice)).collect();
        let focus = (Some(FIELD.to_owned()), Some(submit));
        b.finish(fonts, size, focus, choices, self.revision)
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use gogo_contract::actions::{Binding, Tier};
    use gogo_contract::routes::ROUTES;

    use super::*;

    fn labels(commands: &[Command]) -> Vec<&str> {
        commands.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn every_route_without_a_required_parameter_is_listed() {
        for route in ROUTES {
            let needs = route.params.iter().any(|p| !p.optional);
            let listed = PLACES.iter().any(|(name, _)| *name == route.name);
            assert_eq!(listed, !needs && route.name != "shell", "{}", route.name);
        }
    }

    #[test]
    fn nothing_gated_destructive_or_app_only_is_listed() {
        let mut data = Data::fixture(|| {}).expect("fixture data");
        assert_eq!(data.location().route, "game");
        let listed = commands(&mut data);
        let specs: Vec<_> = listed
            .iter()
            .filter_map(|c| match &c.choice {
                Choice::Command(invocation) => Some(invocation.spec),
                _ => None,
            })
            .collect();
        assert_eq!(
            specs.len(),
            listed.len(),
            "the app's own wait for a skin folder"
        );
        assert!(specs.iter().any(|s| s.name == "game.download"));
        for spec in specs {
            assert!(spec.tier < Tier::Gated, "{}", spec.name);
            assert!(spec.binding != Binding::AppOnly, "{}", spec.name);
            assert!(
                !["ui.set", "clipboard.copy", "palette.open", "env.reveal"].contains(&spec.name),
                "{}",
                spec.name
            );
        }
        assert!(labels(&listed).iter().any(|l| l.starts_with("Go to ")));
    }

    #[test]
    fn a_word_start_comes_before_a_match_inside_a_word() {
        let make = |label: &str| command(label, "library.refresh", &[]).expect("known");
        let all = vec![make("Undone"), make("Go to Doctor"), make("Queue")];
        assert_eq!(
            labels(&matching(all.clone(), "do")),
            ["Go to Doctor", "Undone"]
        );
        assert_eq!(labels(&matching(all.clone(), "  QUE ")), ["Queue"]);
        assert!(matching(all.clone(), "zzz").is_empty());
        assert_eq!(matching(all, "").len(), 3);
    }
}

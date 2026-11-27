//! The safety flow's screen in the window: what it shows, what of the user's input it
//! takes, and when the model hears that it was seen (design 25 §4).
//!
//! The rules, each the reason for a test:
//! - the model is told a screen was presented only once the frame holding all of it is
//!   on the display;
//! - an answer comes only from the screen that is on the display, with its ticket: a
//!   screen the model has that is not there yet takes nothing, Esc included;
//! - an error alert is closed only by a press that could answer a screen: it can appear
//!   between the two halves of a double Enter or Esc, and must be readable (S9);
//! - a hold counts only while interaction says it is down, up to a frame that was on the
//!   display.

use std::rc::Rc;
use std::time::Duration;

use gogo_gui_model::{
    DWELL, HOLD, Modal, Outcome, Press, PromptAnswer, PromptSpec, Secret, Side, Strength, Ticket,
};
use gogo_ui_interact::{InputProvenance, Intent, TextEdit};
use gogo_ui_scene::DisplayList;
use gogo_ui_shell::Zeroizing;
use gogo_ui_text::{EditBuffer, EditOp};

use super::{App, say};
use crate::act;
use crate::dialog::{self, Ask, Content, Dialog, Field, FieldView, Hold};
use crate::panel::Panel;

/// The longest title or one-line value the field takes.
const MAX_TEXT: usize = 4096;

/// What the user has put into the screen of one ticket. A secret stays here, in the
/// app's own memory, until it is sent or dropped (S1).
#[derive(Default)]
pub(super) struct Entry {
    pub(super) ticket: Option<Ticket>,
    pub(super) code: Option<Secret>,
    pub(super) field: Field,
    /// The text field of the screens that have one. A secret never goes in: the buffer
    /// keeps copies it can't wipe, and has an undo history.
    pub(super) buffer: Option<EditBuffer>,
    /// How far the field's text is moved left.
    pub(super) scroll: f32,
    page: usize,
    /// One per conflicting file.
    picks: Vec<Option<Side>>,
}

/// When the dialog on the display got there.
#[derive(Debug, Clone, Copy)]
pub(super) struct Shown {
    pub(super) ticket: Ticket,
    pub(super) at: Duration,
    /// The last input there had been.
    pub(super) after: u64,
    /// Whether all of it was there. An error too large for the window starts its wait
    /// all the same, so that it can be closed, and again once it all fits.
    pub(super) complete: bool,
}

impl Shown {
    /// Whether a press may act on the dialog, by the rule the model has for an answer:
    /// it went down after the dialog was up, not as a key repeating, and not before the
    /// dialog had been there long enough to be seen.
    pub(super) fn takes(self, ticket: Ticket, press: u64, at: Duration, repeat: bool) -> bool {
        self.ticket == ticket
            && !repeat
            && press > self.after
            && at.checked_sub(self.at).is_some_and(|up| up >= DWELL)
    }
}

/// A hold the model has accepted.
#[derive(Debug, Clone, Copy)]
pub(super) struct Holding {
    ticket: Ticket,
    since: Duration,
}

fn press(provenance: &InputProvenance) -> Press {
    Press {
        event: act::number(provenance.event()),
        at: provenance.at(),
        repeat: provenance.repeat(),
    }
}

impl Entry {
    /// What the field holds, without a composition.
    fn text(&self) -> String {
        self.buffer
            .as_ref()
            .map(EditBuffer::text)
            .unwrap_or_default()
    }

    fn field_view(&self, focused: bool) -> FieldView {
        let Some(buffer) = &self.buffer else {
            return FieldView::default();
        };
        FieldView {
            text: buffer.text(),
            shown: buffer.shown().to_owned(),
            selection: buffer.selected_range(),
            // A composition is not part of what assistive technology reads: the caret
            // is where it will go.
            ends: match buffer.preedit() {
                Some(preedit) => (preedit.start, preedit.start),
                None => buffer.selection(),
            },
            caret: buffer.caret().filter(|_| focused),
            preedit: buffer.preedit(),
            scroll: self.scroll,
        }
    }
}

/// The kind of a screen, for the run's output: never its content, which can hold the
/// user's data.
fn kind(modal: &Modal<'_>) -> &'static str {
    match modal {
        Modal::Prompt { .. } => "prompt",
        Modal::Planning { .. } => "planning",
        Modal::PlanCard { .. } => "plan card",
        Modal::Strong { .. } => "destructive confirmation",
        Modal::Error { .. } => "error",
    }
}

/// What the screen of `modal` shows, with what the user has put into it.
fn content(modal: &Modal<'_>, entry: &Entry, changed: bool, hold: Hold, focused: bool) -> Content {
    match modal {
        Modal::Planning { confirming, .. } => Content::Planning {
            confirming: *confirming,
        },
        Modal::PlanCard { plan, .. } => Content::Plan {
            plan: (*plan).clone(),
            changed,
            ask: Ask::Confirm,
        },
        Modal::Strong { plan, strength, .. } => Content::Plan {
            plan: (*plan).clone(),
            changed: false,
            ask: match strength {
                Strength::Typed(want) => Ask::Typed {
                    want: want.clone(),
                    typed: entry.field_view(focused),
                },
                Strength::Hold => Ask::Hold(hold),
            },
        },
        Modal::Prompt {
            spec, action, job, ..
        } => match spec {
            PromptSpec::Text { label, .. } => Content::Text {
                label: label.clone(),
                value: entry.field_view(focused),
            },
            PromptSpec::Secret { label } => Content::Secret {
                label: label.clone(),
                field: entry.field,
                sign_in: sign_in(*action, job.is_some()),
            },
            PromptSpec::Choice { label, options } => Content::Choice {
                label: label.clone(),
                options: options.clone(),
            },
            PromptSpec::Confirm { label } => Content::YesNo {
                label: label.clone(),
            },
            // The model shows this one as a plan card.
            PromptSpec::Plan(plan) => Content::Plan {
                plan: (**plan).clone(),
                changed: true,
                ask: Ask::Confirm,
            },
            PromptSpec::Conflicts { files } => Content::Conflicts {
                files: files.clone(),
                picks: entry.picks.clone(),
            },
        },
        Modal::Error { error, .. } => Content::Error((*error).clone()),
    }
}

fn sign_in(action: Option<&str>, job: bool) -> bool {
    !job && action == Some("account.sign_in")
}

impl App {
    /// Forgets what was put into another screen.
    fn enter_screen(&mut self, ticket: Ticket) {
        if self.entry.ticket == Some(ticket) {
            return;
        }
        let mut entry = Entry {
            ticket: Some(ticket),
            ..Entry::default()
        };
        if let Some(Modal::Prompt {
            spec: PromptSpec::Conflicts { files },
            ..
        }) = self.data.modal()
        {
            entry.picks = vec![None; files.len()];
        }
        self.entry = entry;
    }

    /// Makes the field's buffer, when the screen on top has a field and none yet.
    fn make_field(&mut self) {
        if self.entry.buffer.is_some() {
            return;
        }
        let first = match self.data.modal() {
            Some(Modal::Prompt {
                spec: PromptSpec::Text { default, .. },
                ..
            }) => default.clone(),
            Some(Modal::Strong {
                strength: Strength::Typed(_),
                ..
            }) => String::new(),
            _ => return,
        };
        self.entry.buffer = Some(EditBuffer::new(
            &mut self.page.fonts,
            &dialog::field_style(dialog::FIELD_TEXT_COLOR),
            &first,
            MAX_TEXT,
        ));
    }

    /// Where the hold on the screen of `ticket` is, as of the last frame on the display.
    fn hold_state(&self, ticket: Ticket) -> Hold {
        let is_hold = |key: Option<&str>| key == Some(dialog::HOLD_BUTTON);
        match self.holding {
            Some(h) if h.ticket == ticket => {
                let held = self.now.unwrap_or(h.since).saturating_sub(h.since);
                Hold::Holding(held.as_secs_f32() / HOLD.as_secs_f32())
            }
            _ if is_hold(self.interaction.armed().map(|t| t.key.as_str())) => Hold::Armed,
            _ => Hold::Idle,
        }
    }

    /// Adds the safety screen on top, if there is one.
    pub(super) fn draw_dialog(&mut self, size: (f32, f32)) {
        self.pending.dialog = None;
        self.pending.stuck = None;
        let Some(ticket) = self.data.modal_ticket() else {
            return;
        };
        self.enter_screen(ticket);
        self.make_field();
        let hold = self.hold_state(ticket);
        let changed = self.changed == Some(ticket);
        // The caret shows however the field got focus.
        let focused = self
            .interaction
            .focus()
            .is_some_and(|t| t.key == dialog::FIELD);
        let Some(content) = self
            .data
            .modal()
            .map(|modal| content(&modal, &self.entry, changed, hold, focused))
        else {
            return;
        };
        let built = Dialog::new(
            &mut self.page.fonts,
            size,
            ticket,
            &content,
            self.entry.page,
        );
        #[cfg(test)]
        let built = if self.break_dialogs {
            Err(crate::panel::PanelError::broken())
        } else {
            built
        };
        match built {
            Ok(drawn) => {
                self.entry.page = drawn.page().0;
                if let Some(scroll) = drawn.field_scroll() {
                    self.entry.scroll = scroll;
                }
                let id = drawn.layer().id;
                // A text field shows where typing goes however it got focus.
                let focus = self
                    .interaction
                    .focus_visible()
                    .or_else(|| self.interaction.focus().filter(|t| t.key == dialog::FIELD))
                    .filter(|t| t.layer == id)
                    .map(|t| t.key.as_str());
                drawn.draw(&mut self.list, focus);
                self.pending.dialog = Some(Rc::new(drawn));
            }
            // It stays pending: nothing unseen is answered or cancelled. The user is told
            // and may cancel it.
            Err(e) => {
                tracing::error!("a dialog could not be drawn: {e}");
                self.pending.stuck = Some(ticket);
                let lines = ["Press Esc to cancel it.".to_owned()];
                let heading = "A question could not be shown";
                let panel = Panel::new(&mut self.page.fonts, size, heading, &lines);
                #[cfg(test)]
                let panel = if self.break_panels {
                    Err(crate::panel::PanelError::broken())
                } else {
                    panel
                };
                match panel {
                    Ok(panel) => {
                        panel.draw(&mut self.list);
                        self.pending.panel = Some(Rc::new(panel));
                    }
                    // A blank window with one layer, built at startup, so that there is
                    // still something for Esc to reach.
                    Err(e) => {
                        tracing::error!("the panel for it could not be drawn either: {e}");
                        self.pending.screen = None;
                        self.pending.panel = Some(Rc::clone(&self.fallback));
                        self.list = DisplayList::blank(f64::from(size.0), f64::from(size.1));
                    }
                }
            }
        }
    }

    /// The frame just taken over is on the display, since `at`: tells the model about a
    /// new screen, and moves a hold on. Returns whether to draw again.
    pub(super) fn shown_screen(&mut self, at: Duration) -> bool {
        let previous = self.now.replace(at);
        let Some(drawn) = self.shown.dialog.clone() else {
            self.since = None;
            self.stop_hold();
            return false;
        };
        let ticket = drawn.ticket();
        let complete = drawn.answerable();
        let new = self
            .since
            .is_none_or(|shown| shown.ticket != ticket || (complete && !shown.complete));
        if new && (complete || drawn.is_error()) {
            let latest = self.interaction.latest();
            self.since = Some(Shown {
                ticket,
                at,
                after: latest.get(),
                complete,
            });
            if complete {
                self.data.presented(ticket, act::number(latest), at);
                if let Some(modal) = self.data.modal() {
                    say(format_args!("gui: dialog {}", kind(&modal)));
                }
            }
        }
        let Some(held) = self.holding else {
            return false;
        };
        let down = self
            .interaction
            .holding()
            .is_some_and(|t| t.key == dialog::HOLD_BUTTON);
        if held.ticket != ticket || !down {
            self.stop_hold();
            return true;
        }
        // Up to the frame before: every release that came before it has been heard.
        if let Some(seen) = previous.filter(|&seen| seen >= held.since) {
            self.data.tick(seen);
            self.effects();
            if self.data.modal_ticket() != Some(ticket) {
                self.holding = None;
            }
        }
        true
    }

    pub(super) fn stop_hold(&mut self) {
        if let Some(held) = self.holding.take() {
            self.data.hold_stop(held.ticket);
        }
    }

    /// Does what one intent asks while the dialog `drawn` is on the display. Nothing
    /// under it can be reached.
    pub(super) fn act_on_dialog(&mut self, drawn: &Dialog, intent: Intent) {
        let ticket = drawn.ticket();
        self.enter_if_current(ticket);
        match intent {
            Intent::App {
                target, provenance, ..
            } => self.activate(drawn, &target.key, &provenance),
            Intent::HoldStart {
                target, provenance, ..
            } if drawn.answerable() && target.key == dialog::HOLD_BUTTON => {
                let press = press(&provenance);
                if self.data.hold_start(ticket, press) == Outcome::Accepted {
                    self.holding = Some(Holding {
                        ticket,
                        since: press.at,
                    });
                }
            }
            Intent::Enter { provenance } if drawn.answerable() => {
                // The model confirms a plan card with it and nothing else.
                if self.data.modal_ticket() == Some(ticket) {
                    self.data.enter(press(&provenance));
                }
            }
            Intent::Dismiss { provenance } => self.leave(drawn, &provenance),
            Intent::Paste { provenance } if drawn.answerable() => {
                self.ask_to_paste(ticket, &provenance);
            }
            Intent::Edit { target, edit }
                if target.key == dialog::FIELD && self.entry.ticket == Some(ticket) =>
            {
                self.edit_field(drawn, &edit);
            }
            Intent::Copy { target, cut, .. }
                if target.key == dialog::FIELD && self.entry.ticket == Some(ticket) =>
            {
                self.copy_field(cut);
            }
            Intent::SetValue { target, value }
                if target.key == dialog::FIELD && self.entry.ticket == Some(ticket) =>
            {
                self.change_field(EditOp::Set(&value));
            }
            _ => {}
        }
    }

    /// Starts the entry of the screen on the display, unless the model has moved on.
    fn enter_if_current(&mut self, ticket: Ticket) {
        if self.data.modal_ticket() == Some(ticket) {
            self.enter_screen(ticket);
        }
    }

    fn change_field(&mut self, op: EditOp<'_>) {
        if let Some(buffer) = &mut self.entry.buffer {
            buffer.apply(&mut self.page.fonts, op);
        }
    }

    /// An edit made in the field of the dialog `drawn`.
    fn edit_field(&mut self, drawn: &Dialog, edit: &TextEdit) {
        let op = match *edit {
            TextEdit::Insert(ref text) => EditOp::Type(text),
            TextEdit::Delete { forward, word } => EditOp::Delete { forward, word },
            TextEdit::Move { motion, extend } => EditOp::Move { motion, extend },
            TextEdit::SelectAll => EditOp::SelectAll,
            TextEdit::Undo => EditOp::Undo,
            TextEdit::Redo => EditOp::Redo,
            TextEdit::Preedit { ref text, cursor } => EditOp::Preedit { text, cursor },
            TextEdit::Commit(ref text) => EditOp::Commit(text),
            TextEdit::Select { anchor, focus } => EditOp::Select { anchor, focus },
            TextEdit::Replace(ref text) => EditOp::Paste(text),
            TextEdit::Point { x, y, gesture } => {
                let Some((x, y)) = drawn.field_point(x, y) else {
                    return;
                };
                EditOp::Point { x, y, gesture }
            }
            _ => return,
        };
        self.change_field(op);
    }

    /// Takes the field's selection for the clipboard.
    fn copy_field(&mut self, cut: bool) {
        let Some(text) = self.entry.buffer.as_ref().and_then(EditBuffer::selected) else {
            return;
        };
        self.copied = Some(Zeroizing::new(text.to_owned()));
        if cut {
            self.change_field(EditOp::Delete {
                forward: false,
                word: false,
            });
        }
    }

    /// A button of the dialog `drawn`.
    fn activate(&mut self, drawn: &Dialog, key: &str, provenance: &InputProvenance) {
        let ticket = drawn.ticket();
        // Cancelling and turning pages answer nothing, and need no wait (S8); closing an
        // error does ([`App::leave`]).
        match key {
            dialog::CANCEL | dialog::CLOSE => return self.leave(drawn, provenance),
            dialog::PREVIOUS | dialog::NEXT if self.entry.ticket == Some(ticket) => {
                let (page, pages) = drawn.page();
                self.entry.page = if key == dialog::NEXT {
                    (page + 1).min(pages - 1)
                } else {
                    page.saturating_sub(1)
                };
                return;
            }
            _ => {}
        }
        if !drawn.answerable() || self.entry.ticket != Some(ticket) {
            return;
        }
        let press = press(provenance);
        let answer = |app: &mut App, answer| app.data.answer(ticket, answer, press);
        let index = |prefix| {
            key.strip_prefix(prefix)
                .and_then(|i| i.parse::<usize>().ok())
        };
        match key {
            dialog::CONFIRM => {
                self.data.confirm(ticket, press);
            }
            dialog::TYPED => {
                let typed = self.entry.text();
                self.data.type_name(ticket, &typed, press);
            }
            dialog::PASTE => self.ask_to_paste(ticket, provenance),
            dialog::CLEAR => {
                self.entry.code = None;
                self.entry.field = Field::Empty;
            }
            dialog::OPEN => self.data.open_sign_in_page(),
            dialog::SUBMIT => {
                // Too soon after the dialog appeared, it is ignored and the secret stays.
                if let Some(code) = self.entry.code.clone()
                    && answer(self, PromptAnswer::Secret(code)) != Outcome::Ignored
                {
                    self.entry = Entry::default();
                }
            }
            dialog::OK => {
                let text = self.entry.text();
                answer(self, PromptAnswer::Text(text));
            }
            dialog::YES => {
                answer(self, PromptAnswer::Yes);
            }
            dialog::NO => {
                answer(self, PromptAnswer::No);
            }
            dialog::APPLY => {
                let sides = self.entry.picks.iter().copied().collect::<Option<Vec<_>>>();
                if let Some(sides) = sides {
                    answer(self, PromptAnswer::Sides(sides));
                }
            }
            _ => {
                if let Some(option) = index(dialog::OPTION) {
                    answer(self, PromptAnswer::Choice(option));
                } else if let Some(file) = index(dialog::LOCAL) {
                    self.pick(file, Side::Local);
                } else if let Some(file) = index(dialog::CLOUD) {
                    self.pick(file, Side::Cloud);
                }
            }
        }
    }

    fn pick(&mut self, file: usize, side: Side) {
        if let Some(pick) = self.entry.picks.get_mut(file) {
            *pick = Some(side);
        }
    }

    /// Esc, Cancel or Close on the dialog `drawn`. Cancelling is instant, but an error
    /// is dismissed only by a press that [`Shown::takes`] (S9).
    fn leave(&mut self, drawn: &Dialog, press: &InputProvenance) {
        let ticket = drawn.ticket();
        let read = || {
            self.since.is_some_and(|shown| {
                shown.takes(ticket, press.event().get(), press.at(), press.repeat())
            })
        };
        if !drawn.is_error() || read() {
            self.close(ticket);
        }
    }

    /// Cancels the screen of `ticket` and forgets what was put into it.
    pub(super) fn close(&mut self, ticket: Ticket) {
        self.data.cancel(ticket);
        if self.entry.ticket == Some(ticket) {
            self.entry = Entry::default();
        }
    }

    /// Whether the screen on top takes text from the clipboard.
    fn takes_paste(&self, ticket: Ticket) -> bool {
        match self.data.modal() {
            Some(Modal::Prompt {
                ticket: top, spec, ..
            }) => {
                top == ticket && matches!(spec, PromptSpec::Secret { .. } | PromptSpec::Text { .. })
            }
            Some(Modal::Strong {
                ticket: top,
                strength: Strength::Typed(_),
                ..
            }) => top == ticket,
            _ => false,
        }
    }

    /// The clipboard is read only for a press that could answer the dialog
    /// ([`Shown::takes`]), and only for a dialog that takes it.
    fn ask_to_paste(&mut self, ticket: Ticket, press: &InputProvenance) {
        let fresh = self.since.is_some_and(|shown| {
            shown.takes(ticket, press.event().get(), press.at(), press.repeat())
        });
        if fresh && self.entry.ticket == Some(ticket) && self.takes_paste(ticket) {
            self.paste = Some(ticket);
        }
    }

    /// The clipboard's text, for the dialog that asked and no other.
    pub(super) fn take_paste(&mut self, text: Option<Zeroizing<String>>) -> bool {
        let Some(ticket) = self.paste.take() else {
            return false;
        };
        if self.entry.ticket != Some(ticket) || !self.takes_paste(ticket) {
            return false;
        }
        let secret_for = match self.data.modal() {
            Some(Modal::Prompt {
                spec: PromptSpec::Secret { .. },
                action,
                job,
                ..
            }) => Some(sign_in(action, job.is_some())),
            _ => None,
        };
        let Some(sign_in) = secret_for else {
            if let Some(text) = text.as_deref() {
                self.change_field(EditOp::Paste(text));
            }
            return true;
        };
        let secret = text.as_deref().and_then(|text| {
            if sign_in {
                gogo::auth::browser::code_from_input(text).map(|c| Secret::new(c.as_str()))
            } else {
                let text = text.trim_end_matches(['\r', '\n']);
                (!text.is_empty()).then(|| Secret::new(text))
            }
        });
        self.entry.field = match (&text, &secret) {
            (None, _) => Field::NothingToPaste,
            (Some(_), None) => Field::Rejected,
            (Some(_), Some(_)) => Field::Pasted,
        };
        self.entry.code = secret;
        true
    }
}

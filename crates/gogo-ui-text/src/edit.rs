//! The text of a one-line field while it is edited (design 24 §2), over parley's
//! `PlainEditor`, which has motion, selection and IME composition. Undo is ours.
//!
//! **Never for a secret.** The editor keeps plain copies of what it holds, in its buffer
//! and its undo steps, and can't wipe them. A secret field must not use it.

use std::ops::Range;

use gogo_ui_ir::{Gesture, Motion, TextStyle};
use parley::{Affinity, Cursor, PlainEditor, PlainEditorDriver, Selection};

use crate::FontSet;

/// The most undo steps kept.
pub const UNDO_STEPS: usize = 100;

/// One change to an [`EditBuffer`].
#[derive(Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum EditOp<'a> {
    /// Typed text, replacing the selection. Words typed in a row undo together.
    Type(&'a str),
    /// Pasted text, replacing the selection: an undo step of its own.
    Paste(&'a str),
    /// All of the text at once, as assistive technology sets it.
    Set(&'a str),
    /// Delete the selection, else a cluster or a word beside the caret.
    Delete {
        /// After the caret, not before it.
        forward: bool,
        /// A word, not a cluster.
        word: bool,
    },
    /// Move the caret.
    Move {
        /// Where.
        motion: Motion,
        /// Keep the other end of the selection where it is.
        extend: bool,
    },
    /// Select everything.
    SelectAll,
    /// Select from one byte offset to another, the caret at the second.
    Select {
        /// The end that stays.
        anchor: usize,
        /// The caret's end.
        focus: usize,
    },
    /// A pointer at a point of the laid-out text.
    Point {
        /// From the text's left edge, with any scrolling added.
        x: f32,
        /// From its top.
        y: f32,
        /// What it does.
        gesture: Gesture,
    },
    /// The IME's composition: it takes the selection's place and is not yet part of
    /// the text. Empty text ends it.
    Preedit {
        /// The text composed so far.
        text: &'a str,
        /// The IME's selection in it, in bytes; `None` hides the caret.
        cursor: Option<(usize, usize)>,
    },
    /// The IME's result.
    Commit(&'a str),
    /// Take the last step back.
    Undo,
    /// Make it again.
    Redo,
}

/// Without the text: a field may hold the user's data.
impl std::fmt::Debug for EditOp<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            EditOp::Type(_) => "Type",
            EditOp::Paste(_) => "Paste",
            EditOp::Set(_) => "Set",
            EditOp::Delete { .. } => "Delete",
            EditOp::Move { .. } => "Move",
            EditOp::SelectAll => "SelectAll",
            EditOp::Select { .. } => "Select",
            EditOp::Point { .. } => "Point",
            EditOp::Preedit { .. } => "Preedit",
            EditOp::Commit(_) => "Commit",
            EditOp::Undo => "Undo",
            EditOp::Redo => "Redo",
        };
        f.write_str(name)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    anchor: usize,
    focus: usize,
}

/// Edits that undo as one while they follow each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Run {
    /// `space`: the last character typed was white space.
    Typing {
        space: bool,
    },
    Deleting {
        forward: bool,
    },
}

/// A one-line field's text, caret, selection, composition and undo steps.
///
/// Line breaks and other control characters never get in: of several lines only the
/// first is taken. Offsets are bytes of [`EditBuffer::shown`].
pub struct EditBuffer {
    editor: PlainEditor<()>,
    max_chars: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    run: Option<Run>,
    /// What there was before the composition, until it is committed or given up.
    composing: Option<Snapshot>,
}

/// Without the text.
impl std::fmt::Debug for EditBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditBuffer")
            .field("bytes", &self.editor.raw_text().len())
            .field("composing", &self.editor.is_composing())
            .finish_non_exhaustive()
    }
}

/// How far past the limit a text is segmented: enough to see where the grapheme at the
/// limit ends.
const LOOK_AHEAD: usize = 64;

/// The first line without control characters, at most `room` characters of it, cut
/// between graphemes: an accent is never taken off its letter.
fn one_line(fonts: &mut FontSet, text: &str, room: usize) -> String {
    let mut line: String = text
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(room.saturating_add(LOOK_AHEAD))
        .collect();
    let Some((limit, _)) = line.char_indices().nth(room) else {
        return line;
    };
    // parley's clusters are graphemes, however the text is styled.
    let mut shaped = fonts.shape(&line, &TextStyle::default());
    shaped.break_all_lines(None);
    let cut = shaped
        .lines()
        .flat_map(|l| l.runs())
        .flat_map(|run| run.clusters().map(|c| c.text_range().start))
        .filter(|&start| start <= limit)
        .max()
        .unwrap_or(0);
    line.truncate(cut);
    line
}

/// The nearest character boundary at or before `at`.
fn boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

impl EditBuffer {
    /// A field in `style` holding `text`, the caret at its end, of at most `max_chars`
    /// characters.
    pub fn new(fonts: &mut FontSet, style: &TextStyle, text: &str, max_chars: usize) -> Self {
        let mut editor = PlainEditor::new(style.size);
        for property in fonts.properties(style) {
            editor.edit_styles().insert(property);
        }
        editor.set_text(&one_line(fonts, text, max_chars));
        let mut buffer = EditBuffer {
            editor,
            max_chars,
            undo: Vec::new(),
            redo: Vec::new(),
            run: None,
            composing: None,
        };
        buffer.driver(fonts).move_to_text_end();
        buffer
    }

    /// The text, without a composition in progress.
    pub fn text(&self) -> String {
        self.editor.text().to_string()
    }

    /// The text to draw: with the composition.
    pub fn shown(&self) -> &str {
        self.editor.raw_text()
    }

    /// The selection as (the end that stays, the caret's end).
    pub fn selection(&self) -> (usize, usize) {
        let selection = self.editor.raw_selection();
        (selection.anchor().index(), selection.focus().index())
    }

    /// The selected bytes, in order.
    pub fn selected_range(&self) -> Range<usize> {
        let (anchor, focus) = self.selection();
        anchor.min(focus)..anchor.max(focus)
    }

    /// The selected text; `None` for none, and during a composition.
    pub fn selected(&self) -> Option<&str> {
        self.editor.selected_text()
    }

    /// Where the caret is drawn; `None` while the IME hides it.
    pub fn caret(&self) -> Option<usize> {
        self.editor.is_cursor_visible().then(|| self.selection().1)
    }

    /// The composition's bytes.
    pub fn preedit(&self) -> Option<Range<usize>> {
        self.editor.raw_compose().clone()
    }

    fn driver<'a>(&'a mut self, fonts: &'a mut FontSet) -> PlainEditorDriver<'a, ()> {
        let (font_cx, layout_cx) = fonts.contexts();
        self.editor.driver(font_cx, layout_cx)
    }

    fn snapshot(&self) -> Snapshot {
        let (anchor, focus) = self.selection();
        Snapshot {
            text: self.text(),
            anchor,
            focus,
        }
    }

    /// Characters that still fit once the selection is replaced.
    fn room(&self) -> usize {
        let held = self.shown().chars().count();
        let selected = self
            .shown()
            .get(self.selected_range())
            .map_or(0, |s| s.chars().count());
        self.max_chars.saturating_sub(held - selected)
    }

    fn remember(&mut self, before: Snapshot) {
        if self.undo.len() == UNDO_STEPS {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
    }

    /// Records the change since `before`, unless it carries on the run.
    fn changed(&mut self, before: Snapshot, run: Option<Run>) {
        if before.text == self.text() {
            return;
        }
        let carries_on = match (self.run, run) {
            // A new word is a new step.
            (Some(Run::Typing { space: was }), Some(Run::Typing { space })) => space || !was,
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        if carries_on {
            self.redo.clear();
        } else {
            self.remember(before);
        }
        self.run = run;
    }

    /// Takes a composition out of the buffer.
    fn drop_preedit(&mut self, fonts: &mut FontSet) {
        if self.editor.is_composing() {
            self.driver(fonts).clear_compose();
        }
    }

    /// Ends a composition that was not committed. What it replaced stays replaced, as a
    /// step of its own.
    fn settle(&mut self, fonts: &mut FontSet) {
        self.drop_preedit(fonts);
        if let Some(before) = self.composing.take() {
            self.changed(before, None);
        }
    }

    fn restore(&mut self, fonts: &mut FontSet, to: &Snapshot) {
        self.editor.set_text(&to.text);
        self.select(fonts, to.anchor, to.focus);
    }

    fn select(&mut self, fonts: &mut FontSet, anchor: usize, focus: usize) {
        let anchor = boundary(self.shown(), anchor);
        let focus = boundary(self.shown(), focus);
        let len = self.shown().len();
        let mut driver = self.driver(fonts);
        let layout = driver.layout();
        let cursor = |at: usize| {
            let affinity = if at == len {
                Affinity::Upstream
            } else {
                Affinity::Downstream
            };
            Cursor::from_byte_index(layout, at, affinity)
        };
        let selection = Selection::new(cursor(anchor), cursor(focus));
        driver.set_selection(selection);
    }

    fn insert(&mut self, fonts: &mut FontSet, text: &str) {
        let text = one_line(fonts, text, self.room());
        if !text.is_empty() {
            self.driver(fonts).insert_or_replace_selection(&text);
        }
    }

    /// Makes one change.
    pub fn apply(&mut self, fonts: &mut FontSet, op: EditOp<'_>) {
        match op {
            EditOp::Preedit { text, cursor } => {
                let clean: String = text.chars().filter(|c| !c.is_control()).collect();
                if clean.is_empty() {
                    // The IME clears the composition right before it commits.
                    self.drop_preedit(fonts);
                    return;
                }
                if self.composing.is_none() {
                    self.composing = Some(self.snapshot());
                }
                let cursor = cursor.map(|(a, b)| (boundary(&clean, a), boundary(&clean, b)));
                self.driver(fonts).set_compose(&clean, cursor);
                return;
            }
            EditOp::Commit(text) => {
                self.drop_preedit(fonts);
                let before = self.composing.take().unwrap_or_else(|| self.snapshot());
                self.insert(fonts, text);
                self.changed(before, None);
                return;
            }
            _ => self.settle(fonts),
        }
        let before = self.snapshot();
        match op {
            EditOp::Type(text) => {
                self.insert(fonts, text);
                let space = text.chars().last().is_some_and(char::is_whitespace);
                // Typing over a selection is a step of its own.
                if before.anchor != before.focus {
                    self.run = None;
                }
                self.changed(before, Some(Run::Typing { space }));
            }
            EditOp::Paste(text) => {
                self.insert(fonts, text);
                self.changed(before, None);
            }
            EditOp::Set(text) => {
                let text = one_line(fonts, text, self.max_chars);
                self.editor.set_text(&text);
                self.driver(fonts).move_to_text_end();
                self.changed(before, None);
            }
            EditOp::Delete { forward, word } => {
                let selected = before.anchor != before.focus;
                let mut driver = self.driver(fonts);
                match (forward, word) {
                    _ if selected => driver.delete_selection(),
                    (false, false) => driver.backdelete(),
                    (false, true) => driver.backdelete_word(),
                    (true, false) => driver.delete(),
                    (true, true) => driver.delete_word(),
                }
                if selected {
                    self.run = None;
                }
                let run = (!selected && !word).then_some(Run::Deleting { forward });
                self.changed(before, run);
            }
            EditOp::Move { motion, extend } => {
                self.run = None;
                let mut driver = self.driver(fonts);
                match (motion, extend) {
                    (Motion::Left, false) => driver.move_left(),
                    (Motion::Right, false) => driver.move_right(),
                    (Motion::WordLeft, false) => driver.move_word_left(),
                    (Motion::WordRight, false) => driver.move_word_right(),
                    (Motion::Start, false) => driver.move_to_text_start(),
                    (Motion::End, false) => driver.move_to_text_end(),
                    (Motion::Left, true) => driver.select_left(),
                    (Motion::Right, true) => driver.select_right(),
                    (Motion::WordLeft, true) => driver.select_word_left(),
                    (Motion::WordRight, true) => driver.select_word_right(),
                    (Motion::Start, true) => driver.select_to_text_start(),
                    (Motion::End, true) => driver.select_to_text_end(),
                }
            }
            EditOp::SelectAll => {
                self.run = None;
                self.driver(fonts).select_all();
            }
            EditOp::Select { anchor, focus } => {
                self.run = None;
                self.select(fonts, anchor, focus);
            }
            EditOp::Point { x, y, gesture } => {
                self.run = None;
                let mut driver = self.driver(fonts);
                match gesture {
                    Gesture::Place => driver.move_to_point(x, y),
                    Gesture::Extend => driver.extend_selection_to_point(x, y),
                    Gesture::Word => driver.select_word_at_point(x, y),
                    Gesture::All => driver.select_all(),
                }
            }
            EditOp::Undo => {
                self.run = None;
                if let Some(to) = self.undo.pop() {
                    self.restore(fonts, &to);
                    self.redo.push(before);
                }
            }
            EditOp::Redo => {
                self.run = None;
                if let Some(to) = self.redo.pop() {
                    self.restore(fonts, &to);
                    self.undo.push(before);
                }
            }
            EditOp::Preedit { .. } | EditOp::Commit(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Field {
        fonts: FontSet,
        buffer: EditBuffer,
    }

    fn field(text: &str) -> Field {
        let mut fonts = FontSet::bundled();
        let buffer = EditBuffer::new(&mut fonts, &TextStyle::default(), text, 40);
        Field { fonts, buffer }
    }

    impl Field {
        fn apply(&mut self, op: EditOp<'_>) -> &mut Self {
            self.buffer.apply(&mut self.fonts, op);
            self
        }

        fn typing(&mut self, text: &str) -> &mut Self {
            for c in text.chars() {
                self.apply(EditOp::Type(&c.to_string()));
            }
            self
        }

        fn go(&mut self, motion: Motion, extend: bool) -> &mut Self {
            self.apply(EditOp::Move { motion, extend })
        }

        fn delete(&mut self, forward: bool, word: bool) -> &mut Self {
            self.apply(EditOp::Delete { forward, word })
        }

        fn is(&self, text: &str) {
            assert_eq!(self.buffer.text(), text);
        }
    }

    #[test]
    fn typing_goes_where_the_caret_is_and_replaces_the_selection() {
        let mut f = field("ac");
        assert_eq!(f.buffer.selection(), (2, 2), "the caret starts at the end");
        f.go(Motion::Left, false).typing("b").is("abc");
        f.go(Motion::Start, false).go(Motion::Right, true);
        assert_eq!(f.buffer.selected(), Some("a"));
        f.typing("A").is("Abc");
        f.apply(EditOp::SelectAll).typing("x").is("x");
    }

    #[test]
    fn words_and_line_ends() {
        let mut f = field("one two three");
        f.go(Motion::WordLeft, true);
        assert_eq!(f.buffer.selected(), Some("three"));
        f.go(Motion::Start, false).delete(true, true);
        assert!(f.buffer.text().ends_with("two three"));
        f.go(Motion::End, false).delete(false, true);
        assert!(f.buffer.text().ends_with("two "));
        f.delete(false, false).delete(false, false);
        assert!(f.buffer.text().ends_with("tw"));
        f.go(Motion::Start, true);
        assert_eq!(f.buffer.selected_range().start, 0);
        f.delete(false, false).is("");
        f.delete(false, false).delete(true, false).is("");
    }

    #[test]
    fn a_selection_is_made_by_offsets_on_character_boundaries() {
        let mut f = field("héllo");
        f.apply(EditOp::Select {
            anchor: 99,
            focus: 2,
        });
        assert_eq!(f.buffer.selection(), (6, 1));
        assert_eq!(f.buffer.selected(), Some("éllo"));
    }

    #[test]
    fn one_line_and_no_more_than_the_limit() {
        let mut f = field("");
        f.apply(EditOp::Paste("a\u{7}b\nsecond line")).is("ab");
        f.apply(EditOp::Paste(&"x".repeat(100)));
        assert_eq!(f.buffer.text().chars().count(), 40);
        f.typing("y");
        assert_eq!(f.buffer.text().chars().count(), 40);
        f.apply(EditOp::SelectAll).typing("y").is("y");
        f.apply(EditOp::Set("new\r\nrest")).is("new");
    }

    #[test]
    fn the_limit_never_splits_a_grapheme() {
        let mut fonts = FontSet::bundled();
        let style = TextStyle::default();
        // "e" and a combining acute accent are two characters and one grapheme.
        let accented = "abe\u{301}";
        let mut buffer = EditBuffer::new(&mut fonts, &style, "", 3);
        buffer.apply(&mut fonts, EditOp::Paste(accented));
        assert_eq!(buffer.text(), "ab");
        buffer.apply(&mut fonts, EditOp::Set(accented));
        assert_eq!(buffer.text(), "ab");
        assert_eq!(
            EditBuffer::new(&mut fonts, &style, accented, 3).text(),
            "ab"
        );
        assert_eq!(
            EditBuffer::new(&mut fonts, &style, accented, 4).text(),
            accented
        );
        // A joined emoji goes whole or not at all.
        buffer.apply(&mut fonts, EditOp::Set("a\u{1f469}\u{200d}\u{1f4bb}"));
        assert_eq!(buffer.text(), "a");
    }

    #[test]
    fn typing_undoes_a_word_at_a_time() {
        let mut f = field("");
        f.typing("one two");
        f.apply(EditOp::Undo).is("one ");
        f.apply(EditOp::Undo).is("");
        f.apply(EditOp::Undo).is("");
        f.apply(EditOp::Redo).is("one ");
        f.apply(EditOp::Redo).is("one two");
        f.apply(EditOp::Redo).is("one two");
    }

    #[test]
    fn paste_delete_and_a_moved_caret_are_steps_of_their_own() {
        let mut f = field("ab");
        f.apply(EditOp::Paste("cd")).delete(false, false);
        f.delete(false, false).is("ab");
        f.apply(EditOp::Undo).is("abcd");
        f.apply(EditOp::Undo).is("ab");
        f.typing("x")
            .go(Motion::Start, false)
            .typing("y")
            .is("yabx");
        f.apply(EditOp::Undo).is("abx");
        assert_eq!(f.buffer.selection(), (0, 0), "with the caret");
        f.apply(EditOp::Undo).is("ab");
        f.typing("z");
        f.apply(EditOp::Redo).is("abz");
    }

    #[test]
    fn only_so_many_steps_are_kept() {
        let mut f = field("");
        for _ in 0..UNDO_STEPS + 5 {
            f.apply(EditOp::Set("a")).apply(EditOp::Set("b"));
        }
        assert_eq!(f.buffer.undo.len(), UNDO_STEPS);
    }

    #[test]
    fn a_composition_is_shown_but_is_not_the_text_until_committed() {
        let mut f = field("ab");
        f.go(Motion::Left, false);
        f.apply(EditOp::Preedit {
            text: "ni",
            cursor: Some((2, 2)),
        });
        f.is("ab");
        assert_eq!(f.buffer.shown(), "anib");
        assert_eq!(f.buffer.preedit(), Some(1..3));
        assert_eq!(f.buffer.caret(), Some(3));
        assert_eq!(f.buffer.selected(), None);
        f.apply(EditOp::Preedit {
            text: "你",
            cursor: None,
        });
        assert_eq!(f.buffer.shown(), "a你b");
        assert_eq!(f.buffer.caret(), None);
        // What winit sends: the composition is cleared, then the result comes.
        f.apply(EditOp::Preedit {
            text: "",
            cursor: None,
        });
        f.apply(EditOp::Commit("你")).is("a你b");
        assert_eq!(f.buffer.preedit(), None);
        assert_eq!(f.buffer.caret(), Some(4));
        f.apply(EditOp::Undo).is("ab");
    }

    #[test]
    fn a_composition_given_up_leaves_the_replaced_selection_undoable() {
        let mut f = field("ab");
        f.apply(EditOp::SelectAll);
        f.apply(EditOp::Preedit {
            text: "x",
            cursor: Some((1, 1)),
        });
        f.apply(EditOp::Preedit {
            text: "",
            cursor: None,
        });
        f.is("");
        f.typing("c").is("c");
        f.apply(EditOp::Undo).is("");
        f.apply(EditOp::Undo).is("ab");
    }

    #[test]
    fn a_point_places_extends_and_selects_a_word() {
        let mut f = field("one two");
        let at = |gesture, x| EditOp::Point { x, y: 5.0, gesture };
        f.apply(at(Gesture::Place, 0.0));
        assert_eq!(f.buffer.selection(), (0, 0));
        f.apply(at(Gesture::Extend, 1000.0));
        assert_eq!(f.buffer.selection(), (0, 7));
        f.apply(at(Gesture::Word, 2.0));
        assert_eq!(f.buffer.selected(), Some("one"));
        f.apply(at(Gesture::All, 2.0));
        assert_eq!(f.buffer.selected(), Some("one two"));
    }

    #[test]
    fn debug_output_never_holds_the_text() {
        let mut f = field("hidden words");
        f.apply(EditOp::Preedit {
            text: "pinyin",
            cursor: None,
        });
        let printed = format!(
            "{:?} {:?} {:?}",
            f.buffer,
            EditOp::Type("hidden"),
            EditOp::Preedit {
                text: "pinyin",
                cursor: None
            }
        );
        assert!(!printed.contains("hidden"), "{printed}");
        assert!(!printed.contains("pinyin"), "{printed}");
    }

    #[test]
    fn the_drawn_layout_puts_the_caret_and_selection_between_the_glyphs() {
        let mut fonts = FontSet::bundled();
        let style = TextStyle::default();
        let layout = fonts.layout("hello", &style);
        let (start, end) = (layout.caret_rect(0), layout.caret_rect(5));
        assert!(start.x0 < 1.0 && end.x0 > 20.0 && start.y1 > start.y0);
        assert_eq!(layout.hit(end.x0, 5.0), 5);
        let rects = layout.selection_rects(0..5);
        assert_eq!(rects.len(), 1);
        assert!((rects[0].x1 - end.x0).abs() < 1.0);
        assert!(layout.selection_rects(2..2).is_empty());
        let empty = fonts.layout("", &style).caret_rect(0);
        assert!(empty.y1 - empty.y0 > style.size * 0.8, "{empty:?}");
    }

    #[test]
    fn a_long_line_moves_only_as_far_as_the_caret_needs() {
        let mut fonts = FontSet::bundled();
        let text = "m".repeat(40);
        let layout = fonts.layout(&text, &TextStyle::default());
        let width = 100.0;
        let at_end = layout.scroll_for(0.0, text.len(), width);
        assert!(at_end > 0.0);
        assert!(layout.caret_rect(text.len()).x1 - at_end <= width + 0.01);
        // One step back is still in view: nothing moves.
        assert_eq!(layout.scroll_for(at_end, text.len() - 1, width), at_end);
        assert_eq!(layout.scroll_for(at_end, 0, width), 0.0);
        assert_eq!(layout.scroll_for(50.0, 1, 10_000.0), 0.0);
        assert_eq!(layout.scroll_for(f32::NAN, 0, 0.0), 0.0);
    }
}

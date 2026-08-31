// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/composer.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; draft history is in-process rather
//            than written to $DSH_HOME; bracketed-paste marker stripping is
//            unnecessary (crossterm delivers paste as its own event).
//! Draft editing model.
//!
//! The draft is a string plus a character cursor; every motion and deletion is a
//! pure transform, so the editor can be tested without a terminal. Indices are
//! `char`s, never bytes, so an accented character or an ideograph is one step.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/composer.ts`. The draft
//! history is in-process here rather than written to a file: `dsh` keeps it in
//! `$DSH_HOME` because it is a daily driver, while this is a test host whose log
//! is the artefact worth keeping.

use crate::text::{display_width, wrap_to_width};
use crate::theme::RowTone;

/// Where the caret can be asked to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One character left.
    Left,
    /// One character right.
    Right,
    /// To the start of the word before the caret.
    WordLeft,
    /// Past the word after the caret.
    WordRight,
    /// To the start of the current logical line.
    LineStart,
    /// To the end of the current logical line.
    LineEnd,
    /// Same column, previous logical line.
    Up,
    /// Same column, next logical line.
    Down,
}

/// What a deletion removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deletion {
    /// The character before the caret.
    BackChar,
    /// The character after the caret.
    ForwardChar,
    /// The word before the caret.
    BackWord,
    /// Everything back to the start of the line.
    ToLineStart,
    /// Everything forward to the end of the line.
    ToLineEnd,
}

/// The draft, its caret, and the history behind it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composer {
    draft: String,
    cursor: usize,
    history: Vec<String>,
    /// `None` means "on the live draft", not "at the end of history".
    history_index: Option<usize>,
}

/// Where the caret sits, for vertical-key routing and for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaretLine {
    /// Logical line the caret sits on, counted from zero.
    pub index: usize,
    /// How many logical lines the draft has.
    pub count: usize,
    /// Character offset of the caret within that line.
    pub column: usize,
}

/// The tone of the composer prompt, by permission preset.
///
/// The prompt is the one thing on screen at the moment of typing, so it is where
/// the mode belongs: a mode that runs tools with no approval at all has to look
/// like it, and a read-only session should not read as one that can write. A
/// preset this profile does not recognise keeps the ordinary tone rather than
/// guessing at how permissive it is — the status row still names it in full.
pub fn prompt_tone(mode: &str) -> RowTone {
    let mode = mode.to_lowercase();
    if mode.contains("accept") || mode.contains("danger") {
        RowTone::ModeDanger
    } else if mode.contains("plan") || mode.contains("read-only") {
        RowTone::ModeRestricted
    } else {
        RowTone::User
    }
}

fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

fn line_start(characters: &[char], cursor: usize) -> usize {
    let mut index = cursor;
    while index > 0 && characters[index - 1] != '\n' {
        index -= 1;
    }
    index
}

fn line_end(characters: &[char], cursor: usize) -> usize {
    let mut index = cursor;
    while index < characters.len() && characters[index] != '\n' {
        index += 1;
    }
    index
}

fn word_left(characters: &[char], cursor: usize) -> usize {
    let mut index = cursor;
    while index > 0 && characters[index - 1].is_whitespace() {
        index -= 1;
    }
    while index > 0 && !characters[index - 1].is_whitespace() {
        index -= 1;
    }
    index
}

fn word_right(characters: &[char], cursor: usize) -> usize {
    let mut index = cursor;
    while index < characters.len() && !characters[index].is_whitespace() {
        index += 1;
    }
    while index < characters.len() && characters[index].is_whitespace() {
        index += 1;
    }
    index
}

/// Moves to the same column of the neighbouring logical line.
fn vertical_target(characters: &[char], cursor: usize, down: bool) -> usize {
    let start = line_start(characters, cursor);
    let column = cursor - start;
    if !down {
        if start == 0 {
            return 0;
        }
        let previous = line_start(characters, start - 1);
        return (previous + column).min(start - 1);
    }
    let end = line_end(characters, cursor);
    if end >= characters.len() {
        return characters.len();
    }
    let next_end = line_end(characters, end + 1);
    (end + 1 + column).min(next_end)
}

impl Composer {
    /// The draft as typed.
    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// Character index of the caret.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// True when there is nothing to send.
    pub fn is_empty(&self) -> bool {
        self.draft.is_empty()
    }

    /// Drafts sent this session, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Inserts text at the caret and leaves the caret after it.
    pub fn insert(&mut self, text: &str) {
        let inserted = chars(text);
        if inserted.is_empty() {
            self.history_index = None;
            return;
        }
        let mut characters = chars(&self.draft);
        let at = self.cursor.min(characters.len());
        for (offset, ch) in inserted.iter().enumerate() {
            characters.insert(at + offset, *ch);
        }
        self.set(characters, at + inserted.len());
    }

    /// Inserts a newline rather than sending.
    pub fn newline(&mut self) {
        self.insert("\n");
    }

    /// Moves the caret.
    pub fn move_caret(&mut self, motion: Motion) {
        let characters = chars(&self.draft);
        let cursor = self.cursor.min(characters.len());
        let next = match motion {
            Motion::Left => cursor.saturating_sub(1),
            Motion::Right => cursor + 1,
            Motion::WordLeft => word_left(&characters, cursor),
            Motion::WordRight => word_right(&characters, cursor),
            Motion::LineStart => line_start(&characters, cursor),
            Motion::LineEnd => line_end(&characters, cursor),
            Motion::Up => vertical_target(&characters, cursor, false),
            Motion::Down => vertical_target(&characters, cursor, true),
        };
        self.cursor = next.min(characters.len());
    }

    /// Deletes around the caret.
    pub fn delete(&mut self, kind: Deletion) {
        let mut characters = chars(&self.draft);
        let cursor = self.cursor.min(characters.len());
        let (start, end) = match kind {
            Deletion::BackChar => (cursor.saturating_sub(1), cursor),
            Deletion::ForwardChar => (cursor, (cursor + 1).min(characters.len())),
            Deletion::BackWord => (word_left(&characters, cursor), cursor),
            Deletion::ToLineStart => (line_start(&characters, cursor), cursor),
            Deletion::ToLineEnd => (cursor, line_end(&characters, cursor)),
        };
        if start >= end {
            self.cursor = cursor;
            self.history_index = None;
            return;
        }
        characters.drain(start..end);
        self.set(characters, start);
    }

    /// Walks the draft history. `back` moves towards older entries.
    pub fn walk_history(&mut self, back: bool) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() - 1;
        let next = match (self.history_index, back) {
            (None, true) => Some(last),
            (None, false) => None,
            (Some(0), true) => Some(0),
            (Some(index), true) => Some(index - 1),
            (Some(index), false) if index >= last => None,
            (Some(index), false) => Some(index + 1),
        };
        self.history_index = next;
        self.draft = next.map(|index| self.history[index].clone()).unwrap_or_default();
        self.cursor = self.draft.chars().count();
    }

    /// Takes the draft to send.
    ///
    /// Returns `None` for a draft that is empty or only whitespace: sending one
    /// would spend a turn saying nothing.
    ///
    /// It does **not** record the draft — [`Self::remember`] does, and the caller
    /// is the only thing that knows whether what it took was a message or a
    /// command. Recording here made `/retry` restore `/retry`.
    pub fn submit(&mut self) -> Option<String> {
        if self.draft.trim().is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.draft);
        self.cursor = 0;
        self.history_index = None;
        Some(text)
    }

    /// Records a sent message in the draft history.
    ///
    /// Only messages: a command is already one keystroke away in the palette,
    /// and putting it here would make `↑` and `/retry` offer to run it again.
    pub fn remember(&mut self, text: &str) {
        if self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_string());
        }
    }

    /// Drops the draft without touching the history.
    pub fn clear(&mut self) {
        self.draft.clear();
        self.cursor = 0;
        self.history_index = None;
    }

    /// Puts the caret at a character index, clamped into the draft.
    pub fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.draft.chars().count());
    }

    /// Replaces the draft wholesale, as restoring a transcript row does.
    pub fn set_draft(&mut self, text: impl Into<String>) {
        self.draft = text.into();
        self.cursor = self.draft.chars().count();
        self.history_index = None;
    }

    /// Where the caret sits.
    pub fn caret_line(&self) -> CaretLine {
        let characters = chars(&self.draft);
        let cursor = self.cursor.min(characters.len());
        let mut index = 0;
        let mut start = 0;
        for (at, ch) in characters.iter().enumerate().take(cursor) {
            if *ch == '\n' {
                index += 1;
                start = at + 1;
            }
        }
        CaretLine {
            index,
            count: characters.iter().filter(|ch| **ch == '\n').count() + 1,
            column: cursor - start,
        }
    }

    /// Where the caret sits once the draft is wrapped to `columns`.
    ///
    /// Returns `(row, column)` in cells. This uses the **hard** wrap, not the
    /// word wrap, and that is not an implementation detail: hard wrapping is
    /// greedy per logical line, so wrapping only the text *before* the caret
    /// produces the same rows the whole draft does. Word wrapping does not have
    /// that property — a word pushed to the next row by text that comes *after*
    /// the caret would move the caret without the caret moving.
    pub fn caret_cell(&self, columns: usize) -> (usize, usize) {
        let columns = columns.max(1);
        let prefix: String = self.draft.chars().take(self.cursor).collect();
        let lines: Vec<&str> = prefix.split('\n').collect();
        let mut row = 0;
        for line in &lines[..lines.len() - 1] {
            row += wrap_to_width(line, columns).len();
        }
        let wrapped = wrap_to_width(lines.last().unwrap_or(&""), columns);
        row += wrapped.len() - 1;
        let mut column = display_width(wrapped.last().map(String::as_str).unwrap_or(""));
        // A caret that has just filled a row sits at the start of the next one:
        // that is where the next character will land.
        if column >= columns {
            row += 1;
            column = 0;
        }
        (row, column)
    }

    /// The draft as rows, hard-wrapped to `columns`.
    ///
    /// Paired with [`Self::caret_cell`], which locates the caret in exactly
    /// these rows.
    pub fn rows(&self, columns: usize) -> Vec<String> {
        let columns = columns.max(1);
        self.draft
            .split('\n')
            .flat_map(|line| wrap_to_width(line, columns))
            .collect()
    }

    fn set(&mut self, characters: Vec<char>, cursor: usize) {
        self.cursor = cursor.min(characters.len());
        self.draft = characters.into_iter().collect();
        self.history_index = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composer(draft: &str, cursor: usize) -> Composer {
        Composer {
            draft: draft.into(),
            cursor,
            ..Composer::default()
        }
    }

    #[test]
    fn the_caret_steps_by_character_not_by_byte() {
        let mut c = composer("你好ab", 0);
        c.move_caret(Motion::Right);
        assert_eq!(c.cursor(), 1);
        c.insert("X");
        assert_eq!(c.draft(), "你X好ab");
    }

    #[test]
    fn word_motions_stop_at_word_boundaries() {
        let mut c = composer("alpha beta gamma", 16);
        c.move_caret(Motion::WordLeft);
        assert_eq!(c.cursor(), 11);
        c.move_caret(Motion::WordLeft);
        assert_eq!(c.cursor(), 6);
        c.move_caret(Motion::WordRight);
        assert_eq!(c.cursor(), 11);
    }

    #[test]
    fn line_motions_work_per_logical_line() {
        let mut c = composer("one\ntwo three", 8);
        c.move_caret(Motion::LineStart);
        assert_eq!(c.cursor(), 4);
        c.move_caret(Motion::LineEnd);
        assert_eq!(c.cursor(), 13);
    }

    #[test]
    fn vertical_motion_keeps_the_column_where_it_can() {
        let mut c = composer("alpha\nbe\ngamma", 3);
        c.move_caret(Motion::Down);
        // The middle line is shorter, so the caret clamps to its end.
        assert_eq!(c.caret_line(), CaretLine { index: 1, count: 3, column: 2 });
        c.move_caret(Motion::Down);
        assert_eq!(c.caret_line().index, 2);
        c.move_caret(Motion::Up);
        assert_eq!(c.caret_line().index, 1);
    }

    #[test]
    fn deletions_remove_exactly_their_range() {
        let mut c = composer("alpha beta", 10);
        c.delete(Deletion::BackWord);
        assert_eq!(c.draft(), "alpha ");
        c.delete(Deletion::ToLineStart);
        assert_eq!(c.draft(), "");

        let mut c = composer("one\ntwo", 4);
        c.delete(Deletion::ToLineEnd);
        assert_eq!(c.draft(), "one\n");

        let mut c = composer("ab", 1);
        c.delete(Deletion::ForwardChar);
        assert_eq!(c.draft(), "a");
    }

    #[test]
    fn a_deletion_at_the_edge_is_a_no_op_not_a_panic() {
        let mut c = composer("", 0);
        for kind in [
            Deletion::BackChar,
            Deletion::ForwardChar,
            Deletion::BackWord,
            Deletion::ToLineStart,
            Deletion::ToLineEnd,
        ] {
            c.delete(kind);
            assert_eq!(c.draft(), "");
        }
    }

    #[test]
    fn a_newline_is_inserted_rather_than_sent() {
        let mut c = composer("one", 3);
        c.newline();
        c.insert("two");
        assert_eq!(c.draft(), "one\ntwo");
        assert_eq!(c.caret_line().count, 2);
    }

    #[test]
    fn submitting_clears_the_draft_but_does_not_record_it() {
        // The caller records, because only it knows whether what it took was a
        // message or a command — recording here made `/retry` restore `/retry`.
        let mut c = composer("first", 5);
        assert_eq!(c.submit().as_deref(), Some("first"));
        assert!(c.is_empty());
        assert!(c.history().is_empty());
        c.remember("first");
        assert_eq!(c.history(), ["first".to_string()]);
    }

    #[test]
    fn an_empty_or_blank_draft_is_never_sent() {
        let mut c = composer("   \n ", 0);
        assert_eq!(c.submit(), None);
        assert_eq!(c.draft(), "   \n ", "and the draft is left alone");
    }

    #[test]
    fn the_same_draft_twice_is_one_history_entry() {
        let mut c = Composer::default();
        for _ in 0..2 {
            c.set_draft("same");
            let text = c.submit().unwrap();
            c.remember(&text);
        }
        assert_eq!(c.history().len(), 1);
    }

    #[test]
    fn history_walks_back_and_returns_to_the_live_draft() {
        let mut c = Composer::default();
        for text in ["one", "two"] {
            c.set_draft(text);
            let sent = c.submit().unwrap();
            c.remember(&sent);
        }
        c.walk_history(true);
        assert_eq!(c.draft(), "two");
        c.walk_history(true);
        assert_eq!(c.draft(), "one");
        c.walk_history(true);
        assert_eq!(c.draft(), "one", "the oldest entry is the end of the road");
        c.walk_history(false);
        assert_eq!(c.draft(), "two");
        c.walk_history(false);
        assert_eq!(c.draft(), "", "past the newest is the empty draft again");
    }

    #[test]
    fn typing_leaves_the_history_cursor() {
        let mut c = Composer::default();
        c.set_draft("one");
        let sent = c.submit().unwrap();
        c.remember(&sent);
        c.walk_history(true);
        c.insert("!");
        assert_eq!(c.draft(), "one!");
        // The next `up` starts from the newest entry again rather than stepping
        // off wherever the walk had reached.
        c.walk_history(true);
        assert_eq!(c.draft(), "one");
    }

    #[test]
    fn the_caret_is_locatable_in_the_rows_that_are_drawn() {
        let mut c = composer("alpha beta gamma", 0);
        assert_eq!(c.caret_cell(6), (0, 0));
        c.set_cursor(6);
        assert_eq!(c.rows(6), vec!["alpha ", "beta g", "amma"]);
        assert_eq!(c.caret_cell(6), (1, 0), "a filled row puts the caret on the next");
        c.set_cursor(8);
        assert_eq!(c.caret_cell(6), (1, 2));
        c.set_cursor(16);
        assert_eq!(c.caret_cell(6), (2, 4));
    }

    #[test]
    fn the_caret_counts_cells_not_characters() {
        let mut c = composer("你好ab", 2);
        // Two ideographs are four columns.
        assert_eq!(c.caret_cell(20), (0, 4));
        c.set_cursor(4);
        assert_eq!(c.caret_cell(20), (0, 6));
    }

    #[test]
    fn the_caret_follows_explicit_newlines() {
        let c = composer("one\ntwo", 5);
        assert_eq!(c.rows(20), vec!["one", "two"]);
        assert_eq!(c.caret_cell(20), (1, 1));
    }

    #[test]
    fn an_empty_draft_puts_the_caret_at_the_origin() {
        let c = composer("", 0);
        assert_eq!(c.rows(20), vec![String::new()]);
        assert_eq!(c.caret_cell(20), (0, 0));
    }

    #[test]
    fn the_caret_never_leaves_the_rows_that_were_drawn() {
        let long = "x".repeat(50);
        let drafts = ["", "a", "alpha beta", "你好世界 mixed", "one\ntwo\nthree", &long];
        for draft in drafts {
            let count = draft.chars().count();
            for cursor in 0..=count {
                for columns in 1..12 {
                    let c = composer(draft, cursor);
                    let (row, column) = c.caret_cell(columns);
                    let rows = c.rows(columns);
                    assert!(row <= rows.len(), "{draft:?}@{cursor} cols={columns}");
                    assert!(column <= columns, "{draft:?}@{cursor} cols={columns}");
                }
            }
        }
    }

    #[test]
    fn the_prompt_says_what_the_session_may_do() {
        assert_eq!(prompt_tone("Plan"), RowTone::ModeRestricted);
        assert_eq!(prompt_tone("Accepted"), RowTone::ModeDanger);
        assert_eq!(prompt_tone("Default"), RowTone::User);
        // An unrecognised preset is not guessed at.
        assert_eq!(prompt_tone("something-new"), RowTone::User);
    }
}

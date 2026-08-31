// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/overlay.ts, session-browser.ts,
//           session-selector.ts, transcript-mode.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; one window model serves all four surfaces
//            instead of one file each; the session browser becomes a durable
//            JSONL log browser, which is what this host has instead of sessions.
//! Full-screen surfaces: the shortcut sheet, the palette, the log browser, and
//! the searchable transcript.
//!
//! All four are the same thing — a titled list with a cursor, a window onto it,
//! and a text box — so they are one model with four builders rather than four
//! near-copies. The two behaviours worth naming:
//!
//! - **Typing filters; there is no mode to enter.** The list *is* the search
//!   result, which is why the palette and the browser have no `/` of their own.
//! - **The cursor follows the row, not its number.** After a keystroke narrows
//!   the list, the cursor is still on whatever it was on, or on the nearest row
//!   if that one is gone. A cursor that stayed on index 3 would silently move
//!   onto a different log, and `Enter` would open the wrong one.
//!
//! The transcript screen is the exception that proves the model: its text box
//! *searches* rather than filters, because a transcript with the non-matching
//! rows removed is not a transcript any more.

use crate::text::{display_width, truncate_to_width};
use crate::theme::RowTone;

/// Which surface is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The shortcut sheet.
    Help,
    /// The command palette.
    Palette,
    /// The durable-log browser.
    Browser,
    /// The searchable transcript.
    Transcript,
    /// The staged ChangeSet, before it is committed.
    Diff,
    /// What this session is.
    Status,
}

impl Surface {
    /// Whether the text box filters the list or searches it.
    pub const fn searches(self) -> bool {
        matches!(self, Self::Transcript)
    }

    /// What the surface calls itself.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Help => "keys",
            Self::Palette => "commands",
            Self::Browser => "durable logs",
            Self::Transcript => "transcript",
            Self::Diff => "staged changes",
            Self::Status => "session",
        }
    }

    /// What the footer says the next key does.
    pub const fn hint(self) -> &'static str {
        match self {
            Self::Help => "any key closes",
            Self::Palette => "type to filter · enter to use · esc closes",
            Self::Browser => "type to filter · enter to resume · esc closes",
            Self::Transcript => "/ search · n next · N previous · q closes",
            Self::Diff => "ctrl+s commits · /discard drops · q closes",
            Self::Status => "q closes",
        }
    }
}

/// One row of a surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayRow {
    /// What the row says.
    pub text: String,
    /// What taking the row means. Empty for a row that cannot be taken.
    pub value: String,
    /// How the row is painted.
    pub tone: Option<RowTone>,
}

impl OverlayRow {
    /// A row that can be taken.
    pub fn new(text: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            value: value.into(),
            tone: None,
        }
    }

    /// A heading or separator: shown, never selected.
    pub fn heading(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            value: String::new(),
            tone: Some(RowTone::Heading),
        }
    }

    /// Whether the cursor may land here.
    pub fn selectable(&self) -> bool {
        !self.value.is_empty()
    }
}

/// An open surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    /// Which surface this is.
    pub surface: Surface,
    rows: Vec<OverlayRow>,
    query: String,
    /// The value the cursor is on, so filtering cannot move it onto another row.
    anchor: Option<String>,
    scroll: usize,
}

impl Overlay {
    /// Opens a surface over `rows`.
    pub fn open(surface: Surface, rows: Vec<OverlayRow>) -> Self {
        let anchor = rows
            .iter()
            .find(|row| row.selectable())
            .map(|row| row.value.clone());
        Self {
            surface,
            rows,
            query: String::new(),
            anchor,
            scroll: 0,
        }
    }

    /// The text box contents.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The rows to show: every row for a searching surface, matches for a
    /// filtering one. Headings are kept only while they still have rows under
    /// them, so a filtered list is not mostly section titles.
    pub fn visible_rows(&self) -> Vec<&OverlayRow> {
        if self.surface.searches() || self.query.is_empty() {
            return self.rows.iter().collect();
        }
        let needle = self.query.to_lowercase();
        let mut out: Vec<&OverlayRow> = Vec::new();
        for row in &self.rows {
            if row.selectable() {
                if row.text.to_lowercase().contains(&needle) {
                    out.push(row);
                }
            } else {
                // Drop a heading that ended up with nothing under it.
                while out.last().is_some_and(|last| !last.selectable()) {
                    out.pop();
                }
                out.push(row);
            }
        }
        while out.last().is_some_and(|last| !last.selectable()) {
            out.pop();
        }
        out
    }

    /// Row indices that match the query, for a searching surface.
    pub fn matches(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return Vec::new();
        }
        let needle = self.query.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.text.to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect()
    }

    /// Index of the cursor within [`Self::visible_rows`].
    pub fn selected(&self) -> usize {
        let visible = self.visible_rows();
        self.anchor
            .as_ref()
            .and_then(|anchor| visible.iter().position(|row| &row.value == anchor))
            .or_else(|| visible.iter().position(|row| row.selectable()))
            .unwrap_or(0)
    }

    /// What taking the current row would mean.
    pub fn selected_value(&self) -> Option<String> {
        let visible = self.visible_rows();
        visible
            .get(self.selected())
            .filter(|row| row.selectable())
            .map(|row| row.value.clone())
    }

    /// Moves the cursor by `delta` selectable rows.
    pub fn move_by(&mut self, delta: isize) {
        let visible = self.visible_rows();
        let selectable: Vec<usize> = visible
            .iter()
            .enumerate()
            .filter(|(_, row)| row.selectable())
            .map(|(index, _)| index)
            .collect();
        if selectable.is_empty() {
            return;
        }
        let at = selectable
            .iter()
            .position(|index| *index == self.selected())
            .unwrap_or(0) as isize;
        let next = (at + delta).clamp(0, selectable.len() as isize - 1) as usize;
        self.anchor = visible[selectable[next]].value.clone().into();
    }

    /// Jumps to the `nth` match of a searching surface, wrapping.
    pub fn jump_to_match(&mut self, forward: bool) {
        let matches = self.matches();
        if matches.is_empty() {
            return;
        }
        let current = self.selected();
        let next = if forward {
            matches
                .iter()
                .find(|index| **index > current)
                .copied()
                .unwrap_or(matches[0])
        } else {
            matches
                .iter()
                .rev()
                .find(|index| **index < current)
                .copied()
                .unwrap_or_else(|| *matches.last().expect("non-empty"))
        };
        self.anchor = self.rows.get(next).map(|row| row.value.clone());
        self.scroll = next.saturating_sub(2);
    }

    /// Adds one character to the text box.
    pub fn push_query(&mut self, ch: char) {
        self.query.push(ch);
        self.reanchor();
    }

    /// Removes the last character of the text box.
    ///
    /// Returns false when there was nothing to remove, which is the caller's cue
    /// that `Esc` should close the surface rather than clear it.
    pub fn pop_query(&mut self) -> bool {
        let popped = self.query.pop().is_some();
        if popped {
            self.reanchor();
        }
        popped
    }

    /// Clears the text box.
    pub fn clear_query(&mut self) -> bool {
        let had = !self.query.is_empty();
        self.query.clear();
        self.reanchor();
        had
    }

    /// Keeps the cursor on a row that still exists after a query change.
    fn reanchor(&mut self) {
        if self.surface.searches() {
            return;
        }
        let visible = self.visible_rows();
        let still_here = self
            .anchor
            .as_ref()
            .is_some_and(|anchor| visible.iter().any(|row| &row.value == anchor));
        if !still_here {
            self.anchor = visible
                .iter()
                .find(|row| row.selectable())
                .map(|row| row.value.clone());
        }
    }

    /// The window of rows to draw, and the index of the cursor within it.
    ///
    /// The window follows the cursor with two rows of margin, so moving to the
    /// edge scrolls rather than pinning the cursor to the last line.
    pub fn window(&mut self, height: usize) -> (Vec<OverlayRow>, usize) {
        let height = height.max(1);
        let selected = self.selected();
        let total = self.visible_rows().len();
        if selected < self.scroll + 2 {
            self.scroll = selected.saturating_sub(2);
        }
        if selected + 3 > self.scroll + height {
            self.scroll = selected + 3 - height;
        }
        self.scroll = self.scroll.min(total.saturating_sub(height));
        let rows: Vec<OverlayRow> = self
            .visible_rows()
            .iter()
            .skip(self.scroll)
            .take(height)
            .map(|row| (*row).clone())
            .collect();
        (rows, selected.saturating_sub(self.scroll))
    }

    /// How many rows the surface holds in total.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the surface holds nothing at all.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Lays out a two-column row for a sheet, padding the left column to `width`.
pub fn two_column(left: &str, right: &str, width: usize, columns: usize) -> String {
    let pad = width.saturating_sub(display_width(left));
    truncate_to_width(&format!("{left}{}  {right}", " ".repeat(pad)), columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<OverlayRow> {
        vec![
            OverlayRow::heading("composing"),
            OverlayRow::new("send the draft", "chat:submit"),
            OverlayRow::new("insert a newline", "chat:newline"),
            OverlayRow::heading("reading"),
            OverlayRow::new("scroll up", "scroll:up"),
        ]
    }

    fn palette() -> Overlay {
        Overlay::open(Surface::Palette, rows())
    }

    #[test]
    fn the_cursor_starts_on_the_first_row_it_can_take() {
        let overlay = palette();
        // Not the heading above it.
        assert_eq!(overlay.selected_value().as_deref(), Some("chat:submit"));
    }

    #[test]
    fn typing_filters_and_there_is_no_mode_to_enter() {
        let mut overlay = palette();
        for ch in "scroll".chars() {
            overlay.push_query(ch);
        }
        let visible: Vec<&str> = overlay
            .visible_rows()
            .iter()
            .map(|row| row.text.as_str())
            .collect();
        assert_eq!(visible, ["reading", "scroll up"]);
    }

    #[test]
    fn a_heading_with_nothing_under_it_is_dropped() {
        let mut overlay = palette();
        for ch in "newline".chars() {
            overlay.push_query(ch);
        }
        let visible: Vec<&str> = overlay
            .visible_rows()
            .iter()
            .map(|row| row.text.as_str())
            .collect();
        assert_eq!(visible, ["composing", "insert a newline"]);
    }

    #[test]
    fn the_cursor_follows_the_row_rather_than_its_number() {
        let mut overlay = palette();
        overlay.move_by(1);
        assert_eq!(overlay.selected_value().as_deref(), Some("chat:newline"));
        // Filtering removes the row above it; the cursor must not slide onto a
        // different one just because index 2 now means something else.
        for ch in "line".chars() {
            overlay.push_query(ch);
        }
        assert_eq!(overlay.selected_value().as_deref(), Some("chat:newline"));
    }

    #[test]
    fn a_filter_that_removes_the_cursors_row_moves_it_to_the_first_survivor() {
        let mut overlay = palette();
        overlay.move_by(1);
        for ch in "scroll".chars() {
            overlay.push_query(ch);
        }
        assert_eq!(overlay.selected_value().as_deref(), Some("scroll:up"));
    }

    #[test]
    fn the_cursor_never_leaves_the_list() {
        let mut overlay = palette();
        overlay.move_by(-10);
        assert_eq!(overlay.selected_value().as_deref(), Some("chat:submit"));
        overlay.move_by(10);
        assert_eq!(overlay.selected_value().as_deref(), Some("scroll:up"));
    }

    #[test]
    fn a_search_surface_keeps_every_row_and_navigates_matches() {
        let mut overlay = Overlay::open(
            Surface::Transcript,
            (0..10)
                .map(|index| {
                    OverlayRow::new(
                        if index % 3 == 0 { "hit" } else { "miss" },
                        format!("row-{index}"),
                    )
                })
                .collect(),
        );
        for ch in "hit".chars() {
            overlay.push_query(ch);
        }
        // Nothing is removed: a transcript with rows missing is not a transcript.
        assert_eq!(overlay.visible_rows().len(), 10);
        assert_eq!(overlay.matches(), vec![0, 3, 6, 9]);
        overlay.jump_to_match(true);
        assert_eq!(overlay.selected_value().as_deref(), Some("row-3"));
        overlay.jump_to_match(true);
        assert_eq!(overlay.selected_value().as_deref(), Some("row-6"));
        overlay.jump_to_match(false);
        assert_eq!(overlay.selected_value().as_deref(), Some("row-3"));
    }

    #[test]
    fn match_navigation_wraps_rather_than_stopping() {
        let mut overlay = Overlay::open(
            Surface::Transcript,
            vec![
                OverlayRow::new("hit", "a"),
                OverlayRow::new("miss", "b"),
                OverlayRow::new("hit", "c"),
            ],
        );
        for ch in "hit".chars() {
            overlay.push_query(ch);
        }
        overlay.jump_to_match(true);
        assert_eq!(overlay.selected_value().as_deref(), Some("c"));
        overlay.jump_to_match(true);
        assert_eq!(overlay.selected_value().as_deref(), Some("a"));
    }

    #[test]
    fn backspace_reports_whether_there_was_anything_to_clear() {
        let mut overlay = palette();
        assert!(!overlay.pop_query(), "an empty box means esc should close");
        overlay.push_query('x');
        assert!(overlay.pop_query());
    }

    #[test]
    fn the_window_follows_the_cursor_with_a_margin() {
        let mut overlay = Overlay::open(
            Surface::Palette,
            (0..40)
                .map(|index| OverlayRow::new(format!("row {index}"), format!("v{index}")))
                .collect(),
        );
        for _ in 0..20 {
            overlay.move_by(1);
        }
        let (rows, cursor) = overlay.window(10);
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[cursor].value, "v20");
        // The cursor is not pinned to the last drawn row.
        assert!(cursor + 1 < rows.len());
    }

    #[test]
    fn a_window_taller_than_the_list_shows_all_of_it() {
        let mut overlay = palette();
        let (rows, _) = overlay.window(50);
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn two_column_rows_line_up_and_stay_inside_the_window() {
        let row = two_column("ctrl+o", "fold or unfold", 12, 40);
        assert!(row.starts_with("ctrl+o      "));
        assert!(display_width(&row) <= 40);
        assert!(display_width(&two_column("a", "b", 12, 5)) <= 5);
    }
}

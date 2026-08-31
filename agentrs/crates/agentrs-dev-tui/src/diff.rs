// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/diff-view.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; a deleted file is modelled explicitly;
//            rows carry a tone enum instead of a marker character.
//! Unified-diff view model.
//!
//! The ChangeSet holds before and after text; the terminal needs bounded,
//! line-numbered hunks plus honest add/remove statistics.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/diff-view.ts`.
//!
//! One deviation from the original's role is worth recording. In `dsh` the tool
//! itself declares a diff card, so the diff is part of what the run reported.
//! AgentRS tools report prose (`已写入 N 字节`), and the before/after text lives
//! in the dev adapter's live ChangeSet rather than in any durable event. So a
//! diff here is **live enrichment**: it is computed from the ChangeSet while the
//! session is open, and a `--resume` replay shows the card without it. That is
//! the honest form — inventing diff rows during replay would be the UI claiming
//! to know something the durable log never recorded.

use crate::styling::{DetailLine, StyledSegment};
use crate::theme::RowTone;

/// Unchanged lines kept around each change.
pub const DEFAULT_CONTEXT: usize = 3;
/// Maximum rows retained for one file.
pub const DEFAULT_MAX_ROWS: usize = 200;
/// Above this line-pair product the exact diff is skipped and the file is
/// reported as a whole-file replacement instead of blocking the render loop.
pub const DEFAULT_MAX_CELLS: usize = 250_000;

/// What one diff row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// Unchanged context.
    Context,
    /// Added in the new text.
    Added,
    /// Removed from the old text.
    Removed,
    /// A hunk header or a note about the file as a whole.
    Hunk,
}

impl Marker {
    /// The single character that leads the row.
    pub const fn glyph(self) -> char {
        match self {
            Self::Context => ' ',
            Self::Added => '+',
            Self::Removed => '-',
            Self::Hunk => '@',
        }
    }

    /// The tone the row is painted in.
    ///
    /// Added and removed rows belong to a tool card but must not share its
    /// colour, which is the whole reason a row carries a tone of its own.
    pub const fn tone(self) -> RowTone {
        match self {
            Self::Context => RowTone::Assistant,
            Self::Added => RowTone::DiffAdd,
            Self::Removed => RowTone::DiffRemove,
            Self::Hunk => RowTone::DiffHunk,
        }
    }
}

/// One row of a rendered diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    /// What the row is.
    pub marker: Marker,
    /// The line's text, without the marker.
    pub text: String,
    /// Line number in the old text, when the row has one.
    pub old_line: Option<usize>,
    /// Line number in the new text, when the row has one.
    pub new_line: Option<usize>,
}

/// One file's bounded diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Workspace-relative path.
    pub path: String,
    /// The file could not be diffed as text.
    pub binary: bool,
    /// The file did not exist before.
    pub created: bool,
    /// The file exists before but not after.
    pub deleted: bool,
    /// Lines added.
    pub added: usize,
    /// Lines removed.
    pub removed: usize,
    /// Rows to show, already bounded.
    pub rows: Vec<DiffRow>,
    /// Rows dropped because the file exceeded the row budget.
    pub dropped_rows: usize,
}

impl FileDiff {
    /// The compact `+12 -4` badge, or `None` when nothing changed.
    pub fn badge(&self) -> Option<String> {
        if self.binary {
            return Some("binary".into());
        }
        if self.added == 0 && self.removed == 0 {
            return None;
        }
        Some(format!("+{} -{}", self.added, self.removed))
    }

    /// The rows as styled detail lines, one per row.
    pub fn detail_lines(&self, line_numbers: bool) -> Vec<DetailLine> {
        self.rows
            .iter()
            .map(|row| {
                DetailLine::styled(vec![StyledSegment::toned(
                    row_text(row, line_numbers),
                    row.marker.tone(),
                )])
            })
            .collect()
    }
}

/// One line-level edit operation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Operation {
    Equal { text: String, old: usize, new: usize },
    Remove { text: String, old: usize },
    Add { text: String, new: usize },
}

impl Operation {
    const fn old_line(&self) -> Option<usize> {
        match self {
            Self::Equal { old, .. } | Self::Remove { old, .. } => Some(*old),
            Self::Add { .. } => None,
        }
    }

    const fn new_line(&self) -> Option<usize> {
        match self {
            Self::Equal { new, .. } | Self::Add { new, .. } => Some(*new),
            Self::Remove { .. } => None,
        }
    }

    const fn marker(&self) -> Marker {
        match self {
            Self::Equal { .. } => Marker::Context,
            Self::Add { .. } => Marker::Added,
            Self::Remove { .. } => Marker::Removed,
        }
    }

    fn text(&self) -> &str {
        match self {
            Self::Equal { text, .. } | Self::Remove { text, .. } | Self::Add { text, .. } => text,
        }
    }
}

fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

fn is_binary(text: Option<&str>) -> bool {
    text.is_some_and(|text| text.contains('\0'))
}

/// Longest-common-subsequence line operations.
///
/// Callers must apply the cell guard before reaching this: the table is
/// `before × after` and a large pair of files would stall the render loop.
fn operations(before: &[&str], after: &[&str]) -> Vec<Operation> {
    let rows = before.len();
    let columns = after.len();
    let mut lengths = vec![vec![0usize; columns + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..columns).rev() {
            lengths[i][j] = if before[i] == after[j] {
                lengths[i + 1][j + 1] + 1
            } else {
                lengths[i + 1][j].max(lengths[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < rows && j < columns {
        if before[i] == after[j] {
            out.push(Operation::Equal {
                text: before[i].to_string(),
                old: i + 1,
                new: j + 1,
            });
            i += 1;
            j += 1;
        } else if lengths[i + 1][j] >= lengths[i][j + 1] {
            out.push(Operation::Remove {
                text: before[i].to_string(),
                old: i + 1,
            });
            i += 1;
        } else {
            out.push(Operation::Add {
                text: after[j].to_string(),
                new: j + 1,
            });
            j += 1;
        }
    }
    while i < rows {
        out.push(Operation::Remove {
            text: before[i].to_string(),
            old: i + 1,
        });
        i += 1;
    }
    while j < columns {
        out.push(Operation::Add {
            text: after[j].to_string(),
            new: j + 1,
        });
        j += 1;
    }
    out
}

fn whole_file_replacement(before: &[&str], after: &[&str]) -> Vec<Operation> {
    let mut out: Vec<Operation> = before
        .iter()
        .enumerate()
        .map(|(index, text)| Operation::Remove {
            text: (*text).to_string(),
            old: index + 1,
        })
        .collect();
    out.extend(after.iter().enumerate().map(|(index, text)| Operation::Add {
        text: (*text).to_string(),
        new: index + 1,
    }));
    out
}

/// Keeps changed runs plus `context` unchanged lines, separated by hunk headers.
fn to_rows(ops: &[Operation], context: usize) -> Vec<DiffRow> {
    let mut keep = vec![false; ops.len()];
    for (index, operation) in ops.iter().enumerate() {
        if matches!(operation, Operation::Equal { .. }) {
            continue;
        }
        let start = index.saturating_sub(context);
        let end = (index + context + 1).min(ops.len());
        for slot in &mut keep[start..end] {
            *slot = true;
        }
    }
    let mut rows = Vec::new();
    let mut in_hunk = false;
    for (index, operation) in ops.iter().enumerate() {
        if !keep[index] {
            in_hunk = false;
            continue;
        }
        if !in_hunk {
            rows.push(DiffRow {
                marker: Marker::Hunk,
                text: format!(
                    "@@ -{} +{} @@",
                    operation.old_line().unwrap_or(0),
                    operation.new_line().unwrap_or(0)
                ),
                old_line: None,
                new_line: None,
            });
            in_hunk = true;
        }
        rows.push(DiffRow {
            marker: operation.marker(),
            text: operation.text().to_string(),
            old_line: operation.old_line(),
            new_line: operation.new_line(),
        });
    }
    rows
}

/// Options for [`build_file_diff`].
#[derive(Debug, Clone, Copy)]
pub struct DiffOptions {
    /// Unchanged lines kept around each change.
    pub context: usize,
    /// Maximum rows retained for one file.
    pub max_rows: usize,
    /// Cell ceiling above which the exact diff is skipped.
    pub max_cells: usize,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            context: DEFAULT_CONTEXT,
            max_rows: DEFAULT_MAX_ROWS,
            max_cells: DEFAULT_MAX_CELLS,
        }
    }
}

/// Builds one file's bounded diff view.
///
/// `before` is `None` for a file that did not exist, and `after` is `None` for
/// one that has been deleted.
pub fn build_file_diff(
    path: &str,
    before: Option<&str>,
    after: Option<&str>,
    options: DiffOptions,
) -> FileDiff {
    let created = before.is_none();
    let deleted = after.is_none();
    if is_binary(before) || is_binary(after) {
        return FileDiff {
            path: path.to_string(),
            binary: true,
            created,
            deleted,
            added: 0,
            removed: 0,
            rows: vec![DiffRow {
                marker: Marker::Hunk,
                text: "binary file not shown".into(),
                old_line: None,
                new_line: None,
            }],
            dropped_rows: 0,
        };
    }
    let before_text = before.unwrap_or("");
    let after_text = after.unwrap_or("");
    let before_lines = split_lines(before_text);
    let after_lines = split_lines(after_text);
    let ops = if before_lines.len().saturating_mul(after_lines.len()) > options.max_cells {
        whole_file_replacement(&before_lines, &after_lines)
    } else {
        operations(&before_lines, &after_lines)
    };
    let added = ops
        .iter()
        .filter(|op| matches!(op, Operation::Add { .. }))
        .count();
    let removed = ops
        .iter()
        .filter(|op| matches!(op, Operation::Remove { .. }))
        .count();
    let mut rows = to_rows(&ops, options.context);
    let dropped_rows = rows.len().saturating_sub(options.max_rows);
    rows.truncate(options.max_rows);
    FileDiff {
        path: path.to_string(),
        binary: false,
        created,
        deleted,
        added,
        removed,
        rows,
        dropped_rows,
    }
}

/// Renders one row as terminal text, prefixed with its marker and line number.
pub fn row_text(row: &DiffRow, line_numbers: bool) -> String {
    if row.marker == Marker::Hunk {
        return row.text.clone();
    }
    if !line_numbers {
        return format!("{}{}", row.marker.glyph(), row.text);
    }
    let line = match row.marker {
        Marker::Removed => row.old_line,
        _ => row.new_line,
    };
    let number = line.map(|value| value.to_string()).unwrap_or_default();
    format!("{}{number:>5} {}", row.marker.glyph(), row.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(before: Option<&str>, after: Option<&str>) -> FileDiff {
        build_file_diff("f.md", before, after, DiffOptions::default())
    }

    #[test]
    fn statistics_count_lines_not_rows() {
        let view = diff(Some("a\nb\nc\n"), Some("a\nB\nc\n"));
        assert_eq!((view.added, view.removed), (1, 1));
        assert_eq!(view.badge().as_deref(), Some("+1 -1"));
    }

    #[test]
    fn an_unchanged_file_has_no_rows_and_no_badge() {
        let view = diff(Some("a\nb\n"), Some("a\nb\n"));
        assert!(view.rows.is_empty());
        assert_eq!(view.badge(), None);
    }

    #[test]
    fn context_bounds_what_is_shown() {
        let before = (1..=20).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let after = before.replace("\n10\n", "\nTEN\n");
        let view = build_file_diff("f", Some(&before), Some(&after), DiffOptions::default());
        // One hunk header, three lines of context either side, plus the change.
        assert_eq!(view.rows[0].marker, Marker::Hunk);
        assert_eq!(view.rows.len(), 1 + 3 + 2 + 3);
    }

    #[test]
    fn a_created_file_is_all_additions() {
        let view = diff(None, Some("one\ntwo\n"));
        assert!(view.created);
        assert_eq!((view.added, view.removed), (2, 0));
    }

    #[test]
    fn a_deleted_file_is_all_removals() {
        let view = diff(Some("one\ntwo\n"), None);
        assert!(view.deleted);
        assert_eq!((view.added, view.removed), (0, 2));
    }

    #[test]
    fn binary_content_is_reported_rather_than_rendered() {
        let view = diff(Some("a\0b"), Some("c"));
        assert!(view.binary);
        assert_eq!(view.badge().as_deref(), Some("binary"));
        assert_eq!(view.rows.len(), 1);
    }

    #[test]
    fn the_row_budget_reports_what_it_dropped() {
        let before = String::new();
        let after = (1..=100).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let options = DiffOptions {
            max_rows: 10,
            ..DiffOptions::default()
        };
        let view = build_file_diff("f", Some(&before), Some(&after), options);
        assert_eq!(view.rows.len(), 10);
        // Silently truncating would read as "that is the whole change".
        assert!(view.dropped_rows > 0);
    }

    #[test]
    fn the_cell_guard_falls_back_to_a_whole_file_replacement() {
        let before = (1..=40).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let after = (41..=80).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let options = DiffOptions {
            max_cells: 100,
            ..DiffOptions::default()
        };
        let view = build_file_diff("f", Some(&before), Some(&after), options);
        assert_eq!((view.added, view.removed), (40, 40));
    }

    #[test]
    fn rows_carry_their_own_tone_not_the_cards() {
        let view = diff(Some("a\n"), Some("b\n"));
        let tones: Vec<RowTone> = view.rows.iter().map(|row| row.marker.tone()).collect();
        assert!(tones.contains(&RowTone::DiffAdd));
        assert!(tones.contains(&RowTone::DiffRemove));
        assert!(tones.contains(&RowTone::DiffHunk));
    }

    #[test]
    fn line_numbers_come_from_the_side_the_row_belongs_to() {
        let view = diff(Some("a\nb\n"), Some("a\nB\n"));
        let removed = view
            .rows
            .iter()
            .find(|row| row.marker == Marker::Removed)
            .expect("a removal");
        assert_eq!(row_text(removed, true), "-    2 b");
        assert_eq!(row_text(removed, false), "-b");
    }
}

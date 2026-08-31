// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/terminal-text.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; character width comes from the
//            unicode-width crate rather than the original's hand-rolled range
//            tables.
//! Terminal text safety, width, and wrapping.
//!
//! Model output, tool output, and repository content are untrusted: only the TUI
//! itself may emit styling sequences. Ported from `dsh-code-agent`'s
//! `packages/dsh-tui/src/terminal-text.ts`, with one deliberate substitution —
//! character width comes from the `unicode-width` crate rather than the hand
//! written range tables of the original. The tables were an approximation of the
//! same Unicode data; a crate that tracks it is strictly more faithful, and the
//! two agree on every case the original's tests pin down.

use unicode_width::UnicodeWidthChar;

/// A tab is rendered as this many spaces before anything measures the row.
const TAB_WIDTH: usize = 4;

/// Invisible marks that can hide or reorder text without occupying a cell.
///
/// Zero-width joiner (`U+200D`) is deliberately absent: it occupies no column
/// but it is load bearing inside emoji sequences, so it is kept and simply
/// measured as zero.
const SPOOFING: &[(char, char)] = &[
    ('\u{200b}', '\u{200c}'),
    ('\u{200e}', '\u{200f}'),
    ('\u{202a}', '\u{202e}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{2069}'),
    ('\u{feff}', '\u{feff}'),
];

fn is_spoofing(ch: char) -> bool {
    SPOOFING.iter().any(|(start, end)| ch >= *start && ch <= *end)
}

/// Columns occupied by one character. Controls and combining marks occupy none.
pub fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Terminal columns needed by an already-sanitized string.
pub fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// Index just past the escape sequence starting at `start`.
///
/// The sequence is consumed as a unit rather than having its introducer dropped
/// and its body left behind: `ESC [ 3 1 m` with only the `ESC` removed prints
/// `[31m`, which reads as corruption and still tells the reader nothing.
fn end_of_escape(chars: &[char], start: usize) -> usize {
    let Some(next) = chars.get(start + 1) else {
        return start + 1;
    };
    match next {
        // CSI: parameter bytes, then intermediate bytes, then one final byte.
        '[' => {
            let mut index = start + 2;
            while chars
                .get(index)
                .is_some_and(|ch| ('\u{30}'..='\u{3f}').contains(ch))
            {
                index += 1;
            }
            while chars
                .get(index)
                .is_some_and(|ch| ('\u{20}'..='\u{2f}').contains(ch))
            {
                index += 1;
            }
            (index + 1).min(chars.len())
        }
        // OSC, DCS, PM, APC, SOS: terminated by BEL or by ST (ESC \).
        ']' | 'P' | '^' | '_' | 'X' => {
            let mut index = start + 2;
            while index < chars.len() {
                if chars[index] == '\u{7}' {
                    return index + 1;
                }
                if chars[index] == '\u{1b}' && chars.get(index + 1) == Some(&'\\') {
                    return index + 2;
                }
                index += 1;
            }
            chars.len()
        }
        _ => start + 2,
    }
}

/// True for characters removed outright rather than rendered.
fn dropped(ch: char) -> bool {
    if ch == '\n' {
        return false;
    }
    if ch.is_control() {
        return true;
    }
    is_spoofing(ch)
}

/// Removes escape sequences, control characters, and invisible spoofing marks.
///
/// Tabs become spaces and every flavour of carriage return becomes a newline:
/// a bare `\r` would otherwise let tool output overwrite a row that has already
/// been drawn.
pub fn sanitize_text(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        match ch {
            '\u{1b}' => {
                index = end_of_escape(&chars, index);
                continue;
            }
            '\t' => out.push_str(&" ".repeat(TAB_WIDTH)),
            '\r' => {
                if chars.get(index + 1) == Some(&'\n') {
                    index += 1;
                }
                out.push('\n');
            }
            _ if dropped(ch) => {}
            _ => out.push(ch),
        }
        index += 1;
    }
    out
}

/// Sanitizes and collapses to a single terminal row.
pub fn sanitize_line(text: &str) -> String {
    sanitize_text(text).replace('\n', " ")
}

/// Cuts a sanitized string to `columns`, never splitting a wide cell in half.
///
/// An ellipsis takes the last column when there is room for one, so a cut row
/// says it was cut.
pub fn truncate_to_width(text: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    if display_width(text) <= columns {
        return text.to_string();
    }
    let budget = if columns > 1 { columns - 1 } else { columns };
    let mut width = 0;
    let mut cut = String::new();
    for ch in text.chars() {
        let next = char_width(ch);
        if width + next > budget {
            break;
        }
        width += next;
        cut.push(ch);
    }
    if columns > 1 {
        cut.push('…');
    }
    cut
}

/// Hard-wraps a sanitized string into rows of at most `columns` cells.
///
/// Greedy and per logical line, which is what lets the composer locate its caret
/// by wrapping only the text before it: the prefix produces the same rows the
/// whole draft does. [`wrap_words`] deliberately does not have that property, so
/// the two must not be swapped for one another.
pub fn wrap_to_width(text: &str, columns: usize) -> Vec<String> {
    if columns == 0 {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut width = 0;
        let mut row = String::new();
        for ch in line.chars() {
            let next = char_width(ch);
            // A cell wider than the whole row can never be rendered; standing in
            // for it keeps every row inside the budget instead of overflowing.
            let (cell, cell_width) = if next > columns { ('…', 1) } else { (ch, next) };
            if width + cell_width > columns {
                rows.push(std::mem::take(&mut row));
                width = 0;
            }
            width += cell_width;
            row.push(cell);
        }
        rows.push(row);
    }
    rows
}

/// One wrap chunk: a run of spaces, one wide cell, or a run of narrow non-spaces.
///
/// Wide cells stand alone because CJK text has no spaces to break at, and
/// refusing to break between two ideographs would push every long sentence into
/// one hard cut.
fn wrap_chunks(line: &str) -> Vec<String> {
    #[derive(PartialEq, Eq, Clone, Copy)]
    enum Kind {
        Space,
        Word,
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_kind: Option<Kind> = None;
    for ch in line.chars() {
        if char_width(ch) > 1 {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            current_kind = None;
            chunks.push(ch.to_string());
            continue;
        }
        let kind = if ch.is_whitespace() { Kind::Space } else { Kind::Word };
        if current_kind != Some(kind) && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        current_kind = Some(kind);
        current.push(ch);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Wraps a sanitized string at word boundaries.
///
/// A single token wider than the row falls back to a hard break. Trailing spaces
/// at a break are dropped: they are the break itself, and keeping them shows as
/// a ragged right edge or pushes the row over budget.
pub fn wrap_words(text: &str, columns: usize) -> Vec<String> {
    if columns == 0 {
        return vec![String::new()];
    }
    let mut rows: Vec<String> = Vec::new();
    let mut push = |row: &str| rows.push(row.trim_end().to_string());
    for line in text.split('\n') {
        let mut row = String::new();
        let mut width = 0;
        for chunk in wrap_chunks(line) {
            let chunk_width = display_width(&chunk);
            if width + chunk_width <= columns {
                row.push_str(&chunk);
                width += chunk_width;
                continue;
            }
            // Whitespace never starts a row: the break itself is the separator.
            if chunk.chars().all(char::is_whitespace) {
                push(&row);
                row.clear();
                width = 0;
                continue;
            }
            if chunk_width <= columns {
                push(&row);
                row = chunk;
                width = chunk_width;
                continue;
            }
            // A token too wide for any row is cut, reusing the hard-wrap rules so
            // an oversized single cell is still replaced rather than overflowing.
            if width > 0 {
                push(&row);
                row = String::new();
            }
            let pieces = wrap_to_width(&chunk, columns);
            let last = pieces.len() - 1;
            for piece in &pieces[..last] {
                push(piece);
            }
            row.clone_from(&pieces[last]);
            width = display_width(&row);
        }
        push(&row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_are_removed_whole() {
        let hostile = "ok\u{1b}[31mRED\u{1b}[0m\u{7}\nnext\u{1b}]0;owned\u{7}tail";
        assert_eq!(sanitize_text(hostile), "okRED\nnexttail");
    }

    #[test]
    fn unterminated_osc_swallows_the_rest() {
        assert_eq!(sanitize_text("a\u{1b}]0;no terminator"), "a");
    }

    #[test]
    fn tabs_become_spaces_and_carriage_returns_become_newlines() {
        assert_eq!(sanitize_text("a\tb"), "a    b");
        assert_eq!(sanitize_text("a\r\nb\rc"), "a\nb\nc");
    }

    #[test]
    fn spoofing_marks_are_dropped_but_joiners_survive() {
        assert_eq!(sanitize_text("a\u{202e}b\u{feff}"), "ab");
        assert_eq!(sanitize_text("a\u{200d}b"), "a\u{200d}b");
    }

    #[test]
    fn sanitize_line_collapses_rows() {
        assert_eq!(sanitize_line("one\ntwo"), "one two");
    }

    #[test]
    fn cjk_is_two_columns_and_joiners_are_none() {
        assert_eq!(display_width("你好"), 4);
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("\u{200d}"), 0);
    }

    #[test]
    fn truncate_never_splits_a_wide_cell() {
        // Budget 4 leaves 3 columns before the ellipsis, so only one ideograph
        // fits: half a cell is not a thing a terminal can draw.
        assert_eq!(truncate_to_width("你好世界", 4), "你…");
        assert_eq!(truncate_to_width("abc", 10), "abc");
        assert_eq!(truncate_to_width("abc", 0), "");
    }

    #[test]
    fn hard_wrap_keeps_every_row_inside_the_budget() {
        for text in ["你好世界你好", "abcdefghij", "a b c d e f"] {
            for columns in 1..12 {
                for row in wrap_to_width(text, columns) {
                    assert!(display_width(&row) <= columns, "{text:?} at {columns}");
                }
            }
        }
    }

    #[test]
    fn hard_wrap_stands_in_for_a_cell_wider_than_the_row() {
        assert_eq!(wrap_to_width("你", 1), vec!["…".to_string()]);
    }

    #[test]
    fn word_wrap_breaks_at_spaces_and_drops_them() {
        assert_eq!(
            wrap_words("alpha beta gamma", 11),
            vec!["alpha beta".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn word_wrap_breaks_cjk_between_characters() {
        assert_eq!(
            wrap_words("你好世界", 4),
            vec!["你好".to_string(), "世界".to_string()]
        );
    }

    #[test]
    fn word_wrap_hard_breaks_an_oversized_token() {
        assert_eq!(
            wrap_words("ab supercalifragilistic", 6),
            vec![
                "ab".to_string(),
                "superc".to_string(),
                "alifra".to_string(),
                "gilist".to_string(),
                "ic".to_string(),
            ]
        );
    }

    #[test]
    fn word_wrap_keeps_every_row_inside_the_budget() {
        let samples = [
            "alpha beta gamma delta",
            "你好世界 mixed 文本 with spaces",
            "one",
            "",
            "a\nbb\nccc",
        ];
        for text in samples {
            for columns in 1..20 {
                for row in wrap_words(text, columns) {
                    assert!(display_width(&row) <= columns, "{text:?} at {columns}");
                }
            }
        }
    }
}

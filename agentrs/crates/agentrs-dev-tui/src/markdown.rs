// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/markdown.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; the original's regular expressions are
//            hand rolled for the seven fixed shapes rather than pulling in a
//            regex engine; precedence and output are unchanged.
//! Line-oriented markdown rendering for streamed assistant text.
//!
//! This is deliberately not a parser. Assistant text arrives a token at a time
//! and is re-rendered on every delta, so the cost has to be linear in the text
//! and independent of how much of a construct has arrived: a line is rendered
//! from what it looks like right now, and an unterminated fence or `**` simply
//! stays plain until it closes.
//!
//! The markup is consumed rather than shown: `**read**` is painted bold and the
//! asterisks are gone. Two invariants make that safe.
//!
//! - **One output line per source line.** [`crate::transcript`] takes the first
//!   line as the entry header and maps the rest onto detail rows by index, and
//!   the fold count is the number of detail rows — a line the renderer added or
//!   dropped would shift both. A table's separator row therefore becomes a rule,
//!   not nothing.
//! - **Within a line, the segments sum to its text.** That is what
//!   [`crate::styling::wrap_segments`] walks when a row is too wide.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/markdown.ts`. The
//! original's regular expressions are hand rolled here rather than pulling in a
//! regex engine for seven fixed shapes; the shapes and their precedence are
//! unchanged, and the ported tests pin them down.

use crate::glyphs::GlyphSet;
use crate::styling::{segment_text, StyledSegment};
use crate::text::display_width;
use crate::theme::RowTone;

/// Above this, rendering is skipped: no message this long is worth the scan.
const MAX_SOURCE_LENGTH: usize = 20_000;
/// Only this much of the text is probed for the fast path.
const PROBE_LENGTH: usize = 500;
/// A table wider or taller than this is left as the model wrote it.
const MAX_TABLE_WIDTH: usize = 400;
const MAX_TABLE_ROWS: usize = 200;
/// Width of a thematic break, which has no terminal width to measure against.
const RULE_WIDTH: usize = 24;
/// Nesting depth for inline runs: enough for `**bold `code`**`, no more.
const MAX_INLINE_DEPTH: usize = 2;

/// One rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownLine {
    /// What to show, and what the row is wrapped by.
    pub text: String,
    /// Styled runs; concatenating their text yields `text` exactly.
    pub segments: Vec<StyledSegment>,
}

/// True when the text contains nothing this renderer would touch.
pub fn is_plain_text(source: &str) -> bool {
    let probe: String = source.chars().take(PROBE_LENGTH).collect();
    for line in probe.split('\n') {
        if line.contains(['`', '*', '_', '#', '>', '~', '|']) {
            return false;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") || trimmed.starts_with("+ ") {
            return false;
        }
        if line.trim().len() >= 3 && line.trim().chars().all(|ch| ch == '-') {
            return false;
        }
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && trimmed[digits..].starts_with(". ") {
            return false;
        }
    }
    true
}

fn line_of(segments: Vec<StyledSegment>) -> MarkdownLine {
    let kept: Vec<StyledSegment> = segments
        .into_iter()
        .filter(|segment| !segment.text.is_empty())
        .collect();
    let text = segment_text(&kept);
    MarkdownLine {
        text,
        segments: if kept.is_empty() {
            vec![StyledSegment::plain("")]
        } else {
            kept
        },
    }
}

/// Applies one style to every run of an already-styled span.
fn restyle(segments: Vec<StyledSegment>, apply: impl Fn(&mut StyledSegment)) -> Vec<StyledSegment> {
    segments
        .into_iter()
        .map(|mut segment| {
            apply(&mut segment);
            segment
        })
        .collect()
}

/// Gives runs a tone unless they already carry one of their own.
fn toned(segments: Vec<StyledSegment>, tone: RowTone) -> Vec<StyledSegment> {
    segments
        .into_iter()
        .map(|mut segment| {
            if segment.tone.is_none() {
                segment.tone = Some(tone);
            }
            segment
        })
        .collect()
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn slice(chars: &[char], range: std::ops::Range<usize>) -> String {
    chars[range].iter().collect()
}

/// One matched inline construct: how many characters it consumed, and its runs.
struct InlineMatch {
    consumed: usize,
    segments: Vec<StyledSegment>,
}

/// Finds `delimiter` (one or two identical characters) starting at or after `from`.
fn find_delimiter(chars: &[char], from: usize, delimiter: char, doubled: bool) -> Option<usize> {
    let mut index = from;
    while index < chars.len() {
        if chars[index] == delimiter && (!doubled || chars.get(index + 1) == Some(&delimiter)) {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// Matches one inline construct at the head of `chars`, in precedence order.
///
/// Every construct requires its closing delimiter on the same line: mid-stream,
/// a lone backtick is far more likely to be a construct that has not finished
/// arriving than an error, and half-styling it would make the row twitch as the
/// rest lands. Code comes first so `` `**not bold**` `` stays literal.
fn match_inline(chars: &[char], depth: usize) -> Option<InlineMatch> {
    // `code`
    if chars[0] == '`' {
        if let Some(end) = find_delimiter(chars, 1, '`', false) {
            if end > 1 {
                return Some(InlineMatch {
                    consumed: end + 1,
                    segments: vec![StyledSegment::toned(slice(chars, 1..end), RowTone::Code)],
                });
            }
        }
    }
    // **bold**, __bold__, ~~strike~~
    for (delimiter, bold) in [('*', true), ('_', true), ('~', false)] {
        if chars[0] == delimiter && chars.get(1) == Some(&delimiter) {
            let opens = chars.get(2).is_some_and(|ch| !ch.is_whitespace());
            if opens {
                if let Some(end) = find_delimiter(chars, 3, delimiter, true) {
                    let inner = inline_runs(&chars[2..end], depth + 1);
                    let styled = restyle(inner, |segment| {
                        if bold {
                            segment.bold = true;
                        } else {
                            segment.strikethrough = true;
                        }
                    });
                    return Some(InlineMatch {
                        consumed: end + 2,
                        segments: styled,
                    });
                }
            }
        }
    }
    // *italic* and _italic_, after the doubled forms so `**` never matches as
    // two italics.
    for delimiter in ['*', '_'] {
        if chars[0] != delimiter {
            continue;
        }
        let opens = chars
            .get(1)
            .is_some_and(|ch| !ch.is_whitespace() && *ch != delimiter);
        if !opens {
            continue;
        }
        let mut end = 1;
        while end < chars.len() && chars[end] != delimiter && chars[end] != '\n' {
            end += 1;
        }
        if end <= 1 || chars.get(end) != Some(&delimiter) {
            continue;
        }
        let after = chars.get(end + 1).copied();
        let closes = match delimiter {
            // `read_image` and `str_replace_editor` are the common case, and
            // they are not italic.
            '_' => after.is_none_or(|ch| !is_word(ch)),
            _ => after != Some('*'),
        };
        if !closes {
            continue;
        }
        let inner = inline_runs(&chars[1..end], depth + 1);
        return Some(InlineMatch {
            consumed: end + 1,
            segments: restyle(inner, |segment| segment.italic = true),
        });
    }
    // [label](url) — the label, then the target dimmed. The target is the part a
    // terminal cannot make clickable everywhere, and hiding it would lose the
    // only copyable form of it.
    if chars[0] == '[' {
        let mut close = 1;
        while close < chars.len() && chars[close] != ']' && chars[close] != '\n' {
            close += 1;
        }
        if chars.get(close) == Some(&']') && chars.get(close + 1) == Some(&'(') {
            let mut end = close + 2;
            while end < chars.len() && !chars[end].is_whitespace() && chars[end] != ')' {
                end += 1;
            }
            if chars.get(end) == Some(&')') && end > close + 2 {
                let mut segments = inline_runs(&chars[1..close], depth + 1);
                segments.push(StyledSegment::toned(
                    format!(" ({})", slice(chars, close + 2..end)),
                    RowTone::Quote,
                ));
                return Some(InlineMatch {
                    consumed: end + 1,
                    segments,
                });
            }
        }
    }
    None
}

fn inline_runs(chars: &[char], depth: usize) -> Vec<StyledSegment> {
    let mut segments: Vec<StyledSegment> = Vec::new();
    let mut plain = String::new();
    let mut index = 0;
    while index < chars.len() {
        let rest = &chars[index..];
        let blocked_underscore =
            rest[0] == '_' && index > 0 && is_word(chars[index - 1]);
        let matched = if depth < MAX_INLINE_DEPTH && !blocked_underscore {
            match_inline(rest, depth)
        } else {
            None
        };
        if let Some(found) = matched {
            if !plain.is_empty() {
                segments.push(StyledSegment::plain(std::mem::take(&mut plain)));
            }
            segments.extend(found.segments);
            index += found.consumed;
            continue;
        }
        plain.push(rest[0]);
        index += 1;
    }
    if !plain.is_empty() {
        segments.push(StyledSegment::plain(plain));
    }
    if segments.is_empty() {
        segments.push(StyledSegment::plain(""));
    }
    segments
}

/// Renders the inline runs of one line, consuming the delimiters.
///
/// Unmatched delimiters are left as literal text.
pub fn style_inline(source: &str) -> Vec<StyledSegment> {
    let chars: Vec<char> = source.chars().collect();
    inline_runs(&chars, 0)
}

/// Column alignment declared by a table's separator row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Right,
    Center,
}

fn align_of(spec: &str) -> Align {
    let cell = spec.trim();
    if cell.starts_with(':') && cell.ends_with(':') && cell.len() > 1 {
        Align::Center
    } else if cell.ends_with(':') {
        Align::Right
    } else {
        Align::Left
    }
}

/// Splits the padding a cell needs into a left and a right run.
///
/// The width is a *display* width, not a length: the tables the model writes are
/// mostly Chinese, and a CJK character occupies two terminal columns.
fn padding(text: &str, width: usize, align: Align) -> (String, String) {
    let slack = width.saturating_sub(display_width(text));
    let left = match align {
        Align::Right => slack,
        Align::Center => slack / 2,
        Align::Left => 0,
    };
    (" ".repeat(left), " ".repeat(slack - left))
}

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 2 && trimmed.starts_with('|') && trimmed.ends_with('|')
}

fn is_table_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains('-')
        && trimmed
            .chars()
            .all(|ch| matches!(ch, ' ' | ':' | '|' | '-'))
}

/// Splits a table row into cells, dropping the leading and trailing pipes.
fn table_cells(row: &str) -> Vec<String> {
    let trimmed = row.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    trimmed.split('|').map(|cell| cell.trim().to_string()).collect()
}

/// Renders a pipe table as aligned columns.
///
/// Column widths come from [`display_width`], not byte length. The terminal
/// width is not known here — entries are built once and wrapped per width later
/// — so the table is laid out to its content, and a table too wide for any
/// window falls back to the ordinary wrap.
///
/// Returns `None` when the block is not worth laying out, in which case the
/// caller renders the rows as ordinary text.
fn render_table(rows: &[&str], glyphs: &GlyphSet) -> Option<Vec<MarkdownLine>> {
    if rows.len() > MAX_TABLE_ROWS {
        return None;
    }
    let cells: Vec<Vec<String>> = rows.iter().map(|row| table_cells(row)).collect();
    let aligns: Vec<Align> = cells
        .get(1)
        .map(|row| row.iter().map(|cell| align_of(cell)).collect())
        .unwrap_or_default();
    let columns = cells.iter().map(Vec::len).max().unwrap_or(0);
    // Every cell is styled first: the width that matters is the rendered one,
    // after `**` and backticks are gone.
    let styled: Vec<Vec<Vec<StyledSegment>>> = cells
        .iter()
        .enumerate()
        .map(|(index, row)| {
            if index == 1 {
                Vec::new()
            } else {
                row.iter()
                    .map(|cell| {
                        let runs = style_inline(cell);
                        if index == 0 {
                            restyle(runs, |segment| segment.bold = true)
                        } else {
                            runs
                        }
                    })
                    .collect()
            }
        })
        .collect();
    let mut widths = vec![0usize; columns];
    for (index, row) in styled.iter().enumerate() {
        if index == 1 {
            continue;
        }
        for (column, runs) in row.iter().enumerate() {
            widths[column] = widths[column].max(display_width(&segment_text(runs)));
        }
    }
    let total: usize = widths.iter().sum::<usize>() + columns.saturating_sub(1) * 3;
    if total > MAX_TABLE_WIDTH {
        return None;
    }

    let out = rows
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index == 1 {
                // The separator keeps its row so the line count matches the
                // source, and becomes the rule under the header rather than a
                // row of dashes. The joint is three cells wide, exactly like the
                // separator it sits under.
                let joint = format!("{0}{1}{0}", glyphs.table_row, glyphs.table_cross);
                let rule = widths
                    .iter()
                    .map(|width| glyphs.table_row.repeat(*width))
                    .collect::<Vec<_>>()
                    .join(&joint);
                return line_of(vec![StyledSegment::toned(rule, RowTone::Quote).dimmed()]);
            }
            let empty = Vec::new();
            let row = styled.get(index).unwrap_or(&empty);
            let mut segments: Vec<StyledSegment> = Vec::new();
            for (column, width) in widths.iter().enumerate() {
                let fallback = vec![StyledSegment::plain("")];
                let runs = row.get(column).unwrap_or(&fallback);
                let last = column + 1 == columns;
                let (left, right) = padding(
                    &segment_text(runs),
                    *width,
                    aligns.get(column).copied().unwrap_or(Align::Left),
                );
                // The padding is emitted around the styled runs rather than
                // inside them, so each run stays exactly what the cell said.
                if !left.is_empty() {
                    segments.push(StyledSegment::plain(left));
                }
                segments.extend(runs.iter().cloned());
                // A trailing pad on the last column would be invisible
                // whitespace at the end of every row.
                if !last {
                    if !right.is_empty() {
                        segments.push(StyledSegment::plain(right));
                    }
                    segments.push(
                        StyledSegment::toned(
                            format!(" {} ", glyphs.table_column),
                            RowTone::Quote,
                        )
                        .dimmed(),
                    );
                }
            }
            line_of(segments)
        })
        .collect();
    Some(out)
}

/// How many lines the table block starting at `start` spans, or zero.
fn table_at(lines: &[&str], start: usize) -> usize {
    if !is_table_row(lines[start]) {
        return 0;
    }
    // Without the separator the block is not a table yet — mid-stream that is
    // the ordinary case, and guessing would make the first row jump when the
    // separator arrives.
    let Some(rule) = lines.get(start + 1) else {
        return 0;
    };
    if !is_table_row(rule) || !is_table_rule(rule) {
        return 0;
    }
    let mut end = start + 2;
    while end < lines.len() && is_table_row(lines[end]) {
        end += 1;
    }
    end - start
}

/// Splits a leading run of spaces from the rest of the line.
fn split_indent(source: &str) -> (&str, &str) {
    let indent = source.len() - source.trim_start_matches(' ').len();
    source.split_at(indent)
}

fn render_block_line(source: &str, glyphs: &GlyphSet) -> MarkdownLine {
    // Heading: the hashes go; the weight and the colour say the same thing.
    let hashes = source.chars().take_while(|ch| *ch == '#').count();
    if (1..=6).contains(&hashes) && source[hashes..].starts_with(' ') {
        let body = source[hashes..].trim_start_matches(' ');
        return line_of(toned(
            restyle(style_inline(body), |segment| segment.bold = true),
            RowTone::Heading,
        ));
    }
    let (indent, rest) = split_indent(source);
    if let Some(body) = rest.strip_prefix('>') {
        let body = body.strip_prefix(' ').unwrap_or(body);
        let mut segments = vec![StyledSegment::toned(
            format!("{indent}{} ", glyphs.quote_bar),
            RowTone::Quote,
        )];
        segments.extend(toned(style_inline(body), RowTone::Quote));
        return line_of(segments);
    }
    // A thematic break has no terminal width to measure against, so it takes a
    // fixed one rather than guessing at the window.
    let trimmed = source.trim();
    if trimmed.len() >= 3
        && ['-', '*', '_']
            .iter()
            .any(|marker| trimmed.chars().all(|ch| ch == *marker))
    {
        return line_of(vec![StyledSegment::toned(
            glyphs.table_row.repeat(RULE_WIDTH),
            RowTone::Quote,
        )
        .dimmed()]);
    }
    if let Some(body) = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("+ "))
        .or_else(|| rest.strip_prefix("* "))
    {
        let mut segments = vec![StyledSegment::toned(
            format!("{indent}{} ", glyphs.list_bullet),
            RowTone::Bullet,
        )];
        segments.extend(style_inline(body.trim_start_matches(' ')));
        return line_of(segments);
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && rest[digits..].starts_with(". ") {
        let marker = format!("{indent}{}. ", &rest[..digits]);
        let mut segments = vec![StyledSegment::toned(marker, RowTone::Bullet)];
        segments.extend(style_inline(&rest[digits + 2..]));
        return line_of(segments);
    }
    line_of(style_inline(source))
}

/// Renders a block of assistant text, one output line per input line.
pub fn render_markdown(source: &str, glyphs: &GlyphSet) -> Vec<MarkdownLine> {
    let lines: Vec<&str> = source.split('\n').collect();
    if source.len() > MAX_SOURCE_LENGTH || is_plain_text(source) {
        return lines
            .into_iter()
            .map(|text| MarkdownLine {
                text: text.to_string(),
                segments: vec![StyledSegment::plain(text)],
            })
            .collect();
    }
    let mut out = Vec::with_capacity(lines.len());
    let mut in_fence = false;
    let mut index = 0;
    while index < lines.len() {
        let source_line = lines[index];
        if let Some(info) = source_line.trim_start().strip_prefix("```") {
            let info = info.trim();
            in_fence = !in_fence;
            // The fence itself becomes a rule carrying the language, so the
            // block is still marked off without three backticks on screen.
            let rule = glyphs.table_row.repeat(3);
            let text = if info.is_empty() {
                rule
            } else {
                format!("{rule} {info}")
            };
            out.push(line_of(vec![
                StyledSegment::toned(text, RowTone::Code).dimmed()
            ]));
            index += 1;
            continue;
        }
        if in_fence {
            // Code is shown as written: inside a fence there is no markup to
            // consume.
            out.push(MarkdownLine {
                text: source_line.to_string(),
                segments: vec![StyledSegment::toned(source_line, RowTone::Code)],
            });
            index += 1;
            continue;
        }
        let span = table_at(&lines, index);
        if span > 0 {
            if let Some(table) = render_table(&lines[index..index + span], glyphs) {
                out.extend(table);
                index += span;
                continue;
            }
        }
        out.push(render_block_line(source_line, glyphs));
        index += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyphs::UNICODE_GLYPHS;

    fn render(source: &str) -> Vec<MarkdownLine> {
        render_markdown(source, &UNICODE_GLYPHS)
    }

    fn texts(source: &str) -> Vec<String> {
        render(source).into_iter().map(|line| line.text).collect()
    }

    #[test]
    fn one_output_line_per_source_line() {
        for source in [
            "plain",
            "# head\nbody",
            "| a | b |\n|---|---|\n| 1 | 2 |",
            "```rust\nlet x = 1;\n```\nafter",
            "- one\n- two\n\n> quote",
        ] {
            assert_eq!(
                render(source).len(),
                source.split('\n').count(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn segments_always_sum_to_the_line_text() {
        let source = "# **head** `code`\n- [label](https://x.y) and *em*\n| a | b |\n|---|---|\n| 你好 | 2 |";
        for line in render(source) {
            assert_eq!(segment_text(&line.segments), line.text);
        }
    }

    #[test]
    fn delimiters_are_consumed_not_shown() {
        assert_eq!(texts("**bold** and *em* and `code`"), vec!["bold and em and code"]);
        assert_eq!(texts("~~gone~~"), vec!["gone"]);
        assert_eq!(texts("## Heading"), vec!["Heading"]);
    }

    #[test]
    fn an_unclosed_construct_stays_literal() {
        // Mid-stream this is the ordinary case; styling half of it would make
        // the row twitch as the rest arrives.
        assert_eq!(texts("**not yet"), vec!["**not yet"]);
        assert_eq!(texts("a `code"), vec!["a `code"]);
    }

    #[test]
    fn code_wins_over_emphasis_inside_it() {
        let line = &render("`**not bold**`")[0];
        assert_eq!(line.text, "**not bold**");
        assert_eq!(line.segments[0].tone, Some(RowTone::Code));
        assert!(!line.segments[0].bold);
    }

    #[test]
    fn underscores_inside_a_word_are_not_emphasis() {
        assert_eq!(texts("str_replace_editor"), vec!["str_replace_editor"]);
        assert_eq!(texts("read_image and _em_"), vec!["read_image and em"]);
    }

    #[test]
    fn a_link_keeps_its_target_visible() {
        assert_eq!(
            segment_text(&style_inline("see [docs](https://example.com)")),
            "see docs (https://example.com)"
        );
        // Brackets are not in the fast-path probe, so a line whose only markup
        // is a link never reaches the renderer at all. That is the original's
        // behaviour and it is the right trade: the probe exists to skip the scan
        // for ordinary prose, and a bare URL reads fine either way.
        assert_eq!(
            texts("see [docs](https://example.com)"),
            vec!["see [docs](https://example.com)"]
        );
        // With any other marker on the line the renderer does run.
        assert_eq!(
            texts("see **the** [docs](https://example.com)"),
            vec!["see the docs (https://example.com)"]
        );
    }

    #[test]
    fn list_and_quote_markers_are_replaced() {
        assert_eq!(texts("- one"), vec!["• one"]);
        assert_eq!(texts("  + two"), vec!["  • two"]);
        assert_eq!(texts("1. first"), vec!["1. first"]);
        assert_eq!(texts("> quoted"), vec!["│ quoted"]);
    }

    #[test]
    fn a_thematic_break_becomes_a_fixed_width_rule() {
        assert_eq!(texts("---"), vec!["─".repeat(RULE_WIDTH)]);
    }

    #[test]
    fn a_fence_becomes_a_rule_and_its_body_stays_verbatim() {
        let lines = render("```rust\n  let x = **1**;\n```");
        assert_eq!(lines[0].text, "─── rust");
        assert_eq!(lines[1].text, "  let x = **1**;");
        assert_eq!(lines[1].segments[0].tone, Some(RowTone::Code));
        assert_eq!(lines[2].text, "───");
    }

    #[test]
    fn table_columns_align_by_display_width() {
        let lines = texts("| a | b |\n|---|---|\n| 你好 | 2 |");
        // The header cell is padded to the width of the CJK cell below it, which
        // is four columns wide, not two characters.
        assert_eq!(display_width(&lines[0]), display_width(&lines[2]));
        assert!(lines[1].contains('┼'));
    }

    #[test]
    fn table_alignment_specs_are_honoured() {
        let lines = texts("| a | b |\n|--:|:-:|\n| 1 | 2 |");
        assert!(lines[2].starts_with("1"), "{:?}", lines[2]);
        let wide = texts("| header | b |\n|------:|---|\n| 1 | 2 |");
        assert!(wide[2].starts_with("     1"), "{:?}", wide[2]);
    }

    #[test]
    fn a_table_without_a_separator_is_not_a_table_yet() {
        assert_eq!(texts("| a | b |"), vec!["| a | b |"]);
    }

    #[test]
    fn plain_text_takes_the_fast_path_unchanged() {
        assert!(is_plain_text("just some words here"));
        assert!(!is_plain_text("some **bold** words"));
        assert!(!is_plain_text("- a list"));
        assert_eq!(texts("just some words"), vec!["just some words"]);
    }

    #[test]
    fn nesting_stops_at_the_depth_limit() {
        // `**bold `code`**` is the deepest shape worth supporting.
        let line = &render("**bold `code`**")[0];
        assert_eq!(line.text, "bold code");
        assert!(line.segments.iter().all(|segment| segment.bold));
    }
}

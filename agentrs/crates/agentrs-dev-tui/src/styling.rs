// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/styling.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; segment splitting counts chars, not
//            UTF-16 units; brand-art literal colours dropped with the art.
//! Row and segment styling vocabulary.
//!
//! Tone is a *semantic* name, never a colour: [`crate::theme`] maps it to
//! whatever the terminal can actually show, down to nothing at all with colour
//! off. Keeping the vocabulary here lets the card builders and the transcript
//! view share it without importing each other.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/styling.ts`.

use crate::text::wrap_words;
use crate::theme::RowTone;

/// A styled run within a row. Concatenating `text` yields the plain row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StyledSegment {
    /// The run's characters.
    pub text: String,
    /// What the run means, if anything more specific than its row.
    pub tone: Option<RowTone>,
    /// Rendered bold.
    pub bold: bool,
    /// Rendered dim.
    pub dim: bool,
    /// Rendered italic.
    pub italic: bool,
    /// Rendered struck through.
    pub strikethrough: bool,
}

impl StyledSegment {
    /// An unstyled run.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// A run carrying one tone.
    pub fn toned(text: impl Into<String>, tone: RowTone) -> Self {
        Self {
            text: text.into(),
            tone: Some(tone),
            ..Self::default()
        }
    }

    /// The same run, dimmed.
    pub fn dimmed(mut self) -> Self {
        self.dim = true;
        self
    }
}

/// One body row of a tool card, with the tone it should be painted in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetailLine {
    /// The plain row, which is what wrapping and searching see.
    pub text: String,
    /// The tone of the whole row.
    pub tone: Option<RowTone>,
    /// Styled runs within the row; `text` stays their exact concatenation.
    pub segments: Option<Vec<StyledSegment>>,
}

impl DetailLine {
    /// A row with no styling of its own.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// A row painted entirely in one tone.
    pub fn toned(text: impl Into<String>, tone: RowTone) -> Self {
        Self {
            text: text.into(),
            tone: Some(tone),
            ..Self::default()
        }
    }

    /// A row built from styled runs.
    pub fn styled(segments: Vec<StyledSegment>) -> Self {
        Self {
            text: segment_text(&segments),
            tone: None,
            segments: Some(segments),
        }
    }
}

/// The plain text of a styled row, for tests, search, and the no-colour form.
pub fn segment_text(segments: &[StyledSegment]) -> String {
    segments.iter().map(|segment| segment.text.as_str()).collect()
}

/// Wraps plain rows that carry no tone of their own.
pub fn plain_lines<S: AsRef<str>>(rows: &[S]) -> Vec<DetailLine> {
    rows.iter().map(|row| DetailLine::plain(row.as_ref())).collect()
}

/// Redistributes styled runs across already-wrapped rows.
///
/// The rows must be the output of wrapping [`segment_text`] of the same
/// segments, so this only has to walk the segments in step with them:
/// whitespace the wrapper dropped at a break is skipped, and a run that
/// straddles a break is split. Keeping the wrap decision in one place is what
/// stops the styled and plain forms from disagreeing.
pub fn wrap_segments(segments: &[StyledSegment], rows: &[String]) -> Vec<Vec<StyledSegment>> {
    let source: Vec<char> = segment_text(segments).chars().collect();
    let lengths: Vec<usize> = segments.iter().map(|s| s.text.chars().count()).collect();
    let mut out = Vec::with_capacity(rows.len());
    let mut at = 0;
    let mut index = 0;
    let mut offset = 0;

    let advance = |index: &mut usize, offset: &mut usize| {
        while *index < lengths.len() && *offset >= lengths[*index] {
            *offset -= lengths[*index];
            *index += 1;
        }
    };

    for row in rows {
        let row_chars: Vec<char> = row.chars().collect();
        // Skip whatever the wrapper dropped between rows (only ever whitespace).
        while at < source.len()
            && source[at].is_whitespace()
            && source.get(at..at + row_chars.len()) != Some(row_chars.as_slice())
        {
            at += 1;
            offset += 1;
            advance(&mut index, &mut offset);
        }
        let mut current: Vec<StyledSegment> = Vec::new();
        let mut remaining = row_chars.len();
        while remaining > 0 && index < lengths.len() {
            let available = lengths[index] - offset;
            let take = available.min(remaining);
            if take > 0 {
                let mut piece = segments[index].clone();
                piece.text = segments[index]
                    .text
                    .chars()
                    .skip(offset)
                    .take(take)
                    .collect();
                current.push(piece);
            }
            offset += take;
            remaining -= take;
            advance(&mut index, &mut offset);
        }
        at += row_chars.len();
        if current.is_empty() {
            current.push(StyledSegment::plain(row.clone()));
        }
        out.push(current);
    }
    out
}

/// Wraps one styled row to `columns`, returning the styled rows.
///
/// Convenience over [`wrap_words`] plus [`wrap_segments`]: those two must always
/// be called on the same text, and calling them apart is how the plain and
/// styled forms drift.
pub fn wrap_styled(segments: &[StyledSegment], columns: usize) -> Vec<Vec<StyledSegment>> {
    let rows = wrap_words(&segment_text(segments), columns);
    wrap_segments(segments, &rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(rows: &[Vec<StyledSegment>]) -> Vec<String> {
        rows.iter().map(|row| segment_text(row)).collect()
    }

    #[test]
    fn segments_survive_a_break_inside_a_run() {
        let segments = vec![
            StyledSegment::plain("alpha "),
            StyledSegment::toned("beta gamma", RowTone::Code),
        ];
        let wrapped = wrap_styled(&segments, 8);
        assert_eq!(text_of(&wrapped), vec!["alpha", "beta", "gamma"]);
        // The tone follows the halves of the run it was split from.
        assert_eq!(wrapped[1][0].tone, Some(RowTone::Code));
        assert_eq!(wrapped[2][0].tone, Some(RowTone::Code));
    }

    #[test]
    fn every_wrapped_row_is_the_plain_row() {
        let segments = vec![
            StyledSegment::toned("read", RowTone::Code),
            StyledSegment::plain(" the file 你好世界 now"),
        ];
        for columns in 3..30 {
            let rows = wrap_words(&segment_text(&segments), columns);
            let styled = wrap_segments(&segments, &rows);
            assert_eq!(text_of(&styled), rows, "columns={columns}");
        }
    }

    #[test]
    fn an_empty_row_still_gets_a_segment() {
        let styled = wrap_segments(&[StyledSegment::plain("")], &[String::new()]);
        assert_eq!(styled.len(), 1);
        assert_eq!(styled[0].len(), 1);
    }

    #[test]
    fn styled_line_text_is_the_concatenation() {
        let line = DetailLine::styled(vec![
            StyledSegment::plain("a"),
            StyledSegment::toned("b", RowTone::Error),
        ]);
        assert_eq!(line.text, "ab");
    }
}

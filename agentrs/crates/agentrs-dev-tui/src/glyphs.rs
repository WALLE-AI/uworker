// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/glyphs.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; added a tree prefix, dropped the
//            unused `reasoning` glyph.
//! Glyph sets.
//!
//! A console that cannot render `▸` shows a replacement box, which reads as
//! corruption; an ASCII stand-in reads as a deliberate choice. Every glyph in
//! both sets is one cell wide, so row budgets are unaffected by the choice.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/glyphs.ts`.

/// One complete set of transcript markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphSet {
    /// A tool call that has been proposed but has not started.
    pub pending: &'static str,
    /// A tool call that succeeded.
    pub succeeded: &'static str,
    /// A tool call that failed or was denied.
    pub failed: &'static str,
    /// A tool call that was interrupted.
    pub interrupted: &'static str,
    /// The user's own message.
    pub user: &'static str,
    /// Assistant paragraph marker.
    pub bullet: &'static str,
    /// Reasoning marker; distinct from the assistant's so folds read differently.
    pub thinking: &'static str,
    /// A durable marker: compaction, approval audit, mode change.
    pub marker: &'static str,
    /// Horizontal rule, repeated to fill.
    pub rule: &'static str,
    /// Fold indicator.
    pub fold: &'static str,
    /// Tool-card body gutter: the first row.
    pub gutter_first: &'static str,
    /// Tool-card body gutter: the alignment for every row after the first.
    pub gutter_rest: &'static str,
    /// Markdown list marker, replacing whichever of `-`, `*`, `+` was written.
    pub list_bullet: &'static str,
    /// Markdown block-quote marker, replacing `>`.
    pub quote_bar: &'static str,
    /// Rendered-table column separator.
    pub table_column: &'static str,
    /// Rendered-table horizontal rule.
    pub table_row: &'static str,
    /// Rendered-table joint between the column separator and its rule.
    pub table_cross: &'static str,
    /// Tree prefix for a nested row.
    pub tree: &'static str,
}

/// The set used when the terminal can place wide and box-drawing glyphs.
pub const UNICODE_GLYPHS: GlyphSet = GlyphSet {
    pending: "▸",
    succeeded: "✓",
    failed: "✗",
    interrupted: "⚠",
    user: ">",
    bullet: "●",
    thinking: "∴",
    marker: "•",
    rule: "─",
    fold: "…",
    gutter_first: " ⎿ ",
    gutter_rest: "   ",
    list_bullet: "•",
    quote_bar: "│",
    table_column: "│",
    table_row: "─",
    table_cross: "┼",
    tree: "└─",
};

/// The stand-in set for a terminal without wide-glyph support.
pub const ASCII_GLYPHS: GlyphSet = GlyphSet {
    pending: ">",
    succeeded: "+",
    failed: "x",
    interrupted: "!",
    user: ">",
    bullet: "*",
    thinking: "~",
    marker: "*",
    rule: "-",
    fold: "...",
    gutter_first: " \\ ",
    gutter_rest: "   ",
    list_bullet: "-",
    quote_bar: "|",
    table_column: "|",
    table_row: "-",
    table_cross: "+",
    tree: "\\-",
};

/// Selects the set matching the terminal's wide-glyph support.
pub const fn glyph_set(unicode: bool) -> GlyphSet {
    if unicode {
        UNICODE_GLYPHS
    } else {
        ASCII_GLYPHS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::display_width;

    #[test]
    fn every_status_glyph_is_one_cell_in_either_set() {
        for set in [UNICODE_GLYPHS, ASCII_GLYPHS] {
            for glyph in [set.pending, set.succeeded, set.failed, set.interrupted] {
                assert_eq!(display_width(glyph), 1, "{glyph:?}");
            }
        }
    }

    #[test]
    fn the_two_gutters_align() {
        for set in [UNICODE_GLYPHS, ASCII_GLYPHS] {
            assert_eq!(
                display_width(set.gutter_first),
                display_width(set.gutter_rest),
                "a card body would step sideways after its first row"
            );
        }
    }

    #[test]
    fn ascii_set_is_pure_ascii() {
        let set = ASCII_GLYPHS;
        for glyph in [
            set.pending,
            set.succeeded,
            set.failed,
            set.interrupted,
            set.user,
            set.bullet,
            set.thinking,
            set.marker,
            set.rule,
            set.fold,
            set.gutter_first,
            set.list_bullet,
            set.quote_bar,
            set.table_column,
            set.table_row,
            set.table_cross,
            set.tree,
        ] {
            assert!(glyph.is_ascii(), "{glyph:?} is not ASCII");
        }
    }
}

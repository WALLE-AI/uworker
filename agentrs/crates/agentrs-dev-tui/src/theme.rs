// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/theme.ts, styling.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; tones resolve to ratatui styles;
//            the original's ansi256 level folds into Basic, which its own
//            palette already treated identically.
//! Semantic palette.
//!
//! A [`RowTone`] names a *meaning*; this module is the only place that decides
//! what it looks like, and it degrades with the terminal rather than assuming
//! truecolor. With colour off every tone resolves to no colour at all — the
//! renderer emits no styling, rather than a "colourless" style.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/theme.rs` and
//! `styling.ts`.

use ratatui::style::{Color, Modifier, Style};

/// What a row means. The renderer never picks a colour directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowTone {
    /// The user's own message.
    User,
    /// Ordinary assistant prose.
    Assistant,
    /// Model reasoning.
    Reasoning,
    /// Host-emitted commentary and durable markers.
    System,
    /// A tool card with no more specific kind.
    Tool,
    /// A failure, whatever it was doing.
    Error,
    /// A diff row that adds.
    DiffAdd,
    /// A diff row that removes.
    DiffRemove,
    /// A diff hunk header.
    DiffHunk,
    /// Verbatim code.
    Code,
    /// A markdown heading.
    Heading,
    /// A markdown block quote.
    Quote,
    /// A markdown list marker.
    Bullet,
    /// A card badge.
    Badge,
    /// Something the reader should notice but that has not failed.
    Warning,
    /// A tool that runs a command.
    ToolTerminal,
    /// A tool that changes files.
    ToolDiff,
    /// A tool that searches.
    ToolSearch,
    /// A tool that reads.
    ToolRead,
    /// A tool that fetches over the network.
    ToolWeb,
    /// A permission preset narrower than the default.
    ModeRestricted,
    /// A permission preset wider than the default.
    ModeDanger,
}

/// How much colour the terminal can be trusted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorLevel {
    /// No styling at all.
    None,
    /// The sixteen ANSI names, which respect the user's own scheme.
    Basic,
    /// Full 24-bit colour.
    Truecolor,
}

/// Tones that read as secondary content whatever the palette.
const DIM_TONES: &[RowTone] = &[
    RowTone::Reasoning,
    RowTone::System,
    RowTone::Quote,
    RowTone::DiffHunk,
];

/// Basic ANSI names: the only thing a 16-colour terminal can be trusted with.
const fn basic(tone: RowTone) -> Option<Color> {
    match tone {
        RowTone::User => Some(Color::Green),
        RowTone::Tool | RowTone::Heading | RowTone::Bullet | RowTone::ToolRead => Some(Color::Cyan),
        RowTone::DiffHunk => Some(Color::Cyan),
        RowTone::Error | RowTone::DiffRemove | RowTone::ModeDanger => Some(Color::Red),
        RowTone::DiffAdd | RowTone::ToolDiff => Some(Color::Green),
        RowTone::Code | RowTone::Warning | RowTone::ToolTerminal => Some(Color::Yellow),
        RowTone::Badge | RowTone::ToolSearch => Some(Color::Magenta),
        RowTone::ToolWeb => Some(Color::Blue),
        RowTone::ModeRestricted => Some(Color::Cyan),
        // Prose and reasoning inherit the user's own foreground.
        RowTone::Assistant | RowTone::Reasoning | RowTone::System | RowTone::Quote => None,
    }
}

/// True colour. The greens and reds are muted relative to the ANSI defaults so a
/// large diff does not vibrate, and `Assistant` stays unset so ordinary prose
/// inherits the user's own foreground.
const fn truecolor(tone: RowTone) -> Option<Color> {
    match tone {
        RowTone::User | RowTone::Heading | RowTone::ToolWeb => Some(Color::Rgb(0x7d, 0xa1, 0xde)),
        RowTone::Assistant => None,
        RowTone::Reasoning | RowTone::System | RowTone::Quote | RowTone::DiffHunk => {
            Some(Color::Rgb(0x8d, 0x95, 0xa6))
        }
        RowTone::Tool | RowTone::Bullet | RowTone::ToolRead | RowTone::ModeRestricted => {
            Some(Color::Rgb(0x6c, 0xb6, 0xc9))
        }
        RowTone::Error | RowTone::ModeDanger => Some(Color::Rgb(0xe0, 0x6c, 0x75)),
        RowTone::DiffAdd | RowTone::ToolDiff => Some(Color::Rgb(0x7f, 0xb3, 0x7f)),
        RowTone::DiffRemove => Some(Color::Rgb(0xcf, 0x7f, 0x7f)),
        RowTone::Code | RowTone::Warning | RowTone::ToolTerminal => {
            Some(Color::Rgb(0xd8, 0xb0, 0x70))
        }
        RowTone::Badge | RowTone::ToolSearch => Some(Color::Rgb(0xb4, 0x8e, 0xad)),
    }
}

/// The resolved palette for one terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    level: ColorLevel,
}

impl Theme {
    /// Resolves the palette. `enabled` is the `--no-color` flag, which overrides
    /// detection.
    pub const fn resolve(level: ColorLevel, enabled: bool) -> Self {
        Self {
            level: if enabled { level } else { ColorLevel::None },
        }
    }

    /// False when the terminal gets no colour at all.
    pub const fn enabled(self) -> bool {
        !matches!(self.level, ColorLevel::None)
    }

    /// Foreground for a tone, if it has one.
    pub const fn color(self, tone: RowTone) -> Option<Color> {
        match self.level {
            ColorLevel::None => None,
            ColorLevel::Basic => basic(tone),
            ColorLevel::Truecolor => truecolor(tone),
        }
    }

    /// Fill behind a row.
    ///
    /// A filled row is the loudest thing a terminal can do, so exactly one tone
    /// gets one: the user's own message, which needs to be findable while
    /// scrolling past screens of tool output. Below truecolor there is no such
    /// shade to be had, so a 16-colour terminal gets none rather than a solid
    /// blue slab.
    pub const fn background(self, tone: RowTone) -> Option<Color> {
        match (self.level, tone) {
            (ColorLevel::Truecolor, RowTone::User) => Some(Color::Rgb(0x23, 0x2a, 0x36)),
            _ => None,
        }
    }

    /// Whether the tone reads as secondary content.
    pub fn dim(self, tone: RowTone) -> bool {
        DIM_TONES.contains(&tone)
    }

    /// The complete ratatui style for a tone.
    pub fn style(self, tone: RowTone) -> Style {
        let mut style = Style::default();
        if let Some(color) = self.color(tone) {
            style = style.fg(color);
        }
        if let Some(color) = self.background(tone) {
            style = style.bg(color);
        }
        if self.dim(tone) {
            style = style.add_modifier(Modifier::DIM);
        }
        style
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TONES: &[RowTone] = &[
        RowTone::User,
        RowTone::Assistant,
        RowTone::Reasoning,
        RowTone::System,
        RowTone::Tool,
        RowTone::Error,
        RowTone::DiffAdd,
        RowTone::DiffRemove,
        RowTone::DiffHunk,
        RowTone::Code,
        RowTone::Heading,
        RowTone::Quote,
        RowTone::Bullet,
        RowTone::Badge,
        RowTone::Warning,
        RowTone::ToolTerminal,
        RowTone::ToolDiff,
        RowTone::ToolSearch,
        RowTone::ToolRead,
        RowTone::ToolWeb,
        RowTone::ModeRestricted,
        RowTone::ModeDanger,
    ];

    #[test]
    fn no_color_emits_nothing_at_all() {
        let theme = Theme::resolve(ColorLevel::Truecolor, false);
        assert!(!theme.enabled());
        for tone in TONES {
            assert_eq!(theme.color(*tone), None);
            assert_eq!(theme.background(*tone), None);
        }
    }

    #[test]
    fn basic_never_emits_a_background_or_an_rgb_value() {
        let theme = Theme::resolve(ColorLevel::Basic, true);
        for tone in TONES {
            assert_eq!(theme.background(*tone), None);
            assert!(!matches!(theme.color(*tone), Some(Color::Rgb(..))));
        }
    }

    #[test]
    fn only_the_user_row_is_ever_filled() {
        let theme = Theme::resolve(ColorLevel::Truecolor, true);
        for tone in TONES {
            let filled = theme.background(*tone).is_some();
            assert_eq!(filled, *tone == RowTone::User, "{tone:?}");
        }
    }

    #[test]
    fn dimming_survives_colour_being_off() {
        // Without colour, dimming is the only remaining way to rank a row.
        let theme = Theme::resolve(ColorLevel::None, true);
        assert!(theme.dim(RowTone::Reasoning));
        assert!(!theme.dim(RowTone::Assistant));
    }
}

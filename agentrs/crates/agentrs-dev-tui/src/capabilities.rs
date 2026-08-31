// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/terminal-capabilities.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; detection takes an already-read
//            probe instead of reading the environment itself; terminal size is
//            not probed here because ratatui reports it per frame.
//! Terminal capability detection.
//!
//! Every reduced capability is reported as an explicit note so a degraded run
//! says why it looks different rather than leaving the reader to guess.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/terminal-capabilities.ts`
//! with two deliberate reductions. The original's `ansi256` level is folded into
//! [`ColorLevel::Basic`] because its own palette already treats anything short of
//! truecolor as the sixteen ANSI names; and terminal size is not probed here
//! because ratatui reports it per frame, which is the only moment it is true.
//!
//! Detection is a pure function of an already-read [`EnvProbe`]. Reading the
//! environment is `host_io`'s job.

use crate::theme::ColorLevel;

/// The environment variables detection looks at, already read.
#[derive(Debug, Clone, Default)]
pub struct EnvProbe {
    /// `TERM`.
    pub term: String,
    /// `COLORTERM`.
    pub color_term: String,
    /// `NO_COLOR`.
    pub no_color: String,
    /// `FORCE_COLOR`.
    pub force_color: String,
    /// `WT_SESSION`, set by Windows Terminal.
    pub wt_session: String,
    /// `TMUX`.
    pub tmux: String,
    /// `SSH_CONNECTION`, `SSH_TTY` and `SSH_CLIENT` joined.
    pub ssh: String,
    /// `LC_ALL`, `LC_CTYPE` and `LANG` joined.
    pub locale: String,
    /// Whether both stdin and stdout are terminals.
    pub interactive: bool,
    /// Whether the host platform is Windows.
    pub windows: bool,
}

/// A multiplexer sitting between the app and the real terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    /// tmux.
    Tmux,
    /// GNU screen.
    Screen,
}

impl Multiplexer {
    /// The name used in notes.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Screen => "screen",
        }
    }
}

/// The usable feature set of one terminal.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Both streams are terminals.
    pub interactive: bool,
    /// How much colour may be emitted.
    pub color_level: ColorLevel,
    /// The terminal can be trusted with box-drawing and braille glyphs.
    pub unicode: bool,
    /// The multiplexer in the way, if any.
    pub multiplexer: Option<Multiplexer>,
    /// The session reaches a remote host over SSH.
    pub remote: bool,
    /// One note per degradation, in detection order.
    pub notes: Vec<String>,
}

/// True for a variable that is set to something other than empty or `0`.
fn truthy(value: &str) -> bool {
    !value.is_empty() && value != "0"
}

fn color_of(probe: &EnvProbe, term: &str) -> ColorLevel {
    if probe.force_color == "0" || truthy(&probe.no_color) || term == "dumb" {
        return ColorLevel::None;
    }
    if !probe.interactive && !truthy(&probe.force_color) {
        return ColorLevel::None;
    }
    let color_term = probe.color_term.to_lowercase();
    if color_term == "truecolor" || color_term == "24bit" {
        return ColorLevel::Truecolor;
    }
    if truthy(&probe.wt_session) || term.contains("kitty") || term.contains("direct") {
        return ColorLevel::Truecolor;
    }
    ColorLevel::Basic
}

/// Derives the usable terminal feature set from the environment.
pub fn detect(probe: &EnvProbe) -> Capabilities {
    let mut notes = Vec::new();
    let term = probe.term.to_lowercase();
    if !probe.interactive {
        notes.push("stdin or stdout is not a TTY; interactive rendering is unavailable".into());
    }

    let color_level = color_of(probe, &term);
    if color_level == ColorLevel::None && probe.interactive {
        notes.push("color output is disabled for this terminal".into());
    }

    let multiplexer = if truthy(&probe.tmux) || term.starts_with("tmux") {
        Some(Multiplexer::Tmux)
    } else if term.starts_with("screen") {
        Some(Multiplexer::Screen)
    } else {
        None
    };
    if let Some(multiplexer) = multiplexer {
        notes.push(format!(
            "{} detected; OSC hyperlinks and clipboard passthrough are disabled",
            multiplexer.label()
        ));
    }

    let remote = truthy(&probe.ssh);
    if remote {
        notes.push(
            "remote SSH session detected; editor launch uses the remote host environment".into(),
        );
    }

    let legacy_windows = probe.windows && !truthy(&probe.wt_session);
    if legacy_windows {
        notes.push(
            "legacy Windows console detected; ConPTY resize and Unicode support may be limited"
                .into(),
        );
    }

    // A non-UTF-8 locale renders wide glyphs as replacement boxes, which is worse
    // than the ASCII fallback; `TERM=dumb` cannot place them at all. An empty
    // locale says nothing, so it is not read as "assume the worst".
    let encoding = probe.locale.to_lowercase();
    let unicode = !legacy_windows
        && term != "dumb"
        && (encoding.is_empty() || encoding.contains("utf-8") || encoding.contains("utf8"));
    if probe.interactive && !unicode {
        notes.push("terminal or locale cannot show wide glyphs; ASCII stand-ins are used".into());
    }

    Capabilities {
        interactive: probe.interactive,
        color_level,
        unicode,
        multiplexer,
        remote,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interactive() -> EnvProbe {
        EnvProbe {
            term: "xterm-256color".into(),
            interactive: true,
            ..EnvProbe::default()
        }
    }

    #[test]
    fn no_color_wins_over_every_other_signal() {
        let probe = EnvProbe {
            color_term: "truecolor".into(),
            no_color: "1".into(),
            ..interactive()
        };
        let caps = detect(&probe);
        assert_eq!(caps.color_level, ColorLevel::None);
        assert!(caps.notes.iter().any(|note| note.contains("color output is disabled")));
    }

    #[test]
    fn empty_no_color_is_not_a_signal() {
        // An exported-but-empty variable is the shell's doing, not the user's.
        let probe = EnvProbe {
            no_color: String::new(),
            ..interactive()
        };
        assert_eq!(detect(&probe).color_level, ColorLevel::Basic);
    }

    #[test]
    fn truecolor_comes_from_colorterm_or_a_known_terminal() {
        for probe in [
            EnvProbe { color_term: "TrueColor".into(), ..interactive() },
            EnvProbe { color_term: "24bit".into(), ..interactive() },
            EnvProbe { term: "xterm-kitty".into(), ..interactive() },
            EnvProbe { wt_session: "abc".into(), ..interactive() },
        ] {
            assert_eq!(detect(&probe).color_level, ColorLevel::Truecolor);
        }
    }

    #[test]
    fn dumb_terminal_gets_neither_colour_nor_wide_glyphs() {
        let caps = detect(&EnvProbe { term: "dumb".into(), ..interactive() });
        assert_eq!(caps.color_level, ColorLevel::None);
        assert!(!caps.unicode);
    }

    #[test]
    fn a_non_utf8_locale_drops_wide_glyphs() {
        let caps = detect(&EnvProbe { locale: "en_US.ISO-8859-1".into(), ..interactive() });
        assert!(!caps.unicode);
        assert!(caps.notes.iter().any(|note| note.contains("ASCII stand-ins")));
    }

    #[test]
    fn an_empty_locale_says_nothing_and_keeps_wide_glyphs() {
        assert!(detect(&interactive()).unicode);
    }

    #[test]
    fn multiplexer_and_ssh_are_each_noted_once() {
        let caps = detect(&EnvProbe {
            tmux: "/tmp/tmux-1000/default,123,0".into(),
            ssh: "10.0.0.1 22".into(),
            ..interactive()
        });
        assert_eq!(caps.multiplexer, Some(Multiplexer::Tmux));
        assert!(caps.remote);
        assert_eq!(caps.notes.iter().filter(|note| note.contains("tmux")).count(), 1);
    }

    #[test]
    fn non_interactive_reports_it_and_drops_colour() {
        let caps = detect(&EnvProbe { interactive: false, ..interactive() });
        assert_eq!(caps.color_level, ColorLevel::None);
        assert!(caps.notes.iter().any(|note| note.contains("not a TTY")));
    }
}

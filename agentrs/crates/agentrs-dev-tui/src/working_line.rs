// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/working-line.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; the ellipsis comes from the glyph set
//            so an ASCII terminal gets an ASCII row; the subagent row is
//            dropped (this host derives no child runs).
//! The line shown above the composer while the run is working.
//!
//! Its job is to make a long wait legible: something is turning, it has been
//! turning for this long, this much has come back — and, when it stops coming
//! back, that too. The verb is decorative but it is picked from the turn index
//! rather than at random, so a frame is reproducible from its inputs and the
//! view stays snapshot-testable.
//!
//! Every field is optional under width pressure. The line is assembled from the
//! most important part outward and stops when the next part will not fit, so a
//! forty-column terminal keeps the spinner and the subject rather than wrapping
//! onto a second row and breaking the layout budget.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/working-line.ts`.

use crate::spinner::format_elapsed;
use crate::text::{display_width, truncate_to_width};

/// Token counts only appear once the wait is long enough to want them. Before
/// that they are noise on a line that is already changing every 80 ms.
pub const TOKENS_AFTER_MS: u64 = 30_000;

/// How long output may stop before the wait is called stalled.
///
/// A run with a tool in flight is never stalled however quiet it is: the tool is
/// the work, and its own card is already saying so. This only fires when the
/// model itself has gone silent, which is the case a reader cannot otherwise
/// distinguish from progress.
pub const STALL_AFTER_MS: u64 = 10_000;

const VERBS: [&str; 10] = [
    "Working",
    "Thinking",
    "Digging",
    "Reasoning",
    "Puzzling",
    "Considering",
    "Tracing",
    "Weighing",
    "Composing",
    "Checking",
];

/// The verb for one turn. Stable within the turn, varied across turns.
pub fn working_verb(turn: usize) -> &'static str {
    VERBS[turn % VERBS.len()]
}

/// Everything the line is assembled from.
#[derive(Debug, Clone, Default)]
pub struct WorkingLineInput<'a> {
    /// Current spinner frame, or the static glyph when animation is off.
    pub frame: &'a str,
    /// Turn index, used to pick the verb.
    pub turn: usize,
    /// How long the turn has been going.
    pub elapsed_ms: u64,
    /// Output tokens so far, when any have been reported.
    pub tokens: Option<u64>,
    /// Separator glyph, so a degraded terminal stays ASCII.
    pub separator: &'a str,
    /// Ellipsis glyph, from the same set as the separator.
    pub ellipsis: &'a str,
    /// What the run is doing right now, in the tool's own words.
    ///
    /// `Reading src/a.md` rather than `Working…`. Taken from the running card's
    /// activity phrase, so a tool this profile has never heard of still
    /// describes itself.
    pub activity: Option<&'a str>,
    /// How long output has been absent; `None` when nothing is being awaited.
    pub silent_ms: Option<u64>,
    /// A tool is in flight, so silence is the tool working rather than a stall.
    pub tool_running: bool,
}

/// The assembled line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingLine {
    /// The row, already fitted to the terminal.
    pub text: String,
    /// The run has gone quiet for longer than [`STALL_AFTER_MS`].
    ///
    /// The view paints the row in a warning tone; the wait itself is unchanged.
    pub stalled: bool,
}

fn compact_tokens(value: u64) -> String {
    if value < 1_000 {
        value.to_string()
    } else {
        format!("{:.1}k", value as f64 / 1_000.0)
    }
}

/// Renders the working line, bounded to the terminal width.
///
/// Fields are appended in falling order of worth and the first one that does not
/// fit ends the line. The spinner and the subject are what say the run is alive,
/// so they are what a narrow terminal keeps.
pub fn build_working_line(input: &WorkingLineInput<'_>, columns: usize) -> WorkingLine {
    let stalled = !input.tool_running
        && input.silent_ms.is_some_and(|silent| silent >= STALL_AFTER_MS);
    if columns == 0 {
        return WorkingLine {
            text: String::new(),
            stalled,
        };
    }

    // The activity is the better subject when there is one: `Reading src/a.md`
    // says more than any verb this profile could invent.
    let subject = match input.activity {
        Some(activity) => activity.to_string(),
        None => format!("{}{}", working_verb(input.turn), input.ellipsis),
    };
    // The stall comes first because fields are dropped from the end: it is the
    // one field that changes what the reader should do about the wait.
    let mut optional = Vec::new();
    if let (true, Some(silent)) = (stalled, input.silent_ms) {
        optional.push(format!("no output for {}", format_elapsed(silent)));
    }
    optional.push(format_elapsed(input.elapsed_ms));
    if let Some(tokens) = input.tokens {
        if input.elapsed_ms >= TOKENS_AFTER_MS {
            optional.push(format!("{} tokens", compact_tokens(tokens)));
        }
    }

    let mut text = format!("{} {subject}", input.frame);
    for part in optional {
        let next = format!("{text} {} {part}", input.separator);
        if display_width(&next) > columns {
            break;
        }
        text = next;
    }
    WorkingLine {
        text: truncate_to_width(&text, columns),
        stalled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>() -> WorkingLineInput<'a> {
        WorkingLineInput {
            frame: "⠋",
            turn: 1,
            elapsed_ms: 4_000,
            separator: "·",
            ellipsis: "…",
            ..WorkingLineInput::default()
        }
    }

    #[test]
    fn the_verb_is_stable_within_a_turn_and_varies_across_turns() {
        assert_eq!(working_verb(1), working_verb(1));
        assert_ne!(working_verb(1), working_verb(2));
        // And it wraps rather than running out.
        assert_eq!(working_verb(0), working_verb(10));
    }

    #[test]
    fn an_activity_is_a_better_subject_than_any_verb() {
        let line = build_working_line(
            &WorkingLineInput {
                activity: Some("Reading src/a.md"),
                ..input()
            },
            80,
        );
        assert_eq!(line.text, "⠋ Reading src/a.md · 4s");
    }

    #[test]
    fn tokens_stay_off_the_row_until_the_wait_is_worth_them() {
        let short = build_working_line(
            &WorkingLineInput {
                tokens: Some(1_200),
                elapsed_ms: 5_000,
                ..input()
            },
            80,
        );
        assert!(!short.text.contains("tokens"));
        let long = build_working_line(
            &WorkingLineInput {
                tokens: Some(1_200),
                elapsed_ms: TOKENS_AFTER_MS,
                ..input()
            },
            80,
        );
        assert!(long.text.ends_with("1.2k tokens"), "{}", long.text);
    }

    #[test]
    fn a_narrow_terminal_keeps_the_spinner_and_the_subject() {
        let line = build_working_line(
            &WorkingLineInput {
                activity: Some("Reading a/very/long/path.md"),
                tokens: Some(9_000),
                elapsed_ms: 60_000,
                ..input()
            },
            30,
        );
        assert!(display_width(&line.text) <= 30);
        assert!(line.text.starts_with("⠋ Reading"));
    }

    #[test]
    fn a_row_is_never_wider_than_the_terminal() {
        for columns in 1..60 {
            let line = build_working_line(
                &WorkingLineInput {
                    activity: Some("Reading 一个很长的中文路径.md"),
                    tokens: Some(123_456),
                    elapsed_ms: 90_000,
                    ..input()
                },
                columns,
            );
            assert!(display_width(&line.text) <= columns, "columns={columns}");
        }
    }

    #[test]
    fn silence_with_a_tool_in_flight_is_not_a_stall() {
        // The tool is the work, and its own card is already saying so.
        let line = build_working_line(
            &WorkingLineInput {
                silent_ms: Some(STALL_AFTER_MS * 2),
                tool_running: true,
                ..input()
            },
            80,
        );
        assert!(!line.stalled);
        assert!(!line.text.contains("no output"));
    }

    #[test]
    fn silence_without_one_is_reported_first() {
        let line = build_working_line(
            &WorkingLineInput {
                silent_ms: Some(STALL_AFTER_MS),
                tool_running: false,
                ..input()
            },
            80,
        );
        assert!(line.stalled);
        // It comes before the elapsed field, because it is the one that changes
        // what the reader should do about the wait.
        assert!(line.text.contains("no output for 10s · 4s"), "{}", line.text);
    }

    #[test]
    fn an_ascii_terminal_gets_an_ascii_row() {
        let line = build_working_line(
            &WorkingLineInput {
                frame: "|",
                separator: "-",
                ellipsis: "...",
                ..input()
            },
            80,
        );
        assert!(line.text.is_ascii(), "{}", line.text);
    }
}

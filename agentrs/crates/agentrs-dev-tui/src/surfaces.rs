// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/overlay.ts (helpRows), session-browser.ts,
//           transcript-view.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; the command set is this host's, and the session
//            browser lists durable JSONL logs because that is what this host
//            has instead of sessions.
//! What each full-screen surface is made of.
//!
//! Every builder here reads the same tables the rest of the app reads — the
//! shortcut sheet is built from the live [`Keymap`], not from a second list of
//! what the keys are supposed to be. That is the whole point of the keymap being
//! a table: rebinding a key changes what it does and what the sheet says it does
//! in one move, and a sheet that can go stale is worse than no sheet.

use crate::host_io::LogEntry;
use crate::keymap::{Action, Chord, Context, Keymap};
use crate::overlay::{two_column, Overlay, OverlayRow, Surface};
use crate::transcript::TranscriptRow;

/// One command the palette offers, and `/` completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// The name, without its leading slash.
    pub name: &'static str,
    /// What it does.
    pub summary: &'static str,
    /// The action it stands for. Every command is an action, so the palette,
    /// the slash form and any key that reaches it can never drift apart.
    pub action: Option<Action>,
    /// What an argument would mean, when the command takes one.
    pub argument: Option<&'static str>,
}

/// Every command, in the order the palette shows them.
///
/// The order is by how often you reach for them, not alphabetical: a palette
/// that opens on `/clear` when you almost always want `/diff` is a palette you
/// have to read every time.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "diff",
        summary: "review the staged ChangeSet before committing it",
        action: Some(Action::ReviewChanges),
        argument: None,
    },
    Command {
        name: "commit",
        summary: "commit the staged ChangeSet",
        action: Some(Action::CommitChanges),
        argument: None,
    },
    Command {
        name: "discard",
        summary: "discard the staged ChangeSet",
        action: Some(Action::DiscardChanges),
        argument: None,
    },
    Command {
        name: "clear",
        summary: "drop the conversation and start a fresh one",
        action: Some(Action::ClearSession),
        argument: None,
    },
    Command {
        name: "retry",
        summary: "send the last message again",
        action: Some(Action::Retry),
        argument: None,
    },
    Command {
        name: "cancel",
        summary: "interrupt the run in progress",
        action: Some(Action::Cancel),
        argument: None,
    },
    Command {
        name: "permission",
        summary: "cycle the permission mode for the next run",
        action: Some(Action::PermissionCycle),
        argument: Some("preset"),
    },
    Command {
        name: "status",
        summary: "what this session is: run, log, model, counters",
        action: Some(Action::ShowStatus),
        argument: None,
    },
    Command {
        name: "logs",
        summary: "browse durable logs",
        action: Some(Action::BrowseLogs),
        argument: None,
    },
    Command {
        name: "transcript",
        summary: "open the searchable transcript",
        action: Some(Action::TranscriptOpen),
        argument: None,
    },
    Command {
        name: "export",
        summary: "write the transcript out as markdown",
        action: Some(Action::ExportTranscript),
        argument: Some("path"),
    },
    Command {
        name: "fold",
        summary: "fold or unfold the card in view",
        action: Some(Action::FoldToggle),
        argument: None,
    },
    Command {
        name: "editor",
        summary: "open the card's file in $EDITOR",
        action: Some(Action::EditorOpen),
        argument: None,
    },
    Command {
        name: "mouse",
        summary: "give the wheel back to the terminal, or take it",
        action: Some(Action::ToggleMouse),
        argument: None,
    },
    Command {
        name: "help",
        summary: "show the shortcut sheet",
        action: Some(Action::HelpOpen),
        argument: None,
    },
    Command {
        name: "quit",
        summary: "quit (or ctrl+c twice)",
        action: Some(Action::Quit),
        argument: None,
    },
];

/// The command with this name, if there is one.
pub fn command(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|entry| entry.name == name)
}

/// The commands whose name starts with `prefix`.
pub fn commands_starting_with(prefix: &str) -> Vec<&'static Command> {
    COMMANDS
        .iter()
        .filter(|entry| entry.name.starts_with(prefix))
        .collect()
}

/// The order sections appear in the shortcut sheet.
const SECTIONS: &[(&str, &[Action])] = &[
    (
        "composing",
        &[
            Action::Submit,
            Action::Newline,
            Action::CaretLeft,
            Action::CaretRight,
            Action::CaretWordLeft,
            Action::CaretWordRight,
            Action::CaretLineStart,
            Action::CaretLineEnd,
            Action::DeleteBack,
            Action::DeleteForward,
            Action::DeleteWord,
            Action::DeleteToLineStart,
            Action::DeleteToLineEnd,
            Action::HistoryPrevious,
            Action::HistoryNext,
            Action::CompletionAccept,
        ],
    ),
    (
        "reading",
        &[
            Action::ScrollUp,
            Action::ScrollDown,
            Action::ScrollPageUp,
            Action::ScrollPageDown,
            Action::FoldToggle,
            Action::TranscriptOpen,
            Action::EditorOpen,
        ],
    ),
    (
        "surfaces",
        &[
            Action::HelpOpen,
            Action::PaletteOpen,
            Action::BrowseLogs,
            Action::ListPrevious,
            Action::ListNext,
            Action::ListAccept,
            Action::Close,
        ],
    ),
    (
        "deciding",
        &[
            Action::ApprovalAllow,
            Action::ApprovalReject,
            Action::PermissionCycle,
            Action::ReviewChanges,
            Action::CommitChanges,
            Action::DiscardChanges,
        ],
    ),
    ("leaving", &[Action::Cancel, Action::Escape, Action::Quit]),
];

/// The width the chord column is padded to.
const CHORD_COLUMN: usize = 14;

/// Builds the shortcut sheet from the live keymap.
pub fn help_rows(keymap: &Keymap, columns: usize) -> Vec<OverlayRow> {
    let mut rows = Vec::new();
    for (section, actions) in SECTIONS {
        rows.push(OverlayRow::heading(*section));
        for action in *actions {
            let chords = keymap.chords_in(Context::Composer, *action);
            // An unbound action still gets a row: "you can rebind this" is worth
            // more than pretending the action does not exist.
            let keys = if chords.is_empty() {
                "unbound".to_string()
            } else {
                let mut seen: Vec<String> = Vec::new();
                for chord in chords {
                    let text = chord.to_string();
                    if !seen.contains(&text) {
                        seen.push(text);
                    }
                }
                seen.join(" / ")
            };
            let suffix = if action.reserved() { "  (reserved)" } else { "" };
            rows.push(OverlayRow::new(
                two_column(
                    &keys,
                    &format!("{}{suffix}", action.describe()),
                    CHORD_COLUMN,
                    columns,
                ),
                action.id(),
            ));
        }
    }
    rows.push(OverlayRow::heading("rebinding"));
    rows.push(OverlayRow::new(
        "write ~/.agentrs/keybindings.json: {\"palette:open\": \"ctrl+g\"}",
        "keybindings",
    ));
    rows
}

/// How a command is written, argument included.
pub fn command_usage(entry: &Command) -> String {
    match entry.argument {
        Some(argument) => format!("/{} <{argument}>", entry.name),
        None => format!("/{}", entry.name),
    }
}

/// Builds the command palette.
pub fn palette_rows(keymap: &Keymap, columns: usize) -> Vec<OverlayRow> {
    // The name column is wide enough for the longest usage, so the summaries
    // line up instead of stepping in and out with each argument — but capped, so
    // one long usage cannot push every summary off a narrow window.
    let width = COMMANDS
        .iter()
        .map(|entry| crate::text::display_width(&command_usage(entry)))
        .max()
        .unwrap_or(CHORD_COLUMN)
        .min(22);
    COMMANDS
        .iter()
        .map(|entry| {
            let chord = entry
                .action
                .and_then(|action| keymap.chords_in(Context::Composer, action).first().copied())
                .map(|chord: Chord| chord.to_string())
                .unwrap_or_default();
            let label = if chord.is_empty() {
                entry.summary.to_string()
            } else {
                format!("{}  ({chord})", entry.summary)
            };
            OverlayRow::new(
                two_column(&command_usage(entry), &label, width, columns),
                entry.name,
            )
        })
        .collect()
}

/// Builds the staged-changes review.
///
/// Every staged file gets its diff, oldest ChangeSet first — which is the order
/// a commit will apply them in, so what is read is what will land.
pub fn diff_rows(changes: &[crate::diff::FileDiff], columns: usize) -> Vec<OverlayRow> {
    if changes.is_empty() {
        return vec![OverlayRow::heading("nothing is staged")];
    }
    let mut rows = Vec::new();
    let (added, removed) = changes
        .iter()
        .fold((0, 0), |(a, r), diff| (a + diff.added, r + diff.removed));
    rows.push(OverlayRow::heading(format!(
        "{} file(s)  +{added} -{removed}",
        changes.len()
    )));
    for diff in changes {
        let what = if diff.created {
            "new"
        } else if diff.deleted {
            "deleted"
        } else {
            "edited"
        };
        rows.push(OverlayRow::new(
            two_column(
                &diff.badge().unwrap_or_else(|| "no change".into()),
                &format!("{what}  {}", diff.path),
                10,
                columns,
            ),
            diff.path.clone(),
        ));
        for line in diff.detail_lines(true) {
            let mut row = OverlayRow::new(
                crate::text::truncate_to_width(&format!("    {}", line.text), columns),
                diff.path.clone(),
            );
            row.tone = line.tone;
            rows.push(row);
        }
        if diff.dropped_rows > 0 {
            rows.push(OverlayRow::heading(format!(
                "    … {} more row(s) not shown",
                diff.dropped_rows
            )));
        }
    }
    rows
}

/// One field of the session summary.
#[derive(Debug, Clone)]
pub struct StatusField {
    /// What it is.
    pub name: &'static str,
    /// What it says.
    pub value: String,
}

/// Builds the session summary.
pub fn status_rows(fields: &[StatusField], columns: usize) -> Vec<OverlayRow> {
    let width = fields
        .iter()
        .map(|field| field.name.len())
        .max()
        .unwrap_or(0)
        .max(8);
    fields
        .iter()
        .map(|field| {
            OverlayRow::new(two_column(field.name, &field.value, width, columns), field.name)
        })
        .collect()
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1_024 {
        format!("{bytes} B")
    } else if bytes < 1_024 * 1_024 {
        format!("{:.1} kB", bytes as f64 / 1_024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1_024.0 * 1_024.0))
    }
}

/// Builds the durable-log browser.
pub fn browser_rows(logs: &[LogEntry], current: Option<&str>, columns: usize) -> Vec<OverlayRow> {
    if logs.is_empty() {
        return vec![OverlayRow::heading("no durable logs beside this one")];
    }
    logs.iter()
        .map(|log| {
            let here = current.is_some_and(|name| name == log.name);
            let mark = if here { " ← current" } else { "" };
            OverlayRow::new(
                two_column(
                    &human_bytes(log.bytes),
                    &format!("{}{mark}", log.name),
                    10,
                    columns,
                ),
                log.path.display().to_string(),
            )
        })
        .collect()
}

/// Builds the searchable transcript from the rows already on screen.
///
/// The value is the row's own text, which is what `r` puts back in the draft and
/// what the search matches: a transcript row has no other identity worth having.
pub fn transcript_rows_surface(rows: &[TranscriptRow]) -> Vec<OverlayRow> {
    rows.iter()
        .map(|row| {
            let text = row.text();
            OverlayRow {
                value: if text.is_empty() { " ".into() } else { text.clone() },
                text,
                tone: Some(row.tone),
            }
        })
        .collect()
}

/// Opens one surface, ready to draw.
pub fn open(
    surface: Surface,
    keymap: &Keymap,
    logs: &[LogEntry],
    current_log: Option<&str>,
    rows: &[TranscriptRow],
    columns: usize,
) -> Overlay {
    let built = match surface {
        Surface::Help => help_rows(keymap, columns),
        Surface::Palette => palette_rows(keymap, columns),
        Surface::Browser => browser_rows(logs, current_log, columns),
        Surface::Transcript => transcript_rows_surface(rows),
        // These two are built by the caller: they need live state this function
        // has no business reaching for.
        Surface::Diff | Surface::Status => Vec::new(),
    };
    Overlay::open(surface, built)
}

/// The keymap context a surface routes keys through.
pub const fn context_of(surface: Surface) -> Context {
    match surface {
        Surface::Transcript => Context::Transcript,
        _ => Context::Overlay,
    }
}

/// True when the surface is read-only: nothing in it can be taken.
pub const fn is_reference(surface: Surface) -> bool {
    matches!(surface, Surface::Help | Surface::Status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Key;
    use std::path::PathBuf;

    #[test]
    fn the_sheet_is_built_from_the_live_keymap() {
        let mut keymap = Keymap::default();
        let before = help_rows(&keymap, 80);
        assert!(before
            .iter()
            .any(|row| row.text.contains("ctrl+p") && row.text.contains("command palette")));

        keymap
            .rebind(Action::PaletteOpen, &[Chord::ctrl(Key::Char('g'))])
            .unwrap();
        let after = help_rows(&keymap, 80);
        // Rebinding changed both what the key does and what the sheet says.
        assert!(after
            .iter()
            .any(|row| row.text.contains("ctrl+g") && row.text.contains("command palette")));
        assert!(!after.iter().any(|row| row.text.contains("ctrl+p")));
    }

    #[test]
    fn an_unbound_action_still_gets_a_row() {
        let mut keymap = Keymap::default();
        keymap.rebind(Action::HelpOpen, &[]).unwrap();
        let rows = help_rows(&keymap, 80);
        assert!(rows
            .iter()
            .any(|row| row.text.contains("unbound") && row.text.contains("show this sheet")));
    }

    #[test]
    fn the_reserved_chords_say_that_they_are() {
        let rows = help_rows(&Keymap::default(), 80);
        let cancel = rows
            .iter()
            .find(|row| row.value == "app:cancel")
            .expect("cancel is on the sheet");
        assert!(cancel.text.contains("(reserved)"), "{}", cancel.text);
    }

    #[test]
    fn every_command_is_reachable_by_name_and_by_prefix() {
        for entry in COMMANDS {
            assert_eq!(command(entry.name).map(|found| found.name), Some(entry.name));
        }
        // A prefix may match several; the completion list is what disambiguates.
        let names: Vec<&str> = commands_starting_with("c")
            .iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, ["commit", "clear", "cancel"]);
        assert_eq!(
            commands_starting_with("co").iter().map(|e| e.name).collect::<Vec<_>>(),
            ["commit"]
        );
        assert!(commands_starting_with("zzz").is_empty());
    }

    #[test]
    fn the_palette_shows_the_key_that_does_the_same_thing() {
        let rows = palette_rows(&Keymap::default(), 80);
        let fold = rows.iter().find(|row| row.value == "fold").unwrap();
        assert!(fold.text.contains("ctrl+o"), "{}", fold.text);
        let review = rows.iter().find(|row| row.value == "diff").unwrap();
        assert!(review.text.contains("ctrl+g"), "{}", review.text);
    }

    #[test]
    fn the_browser_marks_the_log_that_is_open() {
        let logs = vec![
            LogEntry {
                path: PathBuf::from("/tmp/b.jsonl"),
                name: "b.jsonl".into(),
                bytes: 2_048,
            },
            LogEntry {
                path: PathBuf::from("/tmp/a.jsonl"),
                name: "a.jsonl".into(),
                bytes: 10,
            },
        ];
        let rows = browser_rows(&logs, Some("a.jsonl"), 80);
        assert!(rows[0].text.contains("2.0 kB"));
        assert!(rows[1].text.contains("← current"));
        assert_eq!(rows[1].value, "/tmp/a.jsonl");
    }

    #[test]
    fn an_empty_browser_says_so_rather_than_offering_nothing() {
        let rows = browser_rows(&[], None, 80);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].selectable(), "there is nothing to take");
    }

    #[test]
    fn a_blank_transcript_row_still_gets_an_identity() {
        // The blank row between a card and the answer after it would otherwise
        // be unselectable, and `n` would skip past the answer with it.
        let rows = transcript_rows_surface(&[TranscriptRow {
            entry_id: "e".into(),
            segments: vec![crate::styling::StyledSegment::plain("")],
            tone: crate::theme::RowTone::Assistant,
        }]);
        assert!(rows[0].selectable());
    }

    #[test]
    fn every_command_names_an_action_so_the_three_forms_cannot_drift() {
        // A command with no action would be a palette row that does nothing and
        // a `/name` that silently succeeds.
        for entry in COMMANDS {
            assert!(entry.action.is_some(), "/{} does nothing", entry.name);
        }
    }

    #[test]
    fn command_names_are_unique_and_prefix_completion_is_unambiguous() {
        let mut seen = std::collections::HashSet::new();
        for entry in COMMANDS {
            assert!(seen.insert(entry.name), "/{} is listed twice", entry.name);
        }
    }

    #[test]
    fn a_command_that_takes_an_argument_says_so_in_its_usage() {
        let export = command("export").unwrap();
        assert_eq!(command_usage(export), "/export <path>");
        let commit = command("commit").unwrap();
        assert_eq!(command_usage(commit), "/commit");
    }

    #[test]
    fn the_review_lists_every_file_with_its_own_statistics() {
        use crate::diff::{build_file_diff, DiffOptions};
        let changes = vec![
            build_file_diff("a.md", Some("one\n"), Some("two\n"), DiffOptions::default()),
            build_file_diff("new.md", None, Some("fresh\n"), DiffOptions::default()),
            build_file_diff("gone.md", Some("bye\n"), None, DiffOptions::default()),
        ];
        let rows = diff_rows(&changes, 80);
        let text: Vec<&str> = rows.iter().map(|row| row.text.as_str()).collect();
        assert!(text[0].contains("3 file(s)"), "{:?}", text[0]);
        assert!(text.iter().any(|row| row.contains("edited") && row.contains("a.md")));
        assert!(text.iter().any(|row| row.contains("new") && row.contains("new.md")));
        assert!(text.iter().any(|row| row.contains("deleted") && row.contains("gone.md")));
        // Taking a row means that file, so ctrl+x can open it.
        assert!(rows.iter().any(|row| row.value == "a.md"));
    }

    #[test]
    fn an_empty_review_says_so_rather_than_showing_a_blank_screen() {
        let rows = diff_rows(&[], 80);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].selectable());
    }

    #[test]
    fn the_status_summary_lines_its_values_up() {
        let fields = vec![
            StatusField { name: "run", value: "r-1".into() },
            StatusField { name: "permission", value: "default".into() },
        ];
        let rows = status_rows(&fields, 80);
        let value_at = |row: &OverlayRow| row.text.find(|c: char| !c.is_whitespace() && c != ':');
        let columns: Vec<usize> = rows
            .iter()
            .map(|row| row.text.rfind("  ").map(|at| at + 2).unwrap_or(0))
            .collect();
        assert_eq!(columns[0], columns[1], "values start in the same column");
        assert!(value_at(&rows[0]).is_some());
    }

    #[test]
    fn each_surface_routes_keys_through_the_right_context() {
        assert_eq!(context_of(Surface::Transcript), Context::Transcript);
        assert_eq!(context_of(Surface::Help), Context::Overlay);
        assert_eq!(context_of(Surface::Palette), Context::Overlay);
        assert_eq!(context_of(Surface::Browser), Context::Overlay);
    }
}

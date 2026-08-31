// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/tool-card.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; card kind comes from an explicit
//            name table because AgentRS's ToolDef declares no presentation
//            field — unknown names stay Generic, as upstream requires.
//! Tool cards: what one call looks like on screen.
//!
//! A card is a header (`✓ Read a.md  [42 lines]`) and a body hung under a
//! gutter, so the whole call reads as one block. The header says what the call
//! did and how it went; the body is the evidence.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/tool-card.ts`, with one
//! deviation recorded here because it matters. The original insists the card
//! *kind* comes from the card the tool itself declared, never from its name —
//! a tool the profile has not seen keeps the plain tool colour and is never
//! collapsed. AgentRS's [`agentrs_types::ToolDef`] carries no presentation
//! field, so the kind comes from an explicit table below. The safe default is
//! preserved exactly: a name that is not in the table is [`CardKind::Generic`],
//! which is never toned and never collapsed.

use crate::diff::FileDiff;
use crate::glyphs::GlyphSet;
use crate::state::{ToolNode, ToolStatus};
use crate::styling::DetailLine;
use crate::text::sanitize_line;
use crate::theme::RowTone;

/// Body rows a successful card shows before it folds.
pub const SUCCESS_ROWS: usize = 3;
/// Body rows a diff shows: enough to read a small change whole.
pub const DIFF_ROWS: usize = 8;
/// Body rows a card that failed, was interrupted, or is still running shows.
///
/// Enough to read the error or watch progress, and still bounded, so a command
/// that dies after printing forty frames of stack does not cost forty rows.
pub const UNSETTLED_ROWS: usize = 8;

/// What a call does, which is the one thing worth a glance before the title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    /// Nothing more specific is known.
    Generic,
    /// Runs a command.
    Terminal,
    /// Changes files.
    Diff,
    /// Searches.
    Search,
    /// Reads.
    Read,
    /// Fetches over the network.
    Web,
}

impl CardKind {
    /// The tone the status glyph is painted in.
    ///
    /// `Generic` deliberately has none: an unrecognised tool keeps the plain
    /// tool colour rather than being given a meaning it never declared.
    pub const fn tone(self) -> Option<RowTone> {
        match self {
            Self::Generic => None,
            Self::Terminal => Some(RowTone::ToolTerminal),
            Self::Diff => Some(RowTone::ToolDiff),
            Self::Search => Some(RowTone::ToolSearch),
            Self::Read => Some(RowTone::ToolRead),
            Self::Web => Some(RowTone::ToolWeb),
        }
    }

    /// Whether a run of these may be folded into one card.
    ///
    /// Only reads and searches: *which* files were looked at matters more than
    /// their contents. A command's output, a diff and a fetched page are never
    /// hidden.
    pub const fn collapsible(self) -> bool {
        matches!(self, Self::Read | Self::Search)
    }

    /// Singular and plural names used by a collapsed run's header.
    pub const fn run_labels(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Read => Some(("read", "reads")),
            Self::Search => Some(("search", "searches")),
            _ => None,
        }
    }
}

/// Maps a tool name to what its call does.
///
/// The table is the whole of the mapping; anything absent is `Generic`.
pub fn kind_of(name: &str) -> CardKind {
    match name {
        "Read" | "View" | "Cat" => CardKind::Read,
        "Grep" | "Glob" | "Search" => CardKind::Search,
        "Write" | "Edit" | "Delete" | "MultiEdit" => CardKind::Diff,
        "Bash" | "Exec" | "Run" | "ExecCommand" => CardKind::Terminal,
        "WebFetch" | "WebSearch" | "Fetch" => CardKind::Web,
        _ => CardKind::Generic,
    }
}

/// A file the call touched, for `Ctrl+X` and for a collapsed run's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardLocation {
    /// Workspace-relative path as the model wrote it.
    pub path: String,
}

/// One call, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCard {
    /// What the call does.
    pub kind: CardKind,
    /// The header without its status glyph: `Read a.md`.
    pub title: String,
    /// The bracketed summary: `42 lines`, `+12 -4`, `exit 1`.
    pub badge: Option<String>,
    /// Present-tense phrase for the working line: `Reading a.md`.
    pub activity: String,
    /// Body rows, unfolded.
    pub body: Vec<DetailLine>,
    /// Files the call named.
    pub locations: Vec<CardLocation>,
    /// Body rows allowed before the card folds.
    pub fold_above_rows: usize,
}

/// Live enrichment a card can be given that the durable log does not hold.
#[derive(Debug, Clone, Default)]
pub struct CardExtras<'a> {
    /// The staged diff for a mutating call, computed from the live ChangeSet.
    pub diff: Option<&'a FileDiff>,
}

/// The status glyph for a call.
pub fn status_glyph(status: ToolStatus, glyphs: &GlyphSet) -> &'static str {
    match status {
        ToolStatus::Proposed | ToolStatus::AwaitingApproval | ToolStatus::Running => glyphs.pending,
        ToolStatus::Succeeded => glyphs.succeeded,
        ToolStatus::Failed | ToolStatus::Denied => glyphs.failed,
        ToolStatus::Canceled => glyphs.interrupted,
    }
}

/// The tone of the status glyph.
///
/// A failure is red whatever it was doing: the kind says what the call was for,
/// and that stops mattering the moment it did not happen.
pub fn status_tone(status: ToolStatus, kind: CardKind) -> Option<RowTone> {
    match status {
        ToolStatus::Failed | ToolStatus::Denied => Some(RowTone::Error),
        ToolStatus::Canceled => Some(RowTone::Warning),
        _ => kind.tone(),
    }
}

/// The first string-valued argument under any of `keys`.
fn argument<'a>(input: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(serde_json::Value::as_str))
}

/// The one argument worth putting in the header.
fn subject(name: &str, input: &serde_json::Value) -> Option<String> {
    let key = match kind_of(name) {
        CardKind::Search => &["pattern", "query", "path"][..],
        CardKind::Terminal => &["command", "cmd"][..],
        CardKind::Web => &["url"][..],
        _ => &["path", "file", "file_path"][..],
    };
    argument(input, key).map(sanitize_line)
}

/// The present-tense verb for the working line.
const fn verb(kind: CardKind) -> &'static str {
    match kind {
        CardKind::Read => "Reading",
        CardKind::Search => "Searching",
        CardKind::Diff => "Editing",
        CardKind::Terminal => "Running",
        CardKind::Web => "Fetching",
        CardKind::Generic => "Calling",
    }
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

/// What the bracketed badge says, in the tool's own terms.
fn badge_of(node: &ToolNode, kind: CardKind, extras: &CardExtras<'_>) -> Option<String> {
    match node.status {
        ToolStatus::Denied => Some(node.deny_code.clone().unwrap_or_else(|| "denied".into())),
        ToolStatus::Failed => Some("failed".into()),
        ToolStatus::Canceled => Some("interrupted".into()),
        ToolStatus::AwaitingApproval => Some("approval required".into()),
        ToolStatus::Proposed | ToolStatus::Running => None,
        ToolStatus::Succeeded => {
            // A live diff outranks a line count: `+12 -4` is what the reader
            // wants from an edit, and the tool's own prose says less.
            if let Some(diff) = extras.diff.and_then(FileDiff::badge) {
                return Some(diff);
            }
            if !node.artifacts.is_empty() {
                return Some(format!("{} artifact(s)", node.artifacts.len()));
            }
            let output = node.output.as_deref()?;
            match kind {
                CardKind::Read => Some(format!("{} lines", count_lines(output))),
                CardKind::Search => Some(format!("{} lines", count_lines(output))),
                _ => None,
            }
        }
    }
}

/// The body rows, before folding.
fn body_of(node: &ToolNode, extras: &CardExtras<'_>) -> Vec<DetailLine> {
    let mut rows = Vec::new();
    if let Some(diff) = extras.diff {
        rows.extend(diff.detail_lines(true));
        if diff.dropped_rows > 0 {
            rows.push(DetailLine::toned(
                format!("… {} more row(s) not shown (diff capped)", diff.dropped_rows),
                RowTone::System,
            ));
        }
    }
    if let Some(message) = &node.message {
        let tone = match node.status {
            ToolStatus::Denied | ToolStatus::Failed => RowTone::Error,
            _ => RowTone::Warning,
        };
        rows.extend(message.lines().map(|line| DetailLine::toned(line, tone)));
    }
    if let Some(output) = &node.output {
        rows.extend(output.lines().map(DetailLine::plain));
    }
    for artifact in &node.artifacts {
        rows.push(DetailLine::toned(
            format!("artifact: {artifact}"),
            RowTone::System,
        ));
    }
    if rows.is_empty() {
        if let Some(isolation) = &node.isolation {
            rows.push(DetailLine::toned(
                format!("isolation: {isolation}"),
                RowTone::System,
            ));
        }
    }
    rows
}

/// The files this call named.
fn locations_of(input: &serde_json::Value) -> Vec<CardLocation> {
    argument(input, &["path", "file", "file_path"])
        .map(|path| {
            vec![CardLocation {
                path: sanitize_line(path),
            }]
        })
        .unwrap_or_default()
}

/// Builds the card for one call.
pub fn build_tool_card(node: &ToolNode, extras: &CardExtras<'_>) -> ToolCard {
    let name = if node.name.is_empty() {
        node.call_id.clone()
    } else {
        node.name.clone()
    };
    let kind = kind_of(&name);
    let subject = subject(&name, &node.input);
    let title = match &subject {
        Some(subject) => format!("{name} {subject}"),
        None => name.clone(),
    };
    let activity = match &subject {
        Some(subject) => format!("{} {subject}", verb(kind)),
        None => format!("{} {name}", verb(kind)),
    };
    let fold_above_rows = if !node.status.settled() {
        UNSETTLED_ROWS
    } else if node.status.succeeded() {
        if extras.diff.is_some() {
            DIFF_ROWS
        } else {
            SUCCESS_ROWS
        }
    } else {
        UNSETTLED_ROWS
    };
    ToolCard {
        kind,
        title: sanitize_line(&title),
        badge: badge_of(node, kind, extras),
        activity: sanitize_line(&activity),
        body: body_of(node, extras),
        locations: locations_of(&node.input),
        fold_above_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{build_file_diff, DiffOptions};

    fn node(name: &str, input: serde_json::Value, status: ToolStatus) -> ToolNode {
        ToolNode {
            call_id: "c1".into(),
            name: name.into(),
            input,
            status,
            output: None,
            message: None,
            deny_code: None,
            artifacts: Vec::new(),
            isolation: None,
            execution_id: None,
            started_at_ms: None,
            ended_at_ms: None,
            turn: 1,
        }
    }

    fn card(node: &ToolNode) -> ToolCard {
        build_tool_card(node, &CardExtras::default())
    }

    #[test]
    fn an_unknown_tool_stays_generic_and_uncollapsible() {
        let built = card(&node("Teleport", serde_json::json!({}), ToolStatus::Succeeded));
        assert_eq!(built.kind, CardKind::Generic);
        assert_eq!(built.kind.tone(), None);
        assert!(!built.kind.collapsible());
        assert_eq!(built.title, "Teleport");
    }

    #[test]
    fn the_header_carries_the_one_argument_worth_reading() {
        let read = card(&node(
            "Read",
            serde_json::json!({"path": "src/a.md"}),
            ToolStatus::Succeeded,
        ));
        assert_eq!(read.title, "Read src/a.md");
        assert_eq!(read.activity, "Reading src/a.md");
        let grep = card(&node(
            "Grep",
            serde_json::json!({"pattern": "todo"}),
            ToolStatus::Running,
        ));
        assert_eq!(grep.title, "Grep todo");
        assert_eq!(grep.activity, "Searching todo");
    }

    #[test]
    fn a_failure_is_red_whatever_it_was_doing() {
        assert_eq!(
            status_tone(ToolStatus::Failed, CardKind::Read),
            Some(RowTone::Error)
        );
        assert_eq!(
            status_tone(ToolStatus::Succeeded, CardKind::Read),
            Some(RowTone::ToolRead)
        );
    }

    #[test]
    fn a_denial_keeps_its_stable_code_as_the_badge() {
        let mut denied = node("Write", serde_json::json!({"path": "a"}), ToolStatus::Denied);
        denied.deny_code = Some("PermissionMode".into());
        denied.message = Some("plan mode forbids writes".into());
        let built = card(&denied);
        assert_eq!(built.badge.as_deref(), Some("PermissionMode"));
        assert_eq!(built.body[0].tone, Some(RowTone::Error));
    }

    #[test]
    fn a_read_reports_how_much_came_back() {
        let mut read = node("Read", serde_json::json!({"path": "a"}), ToolStatus::Succeeded);
        read.output = Some("one\ntwo\nthree".into());
        assert_eq!(card(&read).badge.as_deref(), Some("3 lines"));
    }

    #[test]
    fn a_live_diff_outranks_the_tools_own_prose() {
        let mut write = node(
            "Write",
            serde_json::json!({"path": "a.md"}),
            ToolStatus::Succeeded,
        );
        write.output = Some("已写入 42 字节（未提交，位于 ChangeSet 内）".into());
        let diff = build_file_diff("a.md", Some("one\n"), Some("two\n"), DiffOptions::default());
        let built = build_tool_card(&write, &CardExtras { diff: Some(&diff) });
        assert_eq!(built.badge.as_deref(), Some("+1 -1"));
        // The diff rows come first; the tool's prose is still kept below them.
        assert!(built.body[0].text.starts_with('@'));
        assert!(built.body.iter().any(|row| row.text.contains("已写入")));
        assert_eq!(built.fold_above_rows, DIFF_ROWS);
    }

    #[test]
    fn an_unsettled_card_gets_the_wider_budget() {
        for status in [
            ToolStatus::Running,
            ToolStatus::AwaitingApproval,
            ToolStatus::Failed,
            ToolStatus::Canceled,
        ] {
            let built = card(&node("Read", serde_json::json!({}), status));
            assert_eq!(built.fold_above_rows, UNSETTLED_ROWS, "{status:?}");
        }
        let ok = card(&node("Read", serde_json::json!({}), ToolStatus::Succeeded));
        assert_eq!(ok.fold_above_rows, SUCCESS_ROWS);
    }

    #[test]
    fn locations_come_from_the_path_argument() {
        let edit = card(&node(
            "Edit",
            serde_json::json!({"path": "src/a.md", "old": "x", "new": "y"}),
            ToolStatus::Succeeded,
        ));
        assert_eq!(edit.locations.len(), 1);
        assert_eq!(edit.locations[0].path, "src/a.md");
        // A search names no file, so there is nothing to jump to.
        let grep = card(&node(
            "Grep",
            serde_json::json!({"pattern": "x"}),
            ToolStatus::Succeeded,
        ));
        assert!(grep.locations.is_empty());
    }

    #[test]
    fn a_large_output_is_reported_as_a_reference_not_as_prose() {
        let mut read = node("Read", serde_json::json!({"path": "a"}), ToolStatus::Succeeded);
        read.artifacts = vec!["abc123 · text/plain · 900000 bytes".into()];
        let built = card(&read);
        assert_eq!(built.badge.as_deref(), Some("1 artifact(s)"));
        assert!(built.body[0].text.starts_with("artifact:"));
    }

    #[test]
    fn a_card_with_no_name_yet_falls_back_to_its_call_id() {
        // A result replayed before its proposal has no tool name to show.
        let mut bare = node("", serde_json::json!({}), ToolStatus::Succeeded);
        bare.name = String::new();
        assert_eq!(card(&bare).title, "c1");
    }
}

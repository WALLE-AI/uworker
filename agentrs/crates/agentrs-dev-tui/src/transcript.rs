// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/transcript-view.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; scrollback splitting and the row
//            cache are dropped (this host keeps the alternate screen); entries
//            are rebuilt per frame.
//! The transcript: entries, folding, and the rows they become.
//!
//! An entry is one thing that happened — a message, a tool call, a marker, the
//! end of a turn. It has a header, which is always shown, and a body, which may
//! fold. Turning entries into rows is a separate step because it depends on the
//! terminal width, which changes, while the entries do not.
//!
//! Two rules from the original are load bearing and easy to lose:
//!
//! - **The fold budget is counted in terminal rows, not lines.** A card folds by
//!   what it would actually occupy at this width. That is what stops a single
//!   line of JSON — one line by any count, twenty rows on screen — from pushing
//!   the rest of the session out of view.
//! - **Prose is never folded for being long.** An assistant's answer is the
//!   reply, not evidence; only a tool card's body is evidence a reader dips into.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/transcript-view.ts`.

use std::collections::{HashMap, HashSet};

use crate::diff::FileDiff;
use crate::glyphs::GlyphSet;
use crate::markdown::render_markdown;
use crate::spinner::{format_elapsed, spinner_frame};
use crate::state::{AppState, Node, TextKind, ToolStatus};
use crate::styling::{wrap_styled, DetailLine, StyledSegment};
use crate::text::{display_width, wrap_words};
use crate::theme::RowTone;
use crate::tool_card::{build_tool_card, status_glyph, status_tone, CardExtras, CardKind, CardLocation};

/// What an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// The user's own message.
    User,
    /// Assistant prose.
    Assistant,
    /// Model reasoning.
    Reasoning,
    /// A durable marker.
    Marker,
    /// A tool call.
    Tool,
    /// The rule closing a turn.
    TurnRule,
}

/// One thing in the transcript, ready to be turned into rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    /// Stable identity, derived from what the entry is and never from where it
    /// sits: the transcript is rebuilt on every append, and a position-derived
    /// id would change every time anything before it moved.
    pub id: String,
    /// What the entry is.
    pub kind: EntryKind,
    /// The tone of the entry as a whole.
    pub tone: RowTone,
    /// The header row's styled runs, without the status glyph.
    pub header: Vec<StyledSegment>,
    /// The bracketed summary shown after the header.
    pub badge: Option<String>,
    /// Body rows.
    pub detail: Vec<DetailLine>,
    /// Whether the body may be folded at all.
    pub foldable: bool,
    /// Fold policy before any user override, judged on line count alone.
    pub folded_by_default: bool,
    /// Terminal rows the body may occupy before it folds regardless of how few
    /// logical lines it has. Only tool cards carry one.
    pub fold_above_rows: Option<usize>,
    /// Files the entry named.
    pub locations: Vec<CardLocation>,
    /// Lifecycle position, for a tool entry.
    pub status: Option<ToolStatus>,
    /// Tone of the status glyph, when it differs from the entry's own.
    pub status_tone: Option<RowTone>,
    /// What the call does, for a tool entry.
    pub card_kind: Option<CardKind>,
    /// How many entries this one stands for, when it is a collapsed run.
    pub collapsed_from: Option<usize>,
    /// Wall clock at which a running call started.
    pub started_at_ms: Option<u64>,
    /// Turn the entry belongs to.
    pub turn: usize,
}

impl TranscriptEntry {
    /// The plain header text, for search and for tests.
    pub fn header_text(&self) -> String {
        crate::styling::segment_text(&self.header)
    }
}

/// One rendered row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRow {
    /// The entry this row belongs to.
    pub entry_id: String,
    /// The row's styled runs.
    pub segments: Vec<StyledSegment>,
    /// The row's own tone, used for the fill and for dimming.
    pub tone: RowTone,
}

impl TranscriptRow {
    /// The plain text of the row.
    pub fn text(&self) -> String {
        crate::styling::segment_text(&self.segments)
    }
}

/// Everything the entry builder needs from outside.
#[derive(Debug, Clone, Copy)]
pub struct TranscriptOptions<'a> {
    /// Glyph set matching the terminal.
    pub glyphs: &'a GlyphSet,
    /// Host clock, or zero when replaying.
    pub now_ms: u64,
    /// Live diffs by call id. Absent on replay, which is the honest form.
    pub diffs: &'a HashMap<String, FileDiff>,
}

/// Builds the entries for one projection.
pub fn build_entries(state: &AppState, options: &TranscriptOptions<'_>) -> Vec<TranscriptEntry> {
    let glyphs = options.glyphs;
    let mut entries = Vec::with_capacity(state.nodes.len() + 1);
    // A preamble is an assistant block with a tool call still to come in the
    // same turn — the model saying what it is about to do.
    let tool_turns: HashSet<usize> = state
        .nodes
        .iter()
        .filter_map(|node| match node {
            Node::Tool(tool) => Some(tool.turn),
            _ => None,
        })
        .collect();

    for (index, node) in state.nodes.iter().enumerate() {
        match node {
            Node::Text(text) => {
                let followed_by_tool = tool_turns.contains(&text.turn)
                    && state.nodes[index + 1..].iter().any(|later| {
                        matches!(later, Node::Tool(tool) if tool.turn == text.turn)
                    });
                entries.push(text_entry(
                    &format!("text-{index}"),
                    text.role,
                    text.kind,
                    &text.text,
                    text.turn,
                    followed_by_tool,
                    glyphs,
                ));
            }
            Node::Tool(tool) => {
                let extras = CardExtras {
                    diff: options.diffs.get(&tool.call_id),
                };
                let card = build_tool_card(tool, &extras);
                let glyph = if tool.status == ToolStatus::Running && options.now_ms > 0 {
                    // A call in flight shows a turning frame in place of its
                    // status glyph.
                    spinner_frame(options.now_ms, glyphs.pending != ">")
                } else {
                    status_glyph(tool.status, glyphs)
                };
                let tone = status_tone(tool.status, card.kind).unwrap_or(RowTone::Tool);
                let mut header = vec![
                    StyledSegment::toned(format!("{glyph} "), tone),
                    StyledSegment::plain(card.title.clone()),
                ];
                if let Some(elapsed) = tool.elapsed_ms(options.now_ms) {
                    if !tool.status.settled() {
                        header.push(StyledSegment::toned(
                            format!(" · {}", format_elapsed(elapsed)),
                            RowTone::System,
                        ));
                    }
                }
                entries.push(TranscriptEntry {
                    id: format!("tool-{}", tool.call_id),
                    kind: EntryKind::Tool,
                    tone: RowTone::Tool,
                    header,
                    badge: card.badge,
                    detail: card.body,
                    foldable: true,
                    folded_by_default: false,
                    fold_above_rows: Some(card.fold_above_rows),
                    locations: card.locations,
                    status: Some(tool.status),
                    status_tone: Some(tone),
                    card_kind: Some(card.kind),
                    collapsed_from: None,
                    started_at_ms: tool.started_at_ms,
                    turn: tool.turn,
                });
            }
            Node::Marker(marker) => entries.push(TranscriptEntry {
                id: format!("marker-{index}"),
                kind: EntryKind::Marker,
                tone: RowTone::System,
                header: vec![
                    StyledSegment::toned(format!("{} ", glyphs.marker), RowTone::System),
                    StyledSegment::toned(marker.text.clone(), RowTone::System),
                ],
                badge: None,
                detail: Vec::new(),
                foldable: false,
                folded_by_default: false,
                fold_above_rows: None,
                locations: Vec::new(),
                status: None,
                status_tone: None,
                card_kind: None,
                collapsed_from: None,
                started_at_ms: None,
                turn: marker.turn,
            }),
            Node::TurnEnd { turn } => entries.push(TranscriptEntry {
                id: format!("turn-{index}"),
                kind: EntryKind::TurnRule,
                tone: RowTone::System,
                header: vec![StyledSegment::toned(
                    format!("{} turn {turn} ", glyphs.rule.repeat(2)),
                    RowTone::System,
                )
                .dimmed()],
                badge: None,
                detail: Vec::new(),
                foldable: false,
                folded_by_default: false,
                fold_above_rows: None,
                locations: Vec::new(),
                status: None,
                status_tone: None,
                card_kind: None,
                collapsed_from: None,
                started_at_ms: None,
                turn: *turn,
            }),
        }
    }

    // Live reasoning comes before the live answer, in the order it was said.
    if !state.streaming_thinking.is_empty() {
        entries.push(text_entry(
            "streaming-thinking",
            agentrs_types::Role::Assistant,
            TextKind::Reasoning,
            &state.streaming_thinking,
            state.turn,
            false,
            glyphs,
        ));
    }
    if !state.streaming.is_empty() {
        entries.push(text_entry(
            "streaming",
            agentrs_types::Role::Assistant,
            TextKind::Prose,
            &state.streaming,
            state.turn,
            false,
            glyphs,
        ));
    }
    entries
}

fn text_entry(
    id: &str,
    role: agentrs_types::Role,
    kind: TextKind,
    text: &str,
    turn: usize,
    preamble: bool,
    glyphs: &GlyphSet,
) -> TranscriptEntry {
    use agentrs_types::Role;
    let (entry_kind, tone, marker) = match (role, kind) {
        (_, TextKind::Reasoning) => (EntryKind::Reasoning, RowTone::Reasoning, glyphs.thinking),
        (Role::User, _) => (EntryKind::User, RowTone::User, glyphs.user),
        (Role::Assistant, _) => (EntryKind::Assistant, RowTone::Assistant, glyphs.bullet),
        _ => (EntryKind::Marker, RowTone::System, glyphs.marker),
    };

    // Reasoning is folded behind a fixed header: it is context, not the answer.
    if entry_kind == EntryKind::Reasoning {
        return TranscriptEntry {
            id: id.into(),
            kind: entry_kind,
            tone,
            header: vec![StyledSegment::toned(format!("{marker} Thinking"), tone)],
            badge: None,
            detail: text.lines().map(|line| DetailLine::toned(line, tone)).collect(),
            foldable: true,
            folded_by_default: true,
            fold_above_rows: None,
            locations: Vec::new(),
            status: None,
            status_tone: None,
            card_kind: None,
            collapsed_from: None,
            started_at_ms: None,
            turn,
        };
    }

    // The user's own words are shown as written; only the assistant's are
    // markdown. Rendering the user's text would consume delimiters they typed
    // on purpose.
    let lines: Vec<(String, Vec<StyledSegment>)> = if entry_kind == EntryKind::Assistant {
        render_markdown(text, glyphs)
            .into_iter()
            .map(|line| (line.text, line.segments))
            .collect()
    } else {
        text.lines()
            .map(|line| (line.to_string(), vec![StyledSegment::plain(line)]))
            .collect()
    };

    let mut header = vec![StyledSegment::toned(format!("{marker} "), tone)];
    let mut detail = Vec::new();
    for (index, (plain, segments)) in lines.into_iter().enumerate() {
        if index == 0 {
            header.extend(segments);
        } else {
            detail.push(DetailLine {
                text: plain,
                tone: None,
                segments: Some(segments),
            });
        }
    }

    let folds = preamble && turn > 1 && detail_rows_worth_folding(&detail);
    TranscriptEntry {
        id: id.into(),
        kind: entry_kind,
        tone,
        header,
        badge: None,
        detail,
        // In the first turn a preamble is shown in full, because it is
        // orientation and often carries the caveat the answer rests on. From the
        // second turn on the same narration folds, since by then the routine is
        // known — and only when folding buys a row.
        foldable: preamble,
        folded_by_default: folds,
        fold_above_rows: None,
        locations: Vec::new(),
        status: None,
        status_tone: None,
        card_kind: None,
        collapsed_from: None,
        started_at_ms: None,
        turn,
    }
}

/// A fold that hides one row costs that row back to say so.
fn detail_rows_worth_folding(detail: &[DetailLine]) -> bool {
    detail.len() >= 2
}

/// How many terminal rows a body would occupy at this width.
pub fn body_rows(detail: &[DetailLine], columns: usize) -> usize {
    detail
        .iter()
        .map(|line| wrap_words(&line.text, columns.max(1)).len())
        .sum()
}

/// Whether an entry folds at this width before any user override.
///
/// The budget is in terminal rows, which is why the width is a parameter: the
/// same card folds on a narrow window and does not on a wide one.
pub fn folded_by_default_at(entry: &TranscriptEntry, columns: usize) -> bool {
    if !entry.foldable {
        return false;
    }
    if entry.folded_by_default {
        return true;
    }
    let Some(budget) = entry.fold_above_rows else {
        return false;
    };
    let rows = body_rows(&entry.detail, columns);
    // Never fold to hide a single row: saying so would cost that row back.
    rows > budget && rows.saturating_sub(budget) > 1
}

/// Whether an entry is folded now, given the user's overrides.
pub fn entry_folded(
    entry: &TranscriptEntry,
    columns: usize,
    toggled: &HashSet<String>,
) -> bool {
    let default = folded_by_default_at(entry, columns);
    if toggled.contains(&entry.id) {
        !default
    } else {
        default
    }
}

/// Turns entries into rows for a terminal `columns` wide.
pub fn transcript_rows(
    entries: &[TranscriptEntry],
    columns: usize,
    toggled: &HashSet<String>,
    glyphs: &GlyphSet,
) -> Vec<TranscriptRow> {
    let columns = columns.max(1);
    let mut rows = Vec::new();
    let mut previous_was_tool = false;
    for entry in entries {
        // A blank row separates a tool card from the answer that follows it, so
        // a short reply is not read as part of the card above it.
        if previous_was_tool && entry.kind != EntryKind::Tool {
            rows.push(TranscriptRow {
                entry_id: entry.id.clone(),
                segments: vec![StyledSegment::plain("")],
                tone: RowTone::Assistant,
            });
        }
        previous_was_tool = entry.kind == EntryKind::Tool;

        let mut header = entry.header.clone();
        if let Some(badge) = &entry.badge {
            header.push(StyledSegment::toned(format!("  [{badge}]"), RowTone::Badge));
        }
        for row in wrap_styled(&header, columns) {
            rows.push(TranscriptRow {
                entry_id: entry.id.clone(),
                segments: row,
                tone: entry.tone,
            });
        }

        if entry.detail.is_empty() {
            continue;
        }
        let gutter = entry.kind == EntryKind::Tool;
        let (first, rest) = if gutter {
            (glyphs.gutter_first, glyphs.gutter_rest)
        } else {
            ("", "")
        };
        let indent = display_width(rest);
        if entry_folded(entry, columns, toggled) {
            let hidden = body_rows(&entry.detail, columns.saturating_sub(indent).max(1));
            let label = crate::text::truncate_to_width(
                &format!("{} {hidden} more row(s) — ctrl+o", glyphs.fold),
                columns.saturating_sub(indent).max(1),
            );
            rows.push(TranscriptRow {
                entry_id: entry.id.clone(),
                segments: vec![
                    StyledSegment::toned(first.to_string(), RowTone::System),
                    StyledSegment::toned(label, RowTone::System).dimmed(),
                ],
                tone: RowTone::System,
            });
            continue;
        }
        let mut first_body_row = true;
        for line in &entry.detail {
            let segments = line
                .segments
                .clone()
                .unwrap_or_else(|| vec![StyledSegment::plain(line.text.clone())]);
            let segments = match line.tone {
                Some(tone) => segments
                    .into_iter()
                    .map(|mut segment| {
                        if segment.tone.is_none() {
                            segment.tone = Some(tone);
                        }
                        segment
                    })
                    .collect(),
                None => segments,
            };
            for wrapped in wrap_styled(&segments, columns.saturating_sub(indent).max(1)) {
                let prefix = if first_body_row { first } else { rest };
                first_body_row = false;
                let mut row = Vec::with_capacity(wrapped.len() + 1);
                if !prefix.is_empty() {
                    row.push(StyledSegment::toned(prefix.to_string(), RowTone::System).dimmed());
                }
                row.extend(wrapped);
                rows.push(TranscriptRow {
                    entry_id: entry.id.clone(),
                    segments: row,
                    tone: line.tone.unwrap_or(entry.tone),
                });
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyphs::UNICODE_GLYPHS;
    use crate::state::{MarkerNode, TextNode, ToolNode};
    use agentrs_types::Role;

    fn options<'a>(diffs: &'a HashMap<String, FileDiff>) -> TranscriptOptions<'a> {
        TranscriptOptions {
            glyphs: &UNICODE_GLYPHS,
            now_ms: 0,
            diffs,
        }
    }

    fn tool(call: &str, name: &str, status: ToolStatus, output: Option<&str>) -> Node {
        Node::Tool(Box::new(ToolNode {
            call_id: call.into(),
            name: name.into(),
            input: serde_json::json!({"path": "a.md"}),
            status,
            output: output.map(str::to_string),
            message: None,
            deny_code: None,
            artifacts: Vec::new(),
            isolation: None,
            execution_id: None,
            started_at_ms: None,
            ended_at_ms: None,
            turn: 1,
        }))
    }

    fn text(role: Role, body: &str, turn: usize) -> Node {
        Node::Text(TextNode {
            role,
            kind: TextKind::Prose,
            text: body.into(),
            turn,
        })
    }

    fn state_with(nodes: Vec<Node>) -> AppState {
        let mut state = AppState::default();
        state.nodes = nodes;
        state
    }

    fn entries_of(state: &AppState) -> Vec<TranscriptEntry> {
        let diffs = HashMap::new();
        build_entries(state, &options(&diffs))
    }

    #[test]
    fn each_role_gets_its_own_marker() {
        let state = state_with(vec![
            text(Role::User, "do it", 1),
            text(Role::Assistant, "done", 1),
            Node::Marker(MarkerNode {
                text: "compacted".into(),
                turn: 1,
            }),
            Node::TurnEnd { turn: 1 },
        ]);
        let headers: Vec<String> = entries_of(&state)
            .iter()
            .map(TranscriptEntry::header_text)
            .collect();
        assert_eq!(headers[0], "> do it");
        assert_eq!(headers[1], "● done");
        assert_eq!(headers[2], "• compacted");
        assert!(headers[3].starts_with("──"));
    }

    #[test]
    fn reasoning_is_folded_behind_a_fixed_header() {
        // It is context, not the answer; `ctrl+o` opens it.
        let mut state = state_with(vec![Node::Text(TextNode {
            role: Role::Assistant,
            kind: TextKind::Reasoning,
            text: "first I check the units\nthen the arithmetic".into(),
            turn: 1,
        })]);
        state.streaming_thinking = "still weighing".into();
        let entries = entries_of(&state);
        assert_eq!(entries[0].header_text(), "∴ Thinking");
        assert!(entries[0].folded_by_default);
        assert_eq!(entries[0].detail.len(), 2, "the body is the reasoning itself");
        // The live stream gets its own entry, before any live answer.
        assert_eq!(entries[1].id, "streaming-thinking");
        assert_eq!(entries[1].kind, EntryKind::Reasoning);
    }

    #[test]
    fn prose_is_never_folded_for_being_long() {
        let long = (1..=40).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        let state = state_with(vec![text(Role::Assistant, &long, 1)]);
        let entry = &entries_of(&state)[0];
        assert!(!entry.foldable);
        assert!(!folded_by_default_at(entry, 80));
    }

    #[test]
    fn a_preamble_folds_from_the_second_turn_on() {
        let mut nodes = vec![text(
            Role::Assistant,
            "First I will\nread the file\nand say so",
            1,
        )];
        nodes.push(tool("c1", "Read", ToolStatus::Succeeded, None));
        let first_turn = entries_of(&state_with(nodes));
        // Turn one is orientation, and is shown in full.
        assert!(!folded_by_default_at(&first_turn[0], 80));

        let mut later = vec![text(
            Role::Assistant,
            "Again I will\nread the file\nand say so",
            2,
        )];
        later.push(tool("c2", "Read", ToolStatus::Succeeded, None));
        if let Node::Tool(node) = &mut later[1] {
            node.turn = 2;
        }
        let second_turn = entries_of(&state_with(later));
        assert!(folded_by_default_at(&second_turn[0], 80));
    }

    #[test]
    fn a_one_line_preamble_stays_as_it_is() {
        // Folding it would buy nothing and cost the row back to say so.
        let mut nodes = vec![text(Role::Assistant, "Reading now.", 2)];
        nodes.push(tool("c1", "Read", ToolStatus::Succeeded, None));
        if let Node::Tool(node) = &mut nodes[1] {
            node.turn = 2;
        }
        assert!(!folded_by_default_at(&entries_of(&state_with(nodes))[0], 80));
    }

    #[test]
    fn the_fold_budget_is_counted_in_terminal_rows() {
        // One logical line, wide enough to occupy many rows on a narrow window.
        let one_long_line = "x".repeat(400);
        let state = state_with(vec![tool(
            "c1",
            "Read",
            ToolStatus::Succeeded,
            Some(&one_long_line),
        )]);
        let entry = &entries_of(&state)[0];
        assert_eq!(entry.detail.len(), 1, "one logical line");
        assert!(folded_by_default_at(entry, 40), "twenty rows at forty columns");
        assert!(!folded_by_default_at(entry, 500), "one row at five hundred");
    }

    #[test]
    fn a_card_never_folds_to_hide_a_single_row() {
        let four = "a\nb\nc\nd";
        let state = state_with(vec![tool("c1", "Read", ToolStatus::Succeeded, Some(four))]);
        // Four rows against a budget of three: hiding one costs one to say so.
        assert!(!folded_by_default_at(&entries_of(&state)[0], 80));
    }

    #[test]
    fn a_toggle_inverts_whatever_the_default_was() {
        let long = (1..=20).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let state = state_with(vec![tool("c1", "Read", ToolStatus::Succeeded, Some(&long))]);
        let entry = &entries_of(&state)[0];
        let mut toggled = HashSet::new();
        assert!(entry_folded(entry, 80, &toggled));
        toggled.insert(entry.id.clone());
        assert!(!entry_folded(entry, 80, &toggled));
    }

    #[test]
    fn a_card_body_hangs_under_one_gutter() {
        let state = state_with(vec![tool(
            "c1",
            "Read",
            ToolStatus::Succeeded,
            Some("one\ntwo"),
        )]);
        let entries = entries_of(&state);
        let rows = transcript_rows(&entries, 80, &HashSet::new(), &UNICODE_GLYPHS);
        let texts: Vec<String> = rows.iter().map(TranscriptRow::text).collect();
        assert_eq!(texts[0], "✓ Read a.md  [2 lines]");
        assert_eq!(texts[1], " ⎿ one");
        assert_eq!(texts[2], "   two");
    }

    #[test]
    fn a_blank_row_separates_a_card_from_the_answer_after_it() {
        let state = state_with(vec![
            tool("c1", "Read", ToolStatus::Succeeded, None),
            text(Role::Assistant, "It says hello.", 1),
        ]);
        let entries = entries_of(&state);
        let texts: Vec<String> = transcript_rows(&entries, 80, &HashSet::new(), &UNICODE_GLYPHS)
            .iter()
            .map(TranscriptRow::text)
            .collect();
        let answer = texts.iter().position(|row| row.contains("It says")).unwrap();
        assert_eq!(texts[answer - 1], "");
    }

    #[test]
    fn a_folded_card_says_how_much_it_is_hiding() {
        let long = (1..=20).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let state = state_with(vec![tool("c1", "Read", ToolStatus::Succeeded, Some(&long))]);
        let entries = entries_of(&state);
        let texts: Vec<String> = transcript_rows(&entries, 80, &HashSet::new(), &UNICODE_GLYPHS)
            .iter()
            .map(TranscriptRow::text)
            .collect();
        assert!(texts[1].contains("20 more row(s)"), "{:?}", texts[1]);
    }

    #[test]
    fn every_row_fits_the_terminal() {
        let long = "一段很长的中文说明 ".repeat(20);
        let state = state_with(vec![
            text(Role::Assistant, &long, 1),
            tool("c1", "Read", ToolStatus::Succeeded, Some(&long)),
        ]);
        let entries = entries_of(&state);
        for columns in [20, 40, 80] {
            for row in transcript_rows(&entries, columns, &HashSet::new(), &UNICODE_GLYPHS) {
                assert!(
                    display_width(&row.text()) <= columns,
                    "columns={columns} row={:?}",
                    row.text()
                );
            }
        }
    }

    #[test]
    fn a_running_call_turns_and_reports_how_long() {
        let mut node = tool("c1", "Read", ToolStatus::Running, None);
        if let Node::Tool(tool) = &mut node {
            tool.started_at_ms = Some(1_000);
        }
        let state = state_with(vec![node]);
        let diffs = HashMap::new();
        let entries = build_entries(
            &state,
            &TranscriptOptions {
                glyphs: &UNICODE_GLYPHS,
                now_ms: 13_000,
                diffs: &diffs,
            },
        );
        let header = entries[0].header_text();
        assert!(header.ends_with("· 12s"), "{header}");
        assert!(!header.starts_with('▸'), "a running call turns: {header}");
    }

    #[test]
    fn entry_ids_do_not_move_when_something_before_them_does() {
        let with_one = state_with(vec![tool("c1", "Read", ToolStatus::Succeeded, None)]);
        let with_two = state_with(vec![
            text(Role::User, "first", 1),
            tool("c1", "Read", ToolStatus::Succeeded, None),
        ]);
        let a = entries_of(&with_one)[0].id.clone();
        let b = entries_of(&with_two)[1].id.clone();
        assert_eq!(a, b);
    }
}

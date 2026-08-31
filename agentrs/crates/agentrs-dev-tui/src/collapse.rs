// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/collapse.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; the rule list is one rule, applied
//            directly, rather than a registry.
//! Collapsing runs of entries into one.
//!
//! A turn that greps once and then reads eight files spends nine cards and forty
//! rows saying so, and pushes the answer it was working towards off the screen.
//! None of those cards is wrong; there are just too many of them for what they
//! are worth, which is "it looked at these files".
//!
//! So a run of them becomes one entry whose body lists what was touched. The
//! result is an ordinary foldable card — `ctrl+o` opens it like any other —
//! which is why this file adds no fold machinery of its own.
//!
//! Three rules keep it honest, and they are the whole design:
//!
//! - **Only settled, only successful.** A call that is still running has a
//!   moving status glyph, and a call that failed is the one thing on screen
//!   worth reading. Neither is ever hidden.
//! - **Only kinds whose body is not the point.** A read and a search are
//!   collapsible because *which* files matters more than their contents; a
//!   command's output, a diff and a fetched page are not.
//! - **Only a real run.** Below [`MIN_RUN`] a group saves nothing and costs a
//!   keypress, so a lone read is left exactly as it was.
//!
//! A group's id comes from its first member, never from its position: the
//! transcript is rebuilt on every append, and a position-derived id would change
//! identity every time anything before it moved.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/collapse.ts`.

use crate::glyphs::GlyphSet;
use crate::state::ToolStatus;
use crate::styling::{DetailLine, StyledSegment};
use crate::theme::RowTone;
use crate::tool_card::{CardKind, CardLocation};
use crate::transcript::{EntryKind, TranscriptEntry};

/// Fewer than this many in a row is not a run, and is left alone.
pub const MIN_RUN: usize = 3;

/// True when this entry may join a run at all.
fn collapsible(entry: Option<&TranscriptEntry>) -> bool {
    entry.is_some_and(|entry| {
        entry.kind == EntryKind::Tool
            && entry.status == Some(ToolStatus::Succeeded)
            && entry.card_kind.is_some_and(CardKind::collapsible)
            // A group that has already been collapsed is not re-collapsed.
            && entry.collapsed_from.is_none()
    })
}

/// `8 reads · 2 searches`, in the order the kinds first appeared.
fn summarize(group: &[TranscriptEntry]) -> String {
    let mut counts: Vec<(CardKind, usize)> = Vec::new();
    for entry in group {
        let Some(kind) = entry.card_kind else { continue };
        match counts.iter_mut().find(|(seen, _)| *seen == kind) {
            Some((_, count)) => *count += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(kind, count)| match kind.run_labels() {
            Some((one, many)) => format!("{count} {}", if count == 1 { one } else { many }),
            None => count.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The member's title without its own status glyph.
///
/// Every member succeeded — the group header says so once, and eight ticks
/// under it are noise.
fn member_label(entry: &TranscriptEntry) -> String {
    let header = entry.header_text();
    let title = header
        .split_once(' ')
        .map_or(header.as_str(), |(_, rest)| rest)
        .to_string();
    match &entry.badge {
        Some(badge) => format!("{title}  [{badge}]"),
        None => title,
    }
}

fn deduped_locations(group: &[TranscriptEntry]) -> Vec<CardLocation> {
    let mut seen = Vec::new();
    for entry in group {
        for location in &entry.locations {
            if !seen.iter().any(|kept: &CardLocation| kept.path == location.path) {
                seen.push(location.clone());
            }
        }
    }
    seen
}

fn fold(group: &[TranscriptEntry], glyphs: &GlyphSet) -> TranscriptEntry {
    let first = &group[0];
    TranscriptEntry {
        id: format!("run-{}", first.id),
        kind: EntryKind::Tool,
        tone: RowTone::Tool,
        header: vec![
            StyledSegment::toned(
                format!("{} ", glyphs.succeeded),
                first.status_tone.unwrap_or(RowTone::Tool),
            ),
            StyledSegment::plain(summarize(group)),
        ],
        badge: None,
        detail: group.iter().map(|entry| DetailLine::plain(member_label(entry))).collect(),
        foldable: true,
        // A group is folded by default: the whole point is that it costs one row
        // rather than one per member.
        folded_by_default: true,
        fold_above_rows: None,
        locations: deduped_locations(group),
        status: Some(ToolStatus::Succeeded),
        status_tone: first.status_tone,
        card_kind: first.card_kind,
        collapsed_from: Some(group.len()),
        started_at_ms: None,
        turn: first.turn,
    }
}

/// Folds every run of successful look-ups into one entry.
///
/// The tool count in the status row keeps counting what ran, not what is shown:
/// this changes the transcript, never the totals.
pub fn collapse_runs(entries: Vec<TranscriptEntry>, glyphs: &GlyphSet) -> Vec<TranscriptEntry> {
    let mut out = Vec::with_capacity(entries.len());
    let mut index = 0;
    while index < entries.len() {
        let mut length = 0;
        while collapsible(entries.get(index + length)) {
            length += 1;
        }
        if length >= MIN_RUN {
            out.push(fold(&entries[index..index + length], glyphs));
            index += length;
        } else {
            out.push(entries[index].clone());
            index += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyphs::UNICODE_GLYPHS;

    fn entry(id: &str, kind: CardKind, status: ToolStatus, title: &str) -> TranscriptEntry {
        TranscriptEntry {
            id: id.into(),
            kind: EntryKind::Tool,
            tone: RowTone::Tool,
            header: vec![
                StyledSegment::toned("✓ ", RowTone::ToolRead),
                StyledSegment::plain(title),
            ],
            badge: None,
            detail: vec![DetailLine::plain("body")],
            foldable: true,
            folded_by_default: false,
            fold_above_rows: Some(3),
            locations: vec![CardLocation {
                path: title.split(' ').next_back().unwrap_or("").into(),
            }],
            status: Some(status),
            status_tone: Some(RowTone::ToolRead),
            card_kind: Some(kind),
            collapsed_from: None,
            started_at_ms: None,
            turn: 1,
        }
    }

    fn reads(count: usize) -> Vec<TranscriptEntry> {
        (0..count)
            .map(|index| {
                entry(
                    &format!("t{index}"),
                    CardKind::Read,
                    ToolStatus::Succeeded,
                    &format!("Read f{index}.md"),
                )
            })
            .collect()
    }

    fn collapse(entries: Vec<TranscriptEntry>) -> Vec<TranscriptEntry> {
        collapse_runs(entries, &UNICODE_GLYPHS)
    }

    #[test]
    fn a_run_of_three_becomes_one_card() {
        let out = collapse(reads(3));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].header_text(), "✓ 3 reads");
        assert_eq!(out[0].collapsed_from, Some(3));
        assert_eq!(out[0].detail.len(), 3);
    }

    #[test]
    fn a_lone_look_up_is_left_exactly_as_it_was() {
        for count in [1, 2] {
            let out = collapse(reads(count));
            assert_eq!(out.len(), count);
            assert!(out.iter().all(|entry| entry.collapsed_from.is_none()));
        }
    }

    #[test]
    fn kinds_are_counted_in_the_order_they_first_appeared() {
        let mut entries = reads(3);
        entries.push(entry("s1", CardKind::Search, ToolStatus::Succeeded, "Grep todo"));
        entries.push(entry("s2", CardKind::Search, ToolStatus::Succeeded, "Grep fixme"));
        let out = collapse(entries);
        assert_eq!(out[0].header_text(), "✓ 3 reads · 2 searches");
    }

    #[test]
    fn a_failure_breaks_the_run_and_is_never_hidden() {
        let mut entries = reads(2);
        entries.push(entry("bad", CardKind::Read, ToolStatus::Failed, "Read x.md"));
        entries.extend(reads(3));
        let out = collapse(entries);
        // The two before it stay; the failure stays; only the last three fold.
        assert_eq!(out.len(), 4);
        assert_eq!(out[2].status, Some(ToolStatus::Failed));
        assert_eq!(out[3].collapsed_from, Some(3));
    }

    #[test]
    fn a_running_call_is_never_hidden() {
        let mut entries = reads(3);
        entries[1].status = Some(ToolStatus::Running);
        assert_eq!(collapse(entries).len(), 3);
    }

    #[test]
    fn only_look_ups_collapse() {
        let entries: Vec<TranscriptEntry> = (0..4)
            .map(|index| {
                entry(
                    &format!("d{index}"),
                    CardKind::Diff,
                    ToolStatus::Succeeded,
                    "Edit a.md",
                )
            })
            .collect();
        assert_eq!(collapse(entries).len(), 4);
    }

    #[test]
    fn an_unknown_tool_is_left_exactly_as_it_was() {
        let entries: Vec<TranscriptEntry> = (0..4)
            .map(|index| {
                entry(
                    &format!("g{index}"),
                    CardKind::Generic,
                    ToolStatus::Succeeded,
                    "Teleport",
                )
            })
            .collect();
        assert_eq!(collapse(entries).len(), 4);
    }

    #[test]
    fn the_group_id_comes_from_its_first_member() {
        let out = collapse(reads(4));
        assert_eq!(out[0].id, "run-t0");
        // Prepending something must not change the group's identity.
        let mut with_prefix = vec![entry(
            "e",
            CardKind::Diff,
            ToolStatus::Succeeded,
            "Edit a.md",
        )];
        with_prefix.extend(reads(4));
        assert_eq!(collapse(with_prefix)[1].id, "run-t0");
    }

    #[test]
    fn the_body_lists_what_was_touched_without_repeating_the_tick() {
        let out = collapse(reads(3));
        assert_eq!(out[0].detail[0].text, "Read f0.md");
        assert_eq!(out[0].locations.len(), 3);
    }

    #[test]
    fn a_group_is_folded_by_default_but_opens_like_any_card() {
        let out = collapse(reads(5));
        assert!(out[0].foldable);
        assert!(out[0].folded_by_default);
    }
}

// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/status-line.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; segments differ where AgentRS reports
//            different things: no todo or subagent projection, plus ChangeSet
//            and dropped-live-event fields.
//! Status row model.
//!
//! The row is built as segments with an explicit drop order rather than wrapped
//! or truncated as one string: on a narrow terminal the fields that fall away
//! should be the ones you can reconstruct (the session title, the log path),
//! never the permission preset or the context pressure.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/status-line.ts`. The
//! segments differ where AgentRS reports different things — there is no todo
//! projection and no subagent to report, and there is a ChangeSet and a dropped
//! live-event count, which `dsh` has no equivalent of.

use crate::state::{Counters, RunStatus};
use crate::text::{display_width, truncate_to_width};

/// One field of the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSegment {
    /// What it says.
    pub text: String,
    /// Higher drops first when the row does not fit.
    pub priority: u8,
}

impl StatusSegment {
    fn new(text: impl Into<String>, priority: u8) -> Self {
        Self {
            text: text.into(),
            priority,
        }
    }
}

/// Context pressure, when there is a window to measure against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBar {
    /// Tokens used.
    pub used: u64,
    /// Tokens the window holds.
    pub total: u64,
}

/// The row, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusModel {
    /// Context pressure, when the projection reported a window.
    pub bar: Option<ContextBar>,
    /// Left-hand fields: what the run is and what it has done.
    pub left: Vec<StatusSegment>,
    /// Right-hand fields: where it is.
    pub right: Vec<StatusSegment>,
    /// What the next key does.
    pub hint: String,
}

/// Everything the row needs from outside the projection.
#[derive(Debug, Clone, Copy)]
pub struct StatusContext<'a> {
    /// The model the run was given.
    pub model: &'a str,
    /// The permission preset in force.
    pub permission: &'a str,
    /// The workspace directory.
    pub workspace: &'a str,
    /// Entries staged in the current ChangeSet.
    pub pending_changes: usize,
    /// Context window, when one is configured.
    pub context_window: Option<u64>,
    /// The transcript is scrolled away from the tail.
    pub paused: bool,
    /// Rows appended since the reader scrolled away.
    pub unread: usize,
}

/// Sub-cell ladder, so a bar narrower than the value still moves.
const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// Renders a proportional bar `width` cells wide.
///
/// The filled portion is rounded down to a whole cell plus a partial cell, so a
/// nearly-empty context never reads as one full cell of pressure.
pub fn render_context_bar(used: u64, total: u64, width: usize) -> String {
    if width == 0 || total == 0 {
        return String::new();
    }
    let ratio = (used as f64 / total as f64).clamp(0.0, 1.0);
    let eighths = (ratio * width as f64 * 8.0).round() as usize;
    let full = eighths / 8;
    let partial = if full < width { EIGHTHS[eighths % 8] } else { "" };
    let filled = format!("{}{partial}", "█".repeat(full.min(width)));
    let pad = width.saturating_sub(display_width(&filled));
    format!("{filled}{}", " ".repeat(pad))
}

fn compact(value: u64) -> String {
    if value < 1_000 {
        value.to_string()
    } else if value < 1_000_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    }
}

/// The last component of a path, for the right-hand side.
fn leaf(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|leaf| !leaf.is_empty())
        .unwrap_or(path)
}

/// Folds the projection into the row model.
///
/// Priorities are assigned here so the drop order is one decision in one place.
pub fn build_status_model(
    status: RunStatus,
    counters: Counters,
    usage: &agentrs_types::TokenUsage,
    dropped_live: u64,
    context: &StatusContext<'_>,
) -> StatusModel {
    let mut left = Vec::new();
    // The model and the preset are what the run is: neither is ever dropped.
    if !context.model.is_empty() {
        left.push(StatusSegment::new(context.model, 0));
    }
    left.push(StatusSegment::new(context.permission, 0));
    left.push(StatusSegment::new(status.label(), 0));
    let input = usage.input_tokens;
    if let Some(percent) = context
        .context_window
        .and_then(|window| (input * 100).checked_div(window))
    {
        left.push(StatusSegment::new(format!("ctx {}%", percent.min(100)), 1));
    }
    left.push(StatusSegment::new(format!("tools {}", counters.tools), 3));
    if context.pending_changes > 0 {
        left.push(StatusSegment::new(
            format!("staged {}", context.pending_changes),
            1,
        ));
    }
    if counters.approvals > 0 {
        left.push(StatusSegment::new(
            format!("approvals {}", counters.approvals),
            4,
        ));
    }
    if counters.compactions > 0 {
        left.push(StatusSegment::new(
            format!("compactions {}", counters.compactions),
            4,
        ));
    }
    if input > 0 || usage.output_tokens > 0 {
        left.push(StatusSegment::new(
            format!("tok {}→{}", compact(input), compact(usage.output_tokens)),
            5,
        ));
    }
    if counters.cache_breaks > 0 {
        left.push(StatusSegment::new(
            format!("cache breaks {}", counters.cache_breaks),
            6,
        ));
    }
    // Dropped live events mean the screen is behind the log. It is low value
    // while it is zero and it is never zero silently, so it only appears when
    // it has something to say.
    if dropped_live > 0 {
        left.push(StatusSegment::new(format!("dropped {dropped_live}"), 2));
    }

    let right = vec![StatusSegment::new(leaf(context.workspace), 2)];

    let hint = if context.paused {
        if context.unread == 0 {
            "paused".to_string()
        } else {
            format!("paused · {} unread", context.unread)
        }
    } else if status.terminal() {
        "ctrl+s commit · /discard · /quit".into()
    } else if status == RunStatus::AwaitingApproval {
        "y allow once · n reject".into()
    } else if status == RunStatus::Running {
        "esc to interrupt".into()
    } else {
        "ctrl+p commands · ? keys".into()
    };

    StatusModel {
        bar: context
            .context_window
            .filter(|window| *window > 0)
            .map(|window| ContextBar {
                used: input,
                total: window,
            }),
        left,
        right,
        hint,
    }
}

const SEPARATOR: &str = " · ";

/// Joins segments into a row that fits `columns`.
///
/// Whole segments are dropped from the lowest priority up rather than one being
/// cut mid-word. An oversized lone segment is cut rather than dropped, so the
/// row never goes blank on a terminal narrower than a single field.
pub fn render_segments(segments: &[StatusSegment], columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let mut kept: Vec<StatusSegment> = segments.to_vec();
    let join = |parts: &[StatusSegment]| {
        parts
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR)
    };
    while kept.len() > 1 && display_width(&join(&kept)) > columns {
        let worst = kept.iter().map(|part| part.priority).max().unwrap_or(0);
        let at = kept
            .iter()
            .rposition(|part| part.priority == worst)
            .unwrap_or(0);
        kept.remove(at);
    }
    truncate_to_width(&join(&kept), columns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentrs_types::TokenUsage;

    fn context<'a>() -> StatusContext<'a> {
        StatusContext {
            model: "Qwen3.6-35B",
            permission: "workspace-write",
            workspace: "/tmp/demo",
            pending_changes: 0,
            context_window: Some(100_000),
            paused: false,
            unread: 0,
        }
    }

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..TokenUsage::default()
        }
    }

    fn model() -> StatusModel {
        build_status_model(
            RunStatus::Running,
            Counters {
                tools: 4,
                approvals: 1,
                cache_breaks: 2,
                compactions: 0,
            },
            &usage(8_200, 1_100),
            0,
            &context(),
        )
    }

    #[test]
    fn a_bar_never_reads_as_pressure_it_does_not_have() {
        // One token in a hundred thousand is not one full cell.
        assert_eq!(render_context_bar(1, 100_000, 10).trim_end(), "");
        assert_eq!(render_context_bar(100_000, 100_000, 4), "████");
        assert_eq!(display_width(&render_context_bar(3, 8, 4)), 4, "always padded");
    }

    #[test]
    fn the_row_carries_what_the_run_is_and_what_it_has_done() {
        let row = render_segments(&model().left, 200);
        assert!(row.contains("Qwen3.6-35B"));
        assert!(row.contains("workspace-write"));
        assert!(row.contains("tools 4"));
        assert!(row.contains("ctx 8%"));
        assert!(row.contains("tok 8.2k→1.1k"));
    }

    #[test]
    fn a_narrow_row_drops_whole_fields_lowest_value_first() {
        let model = model();
        let wide = render_segments(&model.left, 200);
        let narrow = render_segments(&model.left, 60);
        assert!(display_width(&narrow) <= 60);
        assert!(wide.contains("cache breaks"));
        // The cache share is the first to go; the preset never goes at all.
        assert!(!narrow.contains("cache breaks"));
        assert!(narrow.contains("workspace-write"));
    }

    #[test]
    fn the_row_is_never_blank_even_below_one_field() {
        let row = render_segments(&model().left, 6);
        assert!(!row.is_empty());
        assert!(display_width(&row) <= 6);
    }

    #[test]
    fn a_field_with_nothing_to_say_is_absent_rather_than_zero() {
        let quiet = build_status_model(
            RunStatus::Idle,
            Counters::default(),
            &TokenUsage::default(),
            0,
            &context(),
        );
        let row = render_segments(&quiet.left, 200);
        assert!(!row.contains("approvals"));
        assert!(!row.contains("dropped"));
        assert!(!row.contains("tok "));
        assert!(row.contains("tools 0"), "what ran is always worth saying");
    }

    #[test]
    fn dropped_live_events_are_reported_because_the_screen_is_behind() {
        let behind = build_status_model(
            RunStatus::Running,
            Counters::default(),
            &TokenUsage::default(),
            17,
            &context(),
        );
        assert!(render_segments(&behind.left, 200).contains("dropped 17"));
    }

    #[test]
    fn the_hint_says_what_the_next_key_does() {
        let hint = |status, paused, unread| {
            build_status_model(
                status,
                Counters::default(),
                &TokenUsage::default(),
                0,
                &StatusContext {
                    paused,
                    unread,
                    ..context()
                },
            )
            .hint
        };
        assert_eq!(hint(RunStatus::Running, false, 0), "esc to interrupt");
        assert_eq!(hint(RunStatus::AwaitingApproval, false, 0), "y allow once · n reject");
        assert_eq!(
            hint(RunStatus::Completed, false, 0),
            "ctrl+s commit · /discard · /quit"
        );
        assert_eq!(hint(RunStatus::Running, true, 3), "paused · 3 unread");
    }

    #[test]
    fn the_right_hand_side_shows_the_workspace_leaf() {
        assert_eq!(render_segments(&model().right, 40), "demo");
    }
}

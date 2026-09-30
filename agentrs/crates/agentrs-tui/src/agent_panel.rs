//! The live agent panel: which sub-agents and team members are working right
//! now. Its transcript counterpart is the `• Agents` block in `ui.rs`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::state::AppState;
use crate::ui::{muted, tool_status_style, truncate_line};

/// Rows the panel shows before it elides the tail. Deliberately smaller than
/// the checklist: the panel is a "who is still working" glance, and it competes
/// with the checklist for an inline viewport.
const MAX_VISIBLE_AGENTS: usize = 4;

/// Rows the live agent panel takes: a header plus one row per shown agent and,
/// when agents are hidden, one more for the count of them. Zero when nothing is
/// running, so an ordinary turn pays nothing for the feature.
pub(super) fn agents_panel_height(state: &AppState, available: u16) -> u16 {
    let active = state.active_agents().count();
    if active == 0 {
        return 0;
    }
    let shown = active.min(MAX_VISIBLE_AGENTS);
    let elided = usize::from(active > MAX_VISIBLE_AGENTS);
    let wanted = (1 + shown + elided).min(u16::MAX as usize) as u16;
    wanted.min(available)
}

pub(super) fn render_agents_panel(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let active = state.active_agents().count();
    if active == 0 || area.height == 0 {
        return;
    }

    let mut lines = vec![Line::from(Span::styled(
        format!("  Agents {active}/{}", state.agents.len()),
        muted(state),
    ))];

    // Same rule as the checklist: fit the rows granted, not the rows asked
    // for, and keep the hidden-count line when they differ.
    let body = area.height.saturating_sub(1) as usize;
    let shown = if active <= body.min(MAX_VISIBLE_AGENTS) {
        active
    } else {
        body.saturating_sub(1).min(MAX_VISIBLE_AGENTS)
    };

    for row in state.active_agents().take(shown) {
        let style = tool_status_style(row.step_status(), state);
        lines.push(Line::from(vec![
            Span::styled("  ▸ ".to_string(), style),
            Span::styled(
                truncate_line(&format!("{}  {}", row.name, row.detail()), area.width),
                style,
            ),
        ]));
    }

    if active > shown {
        lines.push(Line::from(Span::styled(
            format!("  … {} more", active - shown),
            muted(state),
        )));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

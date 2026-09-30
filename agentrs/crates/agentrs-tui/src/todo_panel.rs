//! The checklist panel: the task list the agent published for this turn.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::state::AppState;
use crate::ui::{color, muted, normal};

/// Checklist rows the panel shows before it elides the tail. Sized so a long
/// plan cannot crowd out the transcript it is describing.
const MAX_VISIBLE_TODOS: usize = 6;

/// Rows the checklist panel takes: a header plus one row per shown entry and,
/// when entries are hidden, one more for the count of them. Zero without a
/// checklist.
///
/// Capped at `available`. The frame is an inline viewport a dozen-odd rows
/// tall, not the whole terminal, so a panel that asked for more than the
/// chrome left behind would be silently shaved by the layout solver — and the
/// row it loses is the last one, which is exactly the "N more" line.
pub(super) fn todo_panel_height(state: &AppState, available: u16) -> u16 {
    if state.todos.is_empty() {
        return 0;
    }
    let shown = state.todos.len().min(MAX_VISIBLE_TODOS);
    let elided = usize::from(state.todos.len() > MAX_VISIBLE_TODOS);
    // header + entries + optional "N more" line
    let wanted = (1 + shown + elided).min(u16::MAX as usize) as u16;
    wanted.min(available)
}

pub(super) fn render_todo_panel(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    if state.todos.is_empty() || area.height == 0 {
        return;
    }

    let done = state.todos.iter().filter(|todo| todo.status == "completed").count();
    let mut lines = vec![Line::from(Span::styled(
        format!("  Tasks {}/{}", done, state.todos.len()),
        muted(state),
    ))];

    // Fit the rows actually granted, not the rows requested. When the two
    // differ, the count of hidden entries is the line that must survive: it is
    // the only thing telling the reader the list goes on.
    let body = area.height.saturating_sub(1) as usize;
    let shown = if state.todos.len() <= body.min(MAX_VISIBLE_TODOS) {
        state.todos.len()
    } else {
        // One body row goes to the "N more" line, so the entries get the rest.
        body.saturating_sub(1).min(MAX_VISIBLE_TODOS)
    };

    for todo in state.todos.iter().take(shown) {
        let (marker, style) = match todo.status.as_str() {
            "completed" => ("✓", muted(state)),
            "in_progress" => ("▸", color(state, Color::Cyan)),
            _ => ("○", normal(state)),
        };
        // The active entry is the one the user is waiting on, so it shows its
        // present-continuous form; the rest stay in the imperative they were
        // planned in.
        let label = if todo.status == "in_progress" {
            todo.active_form.as_deref().unwrap_or(&todo.content)
        } else {
            &todo.content
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {marker} "), style),
            Span::styled(label.to_string(), style),
        ]));
    }

    if state.todos.len() > shown {
        lines.push(Line::from(Span::styled(
            format!("  … {} more", state.todos.len() - shown),
            muted(state),
        )));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

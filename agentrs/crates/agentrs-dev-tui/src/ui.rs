//! Ratatui renderer for the development harness.

use agentrs_contracts::policy::ApprovalRequest;
use agentrs_types::Role;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::sanitize::terminal_text;
use crate::state::{AppState, RunStatus};

/// Transient input and modal data layered over the event-derived state.
pub struct View<'a> {
    /// Deterministic event projection.
    pub state: &'a AppState,
    /// Current composer contents.
    pub composer: &'a str,
    /// Approval currently awaiting a key decision.
    pub approval: Option<&'a ApprovalRequest>,
    /// Workspace root shown in the header.
    pub workspace: &'a str,
    /// Configured model shown in the header.
    pub model: &'a str,
    /// Number of entries staged in the current ChangeSet.
    pub pending_changes: usize,
}

/// Draws one complete frame.
pub fn render(frame: &mut Frame<'_>, view: &View<'_>) {
    let area = frame.area();
    if area.width < 48 || area.height < 14 {
        frame.render_widget(
            Paragraph::new("AgentRS TUI requires at least 48x14")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Yellow)),
            area,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(frame, rows[0], view);
    render_transcript(frame, rows[1], view.state);
    render_tools(frame, rows[2], view.state);
    render_composer(frame, rows[3], view.composer, view.state.status);
    render_status(frame, rows[4], view);

    if let Some(request) = view.approval {
        render_approval(frame, area, request);
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, view: &View<'_>) {
    let run = view
        .state
        .run_id
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "not started".into());
    let header = Line::from(vec![
        Span::styled(
            " AGENTRS DEV TUI ",
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {}  model={}  run={}  workspace={}",
            view.state.status.label(),
            terminal_text(view.model),
            run,
            terminal_text(view.workspace)
        )),
    ]);
    frame.render_widget(Paragraph::new(header), area);
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let mut lines = Vec::new();
    for entry in &state.transcript {
        let (label, color) = match entry.role {
            Role::User => ("you", Color::Cyan),
            Role::Assistant => ("assistant", Color::Green),
            Role::System => ("system", Color::Yellow),
            Role::Tool => ("tool", Color::Magenta),
        };
        lines.push(Line::from(Span::styled(
            format!("{label}>"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));
        lines.extend(Text::from(entry.text.clone()).lines);
        lines.push(Line::default());
    }
    if !state.streaming.is_empty() {
        lines.push(Line::from(Span::styled(
            "assistant> streaming",
            Style::default().fg(Color::Green),
        )));
        lines.extend(Text::from(state.streaming.clone()).lines);
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Enter a prompt below to start a run.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" Transcript ").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_tools(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let lines = state
        .tools
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .map(|row| {
            Line::from(vec![
                Span::styled(format!("{:>10}", row.state), Style::default().fg(Color::Yellow)),
                Span::raw(format!("  {}  {}", row.id, row.detail)),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" Tool activity ").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, composer: &str, status: RunStatus) {
    let title = if status == RunStatus::Idle {
        " Prompt - Enter to run "
    } else {
        " Steering - Enter to queue "
    };
    frame.render_widget(
        Paragraph::new(terminal_text(composer))
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_status(frame: &mut Frame<'_>, area: Rect, view: &View<'_>) {
    let state = view.state;
    let notice = state.notice.as_deref().unwrap_or("");
    let line = format!(
        "Ctrl+C cancel/quit | in={} out={} cache-read={} | changes={} dropped-live={} | {}",
        state.usage.input_tokens,
        state.usage.output_tokens,
        state.usage.cache_read_tokens,
        view.pending_changes,
        state.dropped_live,
        terminal_text(notice)
    );
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_approval(frame: &mut Frame<'_>, area: Rect, request: &ApprovalRequest) {
    let popup = centered_rect(76, 60, area);
    frame.render_widget(Clear, popup);
    let body = vec![
        Line::from(Span::styled(
            "HUMAN APPROVAL REQUIRED",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(format!("tool: {}", terminal_text(&request.proposal.tool_name))),
        Line::from(format!(
            "workspace: {}",
            terminal_text(&request.proposal.workspace_id)
        )),
        Line::from(format!("change set: {}", request.proposal.change_set_id)),
        Line::from(format!("risk: {}", terminal_text(&request.risk_summary))),
        Line::from(format!(
            "arguments: {}",
            terminal_text(&request.proposal.arguments.to_string())
        )),
        Line::default(),
        Line::from(Span::styled(
            "Y allow once    N reject",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )),
    ];
    frame.render_widget(
        Paragraph::new(body)
            .block(
                Block::default()
                    .title(" Approval ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn renders_non_blank_desktop_and_small_fallback() {
        for (width, height) in [(100, 30), (40, 10)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let state = AppState::default();
            terminal
                .draw(|frame| {
                    render(
                        frame,
                        &View {
                            state: &state,
                            composer: "test",
                            approval: None,
                            workspace: ".",
                            model: "model",
                            pending_changes: 0,
                        },
                    )
                })
                .unwrap();
            let content = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(!content.trim().is_empty());
        }
    }
}

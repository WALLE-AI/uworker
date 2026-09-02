// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/views/*.tsx, app.tsx @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: React/Ink → ratatui; the same layout and drop order,
//            painted imperatively.
//! Ratatui renderer.
//!
//! There are no boxes. The frame is a column of rows: the transcript fills what
//! is left after the fixed furniture below it, and each piece of furniture takes
//! rows only when it has something to say — the working line while the run is
//! busy, the notice row while a notice holds it, the context bar when there is a
//! window to measure against.
//!
//! Everything shown here is computed by the pure modules; this file only places
//! and paints. That is what lets a snapshot test assert on layout without a
//! terminal, and what keeps the wrap decision in one place.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/views/` and `app.tsx`.

use agentrs_contracts::authority::PermissionMode;
use agentrs_contracts::policy::ApprovalRequest;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::approval::{mode_label, ApprovalOption};
use crate::completion::Completion;
use crate::composer::{prompt_tone, Composer};
use crate::glyphs::GlyphSet;
use crate::notices::NoticeQueue;
use crate::overlay::Overlay;
use crate::state::AppState;
use crate::status_line::{build_status_model, render_context_bar, render_segments, StatusContext};
use crate::styling::StyledSegment;
use crate::text::{display_width, truncate_to_width, wrap_to_width};
use crate::theme::{RowTone, Theme};
use crate::transcript::TranscriptRow;
use crate::working_line::WorkingLine;

/// The narrowest window worth drawing.
pub const MIN_COLUMNS: u16 = 24;
/// The shortest window worth drawing.
pub const MIN_ROWS: u16 = 6;
/// Below this the hint is dropped whole rather than cut to a stub.
const MIN_HINT: usize = 10;

/// One approval, with the row the user is on.
#[derive(Debug, Clone)]
pub struct ApprovalView<'a> {
    /// The request as the policy stated it.
    pub request: &'a ApprovalRequest,
    /// The rows on offer.
    pub options: &'a [ApprovalOption],
    /// Which row is selected.
    pub selected: usize,
}

/// Everything one frame is drawn from.
pub struct View<'a> {
    /// Deterministic event projection.
    pub state: &'a AppState,
    /// Transcript rows, already wrapped to this width.
    pub rows: &'a [TranscriptRow],
    /// First visible row of the transcript.
    pub scroll: usize,
    /// Current composer contents.
    pub composer: &'a Composer,
    /// Approval awaiting a decision.
    pub approval: Option<ApprovalView<'a>>,
    /// A full-screen surface, which takes the whole window when open.
    pub overlay: Option<&'a Overlay>,
    /// An open draft completion.
    pub completion: Option<&'a Completion>,
    /// The working line, while the run is busy.
    pub working: Option<&'a WorkingLine>,
    /// The notice queue.
    pub notices: &'a NoticeQueue,
    /// Workspace root.
    pub workspace: &'a str,
    /// Configured model.
    pub model: &'a str,
    /// Permission mode the next run will use.
    pub permission: &'a PermissionMode,
    /// Entries staged in the current ChangeSet.
    pub pending_changes: usize,
    /// Context window the run was given.
    pub context_window: Option<u64>,
    /// Glyphs matching the terminal.
    pub glyphs: &'a GlyphSet,
    /// Palette matching the terminal.
    pub theme: Theme,
}

impl View<'_> {
    /// True when the reader has scrolled away from the tail.
    pub fn paused(&self, viewport: usize) -> bool {
        self.scroll + viewport < self.rows.len()
    }
}

/// Turns one styled segment into a ratatui span.
fn span(theme: Theme, segment: &StyledSegment, row_tone: RowTone) -> Span<'static> {
    let tone = segment.tone.unwrap_or(row_tone);
    let mut style = theme.style(tone);
    if segment.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if segment.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if segment.strikethrough {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if segment.dim {
        style = style.add_modifier(Modifier::DIM);
    }
    Span::styled(segment.text.clone(), style)
}

fn styled_line(theme: Theme, segments: &[StyledSegment], tone: RowTone) -> Line<'static> {
    Line::from(
        segments
            .iter()
            .map(|segment| span(theme, segment, tone))
            .collect::<Vec<_>>(),
    )
}

/// Rows the composer needs, and the budget each of them is wrapped to.
///
/// The prompt glyph and its space take two columns from the first row; every
/// row is indented to match, so one budget covers them all.
fn composer_shape(view: &View<'_>, columns: usize) -> (u16, usize) {
    let budget = columns.saturating_sub(2).max(1);
    if view.composer.is_empty() {
        return (1, budget);
    }
    let rows = view.composer.rows(budget).len();
    (u16::try_from(rows.clamp(1, 8)).unwrap_or(1), budget)
}

fn composer_rows(view: &View<'_>, columns: usize) -> u16 {
    composer_shape(view, columns).0
}

/// Rows the completion list takes: bounded, because it sits over the transcript.
fn completion_rows(view: &View<'_>) -> u16 {
    view.completion
        .map(|completion| u16::try_from(completion.items.len().min(6)).unwrap_or(6))
        .unwrap_or(0)
}

fn approval_rows(approval: &ApprovalView<'_>) -> u16 {
    // Three rows of context — what is being asked, the risk, the arguments —
    // then one row per answer.
    u16::try_from(approval.options.len() + 3).unwrap_or(7)
}

/// How many rows the furniture below the transcript needs.
fn furniture_rows(view: &View<'_>, columns: usize) -> u16 {
    let mut rows = 1; // the status row, which is always there
    rows += composer_rows(view, columns);
    rows += completion_rows(view);
    if view.working.is_some() {
        rows += 1;
    }
    if view.notices.current().is_some() {
        rows += 1;
    }
    if view.context_window.is_some() {
        rows += 1;
    }
    if let Some(approval) = &view.approval {
        rows += approval_rows(approval);
    }
    rows
}

/// Draws one complete frame.
pub fn render(frame: &mut Frame<'_>, view: &View<'_>) {
    let area = frame.area();
    if area.width < MIN_COLUMNS || area.height < MIN_ROWS {
        frame.render_widget(
            Paragraph::new(format!("{MIN_COLUMNS}x{MIN_ROWS} minimum"))
                .alignment(Alignment::Center)
                .style(view.theme.style(RowTone::Warning)),
            area,
        );
        return;
    }
    let columns = area.width as usize;
    if view.overlay.is_some() {
        render_overlay(frame, area, view);
        return;
    }
    let furniture = furniture_rows(view, columns).min(area.height.saturating_sub(1));
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(furniture)])
        .split(area);

    let viewport = split[0].height as usize;
    render_transcript(frame, split[0], view, viewport);

    let mut cursor = split[1].y;
    let bottom = split[1].y + split[1].height;
    let mut take = |height: u16| -> Option<Rect> {
        if cursor + height > bottom {
            return None;
        }
        let rect = Rect::new(split[1].x, cursor, split[1].width, height);
        cursor += height;
        Some(rect)
    };

    if let Some(approval) = &view.approval {
        if let Some(rect) = take(approval_rows(approval)) {
            render_approval(frame, rect, view, approval);
        }
    }
    if let Some(working) = view.working {
        if let Some(rect) = take(1) {
            let tone = if working.stalled {
                RowTone::Warning
            } else {
                RowTone::System
            };
            frame.render_widget(
                Paragraph::new(working.text.clone()).style(view.theme.style(tone)),
                rect,
            );
        }
    }
    if let Some((text, tone)) = view.notices.row() {
        if let Some(rect) = take(1) {
            frame.render_widget(
                Paragraph::new(truncate_to_width(&text, columns)).style(view.theme.style(tone)),
                rect,
            );
        }
    }
    if let Some(window) = view.context_window {
        if let Some(rect) = take(1) {
            render_context(frame, rect, view, window);
        }
    }
    if let Some(rect) = take(completion_rows(view)) {
        render_completion(frame, rect, view);
    }
    if let Some(rect) = take(composer_rows(view, columns)) {
        render_composer(frame, rect, view);
    }
    if let Some(rect) = take(1) {
        render_status(frame, rect, view, viewport);
    }
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, view: &View<'_>, viewport: usize) {
    if view.rows.is_empty() {
        frame.render_widget(
            Paragraph::new("Type a message and press Enter to start a run.  ? for keys.")
                .style(view.theme.style(RowTone::System)),
            area,
        );
        return;
    }
    let start = view.scroll.min(view.rows.len().saturating_sub(1));
    let lines: Vec<Line<'static>> = view
        .rows
        .iter()
        .skip(start)
        .take(viewport)
        .map(|row| styled_line(view.theme, &row.segments, row.tone))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_context(frame: &mut Frame<'_>, area: Rect, view: &View<'_>, window: u64) {
    let used = view.state.usage.input_tokens;
    let width = (area.width as usize).saturating_sub(12).clamp(1, 40);
    let bar = render_context_bar(used, window, width);
    let percent = (used * 100).checked_div(window).unwrap_or(0).min(100);
    frame.render_widget(
        Paragraph::new(format!("ctx {bar} {percent:>3}%")).style(view.theme.style(RowTone::System)),
        area,
    );
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, view: &View<'_>) {
    let tone = prompt_tone(mode_label(view.permission));
    let marker = format!("{} ", view.glyphs.user);
    let indent = " ".repeat(display_width(&marker));
    let budget = (area.width as usize)
        .saturating_sub(display_width(&marker))
        .max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if view.composer.is_empty() {
        let placeholder = if view.state.status.terminal() {
            "message… · ctrl+s commit · /discard · /quit"
        } else {
            "message…"
        };
        lines.push(Line::from(vec![
            Span::styled(marker.clone(), view.theme.style(tone)),
            // The caret sits on the first cell of the placeholder, so an empty
            // composer still shows where typing will land.
            Span::styled(" ", view.theme.style(tone).add_modifier(Modifier::REVERSED)),
            Span::styled(
                placeholder.to_string(),
                view.theme.style(RowTone::System).add_modifier(Modifier::DIM),
            ),
        ]));
    } else {
        let (caret_row, caret_column) = view.composer.caret_cell(budget);
        for (index, row) in view.composer.rows(budget).into_iter().enumerate() {
            let prefix = if index == 0 { marker.clone() } else { indent.clone() };
            let mut spans = vec![Span::styled(prefix, view.theme.style(tone))];
            if index == caret_row {
                spans.extend(caret_spans(&row, caret_column));
            } else {
                spans.push(Span::styled(row, Style::default()));
            }
            lines.push(Line::from(spans));
        }
        // A caret that has just filled the last row lands on a row that does not
        // exist yet. Drawing it is the difference between "there is nowhere left
        // to type" and "the next character goes here".
        if caret_row >= lines.len() {
            lines.push(Line::from(vec![
                Span::styled(indent, view.theme.style(tone)),
                Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Splits one row so the cell under the caret is drawn reversed.
///
/// Without it every motion — the arrows, `ctrl+a`, `ctrl+e`, `delete` — is
/// guesswork: the row looks identical whatever the caret is doing, and the first
/// sign of where it was is a character disappearing from somewhere unexpected.
fn caret_spans(row: &str, column: usize) -> Vec<Span<'static>> {
    let mut before = String::new();
    let mut under = String::new();
    let mut after = String::new();
    let mut width = 0;
    for ch in row.chars() {
        let cell = crate::text::char_width(ch);
        if width < column {
            before.push(ch);
        } else if under.is_empty() && width == column {
            under.push(ch);
        } else {
            after.push(ch);
        }
        width += cell;
    }
    if under.is_empty() {
        // Past the end of the row: the caret is the next cell to be filled.
        under.push(' ');
    }
    vec![
        Span::styled(before, Style::default()),
        Span::styled(under, Style::default().add_modifier(Modifier::REVERSED)),
        Span::styled(after, Style::default()),
    ]
}

/// 审批面板上参数那几行。
///
/// 从前是一行被截断的 JSON。对文件工具尚可（路径在最前面），对 `Bash` 就不行了：
/// 人要判断的恰恰是那条命令，而它长得几乎必然被截掉，于是面板上只剩
/// `{"command":"find . -name '*.rs' -type f -exec wc…`——**要点正好在省略号后面**。
///
/// 所以：模型给的 `description` 单独占一行（它就是写给人看的），其余参数换行
/// 铺开而不是截断。上限 6 行，够长的参数仍会被收住，但收住的是尾巴不是要点。
fn argument_lines(arguments: &serde_json::Value, columns: usize) -> Vec<String> {
    const 上限: usize = 6;
    let mut out = Vec::new();
    let mut rest = arguments.clone();

    // `description` 是给人的一句话，不该混在 JSON 里跟转义字符一起读。
    if let Some(obj) = rest.as_object_mut() {
        if let Some(text) = obj
            .remove("description")
            .and_then(|v| v.as_str().map(str::to_string))
        {
            if !text.trim().is_empty() {
                out.extend(wrap_to_width(&text, columns).into_iter().take(2));
            }
        }
    }
    let body = match rest.as_object() {
        // 只剩一个字符串参数时（`command`、`url`、`query`）直接给原文——
        // JSON 的引号与反斜杠在这里只是噪声，人读的是命令本身。
        Some(obj) if obj.len() == 1 => match obj.values().next().and_then(|v| v.as_str()) {
            Some(text) => text.to_string(),
            None => rest.to_string(),
        },
        _ => rest.to_string(),
    };
    out.extend(wrap_to_width(&body, columns));
    if out.len() > 上限 {
        out.truncate(上限);
        out.push(format!("… 参数共 {} 行，其余未显示", out.len()));
    }
    out
}

fn render_approval(frame: &mut Frame<'_>, area: Rect, view: &View<'_>, approval: &ApprovalView<'_>) {
    let request = approval.request;
    let columns = area.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(
            truncate_to_width(
                &format!(
                    "{} approval required: {}",
                    view.glyphs.interrupted, request.proposal.tool_name
                ),
                columns,
            ),
            view.theme
                .style(RowTone::Warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            truncate_to_width(
                &format!(
                    "  {}  ·  change set {}",
                    request.risk_summary, request.proposal.change_set_id
                ),
                columns,
            ),
            view.theme.style(RowTone::System),
        )),
    ];
    for line in argument_lines(&request.proposal.arguments, columns.saturating_sub(2)) {
        lines.push(Line::from(Span::styled(
            format!("  {line}"),
            view.theme.style(RowTone::System).add_modifier(Modifier::DIM),
        )));
    }
    for (index, option) in approval.options.iter().enumerate() {
        let selected = index == approval.selected;
        let marker = if selected { ">" } else { " " };
        let tone = if option.allowed {
            RowTone::Warning
        } else {
            RowTone::Assistant
        };
        let mut style = view.theme.style(tone);
        if selected {
            style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(
            truncate_to_width(&format!("{marker} {}. {}", index + 1, option.label), columns),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Draws an open completion as a short list above the composer.
fn render_completion(frame: &mut Frame<'_>, area: Rect, view: &View<'_>) {
    let Some(completion) = view.completion else {
        return;
    };
    let columns = area.width as usize;
    let height = area.height as usize;
    // The window follows the selection so a long list still shows what is taken.
    let start = completion.selected.saturating_sub(height.saturating_sub(1));
    let lines: Vec<Line<'static>> = completion
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(index, item)| {
            let selected = index == completion.selected;
            let mut style = view.theme.style(RowTone::Tool);
            if selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Line::from(Span::styled(
                truncate_to_width(&format!("  {item}"), columns),
                style,
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Draws a full-screen surface: a title, its rows, a text box, and a hint.
fn render_overlay(frame: &mut Frame<'_>, area: Rect, view: &View<'_>) {
    let Some(overlay) = view.overlay.as_ref() else {
        return;
    };
    let columns = area.width as usize;
    let list_height = (area.height as usize).saturating_sub(3).max(1);
    // `window` moves the surface's own scroll, so it works on a copy: a draw
    // must not be the thing that changes what is drawn next time.
    let mut scratch = (*overlay).clone();
    let (rows, cursor) = scratch.window(list_height);

    let mut lines = vec![Line::from(Span::styled(
        truncate_to_width(
            &format!(
                "{} {} ({} rows)",
                view.glyphs.rule.repeat(2),
                overlay.surface.title(),
                overlay.len()
            ),
            columns,
        ),
        view.theme
            .style(RowTone::Heading)
            .add_modifier(Modifier::BOLD),
    ))];
    for (index, row) in rows.iter().enumerate() {
        let mut style = view.theme.style(row.tone.unwrap_or(RowTone::Assistant));
        if index == cursor && row.selectable() {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let marker = if index == cursor && row.selectable() { ">" } else { " " };
        lines.push(Line::from(Span::styled(
            truncate_to_width(&format!("{marker} {}", row.text), columns),
            style,
        )));
    }
    // The list is padded so the text box and the hint stay on the last two rows
    // however short the list is.
    while lines.len() + 2 < area.height as usize {
        lines.push(Line::default());
    }
    let box_label = if overlay.surface.searches() { "search" } else { "filter" };
    lines.push(Line::from(Span::styled(
        truncate_to_width(&format!("{box_label}: {}", overlay.query()), columns),
        view.theme.style(RowTone::User),
    )));
    lines.push(Line::from(Span::styled(
        truncate_to_width(overlay.surface.hint(), columns),
        view.theme.style(RowTone::System).add_modifier(Modifier::DIM),
    )));
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_status(frame: &mut Frame<'_>, area: Rect, view: &View<'_>, viewport: usize) {
    let columns = area.width as usize;
    let paused = view.paused(viewport);
    let model = build_status_model(
        view.state.status,
        view.state.counters,
        &view.state.usage,
        view.state.dropped_live,
        &StatusContext {
            model: view.model,
            permission: mode_label(view.permission),
            workspace: view.workspace,
            pending_changes: view.pending_changes,
            context_window: view.context_window,
            paused,
            unread: view.rows.len().saturating_sub(view.scroll + viewport),
        },
    );
    // The three fields are laid out by hand rather than by a layout: the middle
    // one is a hint that may be dropped whole, and a layout would pad it instead.
    //
    // The order the budget is handed out in *is* the priority order between the
    // three. The right-hand side is capped at a quarter so it can never squeeze
    // the left; the left takes everything else and drops its own fields by
    // priority; the hint takes whatever is left, and goes entirely rather than
    // being cut to a stub that says nothing.
    let right = render_segments(&model.right, columns / 4);
    let left_budget = columns
        .saturating_sub(display_width(&right))
        .saturating_sub(2);
    let left = render_segments(&model.left, left_budget);
    let leftover = columns
        .saturating_sub(display_width(&left))
        .saturating_sub(display_width(&right));
    let hint = if leftover >= MIN_HINT + 2 {
        truncate_to_width(&model.hint, leftover - 2)
    } else {
        String::new()
    };
    let gap = columns
        .saturating_sub(display_width(&left))
        .saturating_sub(display_width(&hint))
        .saturating_sub(display_width(&right));
    let style = view.theme.style(RowTone::System);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(left, style),
            Span::styled(" ".repeat(gap / 2), style),
            Span::styled(hint, style.add_modifier(Modifier::DIM)),
            Span::styled(" ".repeat(gap - gap / 2), style),
            Span::styled(right, style),
        ]))
        .style(style),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyphs::{ASCII_GLYPHS, UNICODE_GLYPHS};
    use crate::state::{Node, TextKind, TextNode};
    use crate::theme::ColorLevel;
    use crate::transcript::{build_entries, transcript_rows, TranscriptOptions};
    use agentrs_types::Role;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn 审批面板把命令原文完整摆出来() {
        // 这条抓的是一个真的：面板从前只印一行截断的 JSON，于是
        // `{"command":"find . -name '*.rs' -type f -exec wc…` 把要点正好留在
        // 省略号后面，而人要判断的恰恰是那条命令。
        let args = serde_json::json!({
            "command": "find . -name '*.rs' -type f -exec wc -l {} + | tail -n 1",
            "description": "统计工作区里 Rust 代码的总行数"
        });
        let lines = argument_lines(&args, 40);
        let 全文 = lines.join("\n");
        // 人话那一句在最前面，因为它就是写给人看的。
        assert!(lines[0].contains("统计工作区"), "{lines:?}");
        // 命令**每一段**都在，不是截断到第一行。
        for 片段 in ["find .", "-name", "*.rs", "wc -l", "tail -n 1"] {
            assert!(全文.contains(片段), "命令里的 {片段} 没了：{全文}");
        }
        // 只剩一个字符串参数时给原文，不给 JSON——引号与反斜杠在这里只是噪声。
        assert!(!全文.contains("{\"command\""), "{全文}");
    }

    #[test]
    fn 审批面板不会被超长参数撑满整屏() {
        let args = serde_json::json!({"content": "x".repeat(4000)});
        let lines = argument_lines(&args, 40);
        assert!(lines.len() <= 7, "{} 行", lines.len());
        assert!(lines.last().unwrap().contains("未显示"), "{lines:?}");
    }

    #[test]
    fn 没有_description_时照样把参数摆出来() {
        let lines = argument_lines(&serde_json::json!({"path": "src/main.rs"}), 40);
        assert_eq!(lines, vec!["src/main.rs".to_string()]);
        // 多参数保持 JSON：字段名此时是必要的，`old`/`new` 分不清就危险了。
        let lines = argument_lines(&serde_json::json!({"path": "a", "old": "b", "new": "c"}), 60);
        assert!(lines[0].contains("\"old\""), "{lines:?}");
    }

    fn state() -> AppState {
        let mut state = AppState::default();
        state.turn = 1;
        state.nodes = vec![
            Node::Text(TextNode {
                role: Role::User,
                kind: TextKind::Prose,
                text: "read the three files".into(),
                turn: 1,
            }),
            Node::Text(TextNode {
                role: Role::Assistant,
                kind: TextKind::Prose,
                text: "Reading them now.".into(),
                turn: 1,
            }),
        ];
        state
    }

    fn snapshot(width: u16, height: u16, glyphs: &GlyphSet, theme: Theme) -> Vec<String> {
        let state = state();
        let diffs = HashMap::new();
        let entries = build_entries(
            &state,
            &TranscriptOptions {
                glyphs,
                now_ms: 0,
                diffs: &diffs,
            },
        );
        let rows = transcript_rows(&entries, width as usize, &HashSet::new(), glyphs);
        let composer = Composer::default();
        let notices = NoticeQueue::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &View {
                        state: &state,
                        rows: &rows,
                        scroll: 0,
                        composer: &composer,
                        approval: None,
                        overlay: None,
                        completion: None,
                        working: None,
                        notices: &notices,
                        workspace: "/tmp/agentrs-tui-demo",
                        model: "Qwen3.6-35B",
                        permission: &PermissionMode::Default,
                        pending_changes: 0,
                        context_window: Some(100_000),
                        glyphs,
                        theme,
                    },
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_frame_has_no_borders_at_all() {
        let rows = snapshot(
            80,
            12,
            &UNICODE_GLYPHS,
            Theme::resolve(ColorLevel::Truecolor, true),
        );
        let joined = rows.join("\n");
        for boxy in ['┌', '┐', '└', '┘', '│', '├'] {
            assert!(!joined.contains(boxy), "{boxy} in\n{joined}");
        }
    }

    #[test]
    fn the_transcript_and_the_furniture_both_get_their_rows() {
        let rows = snapshot(
            80,
            12,
            &UNICODE_GLYPHS,
            Theme::resolve(ColorLevel::Truecolor, true),
        );
        assert!(rows[0].starts_with("> read the three files"), "{:?}", rows[0]);
        assert!(rows[1].starts_with("● Reading them now."), "{:?}", rows[1]);
        // The status row is last and always present; the composer sits above it,
        // and the context bar above that.
        assert!(rows[11].contains("Qwen3.6-35B"), "{:?}", rows[11]);
        assert!(rows[10].starts_with("> "), "{:?}", rows[10]);
        assert!(rows[9].starts_with("ctx "), "{:?}", rows[9]);
    }

    #[test]
    fn a_narrow_window_drops_status_fields_rather_than_wrapping() {
        let rows = snapshot(40, 10, &UNICODE_GLYPHS, Theme::resolve(ColorLevel::Basic, true));
        let status = rows.last().unwrap();
        assert!(display_width(status) <= 40, "{status:?}");
        // The model and the preset are priority zero and never go.
        assert!(
            status.contains("Qwen3.6-35B") || status.contains("default"),
            "{status:?}"
        );
    }

    #[test]
    fn an_ascii_terminal_gets_no_wide_glyphs_in_the_transcript() {
        let rows = snapshot(80, 12, &ASCII_GLYPHS, Theme::resolve(ColorLevel::None, true));
        assert!(rows[0].is_ascii(), "{:?}", rows[0]);
        assert!(rows[1].is_ascii(), "{:?}", rows[1]);
    }

    fn frame_with(
        width: u16,
        height: u16,
        overlay: Option<&Overlay>,
        completion: Option<&Completion>,
        approval: Option<ApprovalView<'_>>,
    ) -> Vec<String> {
        let state = state();
        let composer = Composer::default();
        let notices = NoticeQueue::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &View {
                        state: &state,
                        rows: &[],
                        scroll: 0,
                        composer: &composer,
                        approval,
                        overlay,
                        completion,
                        working: None,
                        notices: &notices,
                        workspace: "/tmp/demo",
                        model: "m",
                        permission: &PermissionMode::Default,
                        pending_changes: 0,
                        context_window: None,
                        glyphs: &UNICODE_GLYPHS,
                        theme: Theme::resolve(ColorLevel::None, true),
                    },
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn an_open_surface_takes_the_whole_window() {
        use crate::overlay::{OverlayRow, Surface};
        let overlay = Overlay::open(
            Surface::Palette,
            vec![
                OverlayRow::new("/help    show the sheet", "help"),
                OverlayRow::new("/quit    quit", "quit"),
            ],
        );
        let rows = frame_with(40, 8, Some(&overlay), None, None);
        assert!(rows[0].contains("commands"), "{:?}", rows[0]);
        assert!(rows[1].starts_with("> /help"), "{:?}", rows[1]);
        // The transcript is gone: a surface is a surface, not a panel.
        assert!(!rows.iter().any(|row| row.contains("read the three files")));
        // The text box and the hint hold the last two rows whatever the height.
        assert!(rows[6].starts_with("filter:"), "{:?}", rows[6]);
        assert!(rows[7].contains("type to filter"), "{:?}", rows[7]);
    }

    #[test]
    fn a_searching_surface_says_search_rather_than_filter() {
        use crate::overlay::{OverlayRow, Surface};
        let overlay = Overlay::open(Surface::Transcript, vec![OverlayRow::new("a row", "a row")]);
        let rows = frame_with(40, 8, Some(&overlay), None, None);
        assert!(rows[6].starts_with("search:"), "{:?}", rows[6]);
    }

    #[test]
    fn drawing_a_surface_does_not_move_it() {
        use crate::overlay::{OverlayRow, Surface};
        let overlay = Overlay::open(
            Surface::Palette,
            (0..40)
                .map(|index| OverlayRow::new(format!("row {index}"), format!("v{index}")))
                .collect(),
        );
        let before = overlay.clone();
        frame_with(40, 8, Some(&overlay), None, None);
        assert_eq!(overlay, before, "a draw must not change what is drawn next");
    }

    #[test]
    fn an_open_completion_sits_above_the_composer() {
        let completion = Completion {
            kind: crate::completion::CompletionKind::Path,
            start: 0,
            query: String::new(),
            items: vec!["src/".into(), "README.md".into()],
            selected: 1,
        };
        let rows = frame_with(40, 10, None, Some(&completion), None);
        // Two candidate rows, then the composer, then the status row.
        assert!(rows[6].contains("src/"), "{:?}", rows[6]);
        assert!(rows[7].contains("README.md"), "{:?}", rows[7]);
        assert!(rows[8].starts_with("> "), "{:?}", rows[8]);
    }

    #[test]
    fn the_composer_shows_where_the_caret_is() {
        // Without a drawn caret every motion is guesswork: the row looks the
        // same whatever ctrl+a just did.
        let mut composer = Composer::default();
        composer.insert("hello");
        composer.move_caret(crate::composer::Motion::LineStart);
        let state = state();
        let notices = NoticeQueue::default();
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &View {
                        state: &state,
                        rows: &[],
                        scroll: 0,
                        composer: &composer,
                        approval: None,
                        overlay: None,
                        completion: None,
                        working: None,
                        notices: &notices,
                        workspace: "/tmp/demo",
                        model: "m",
                        permission: &PermissionMode::Default,
                        pending_changes: 0,
                        context_window: None,
                        glyphs: &UNICODE_GLYPHS,
                        theme: Theme::resolve(ColorLevel::None, true),
                    },
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        // The composer is the second-to-last row; `h` sits at column 2 and is
        // the cell the caret is on.
        let y = 8;
        let reversed: Vec<u16> = (0..40)
            .filter(|x| {
                buffer[(*x, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            })
            .collect();
        assert_eq!(reversed, vec![2], "exactly one cell carries the caret");
        assert_eq!(buffer[(2, y)].symbol(), "h");
    }

    #[test]
    fn a_window_too_small_to_draw_says_so_rather_than_drawing_nothing() {
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        let state = AppState::default();
        let composer = Composer::default();
        let notices = NoticeQueue::default();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &View {
                        state: &state,
                        rows: &[],
                        scroll: 0,
                        composer: &composer,
                        approval: None,
                        overlay: None,
                        completion: None,
                        working: None,
                        notices: &notices,
                        workspace: ".",
                        model: "m",
                        permission: &PermissionMode::Default,
                        pending_changes: 0,
                        context_window: None,
                        glyphs: &UNICODE_GLYPHS,
                        theme: Theme::resolve(ColorLevel::None, true),
                    },
                )
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(content.contains("minimum"), "{content}");
    }
}

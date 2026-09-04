use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::markdown::{MarkdownTheme, render_markdown};
use crate::state::{AppState, ApprovalChoice};
use crate::transcript::{EntryKind, ToolStepStatus, TranscriptEntry};

const MAX_COMPOSER_HEIGHT: u16 = 8;
const MAX_POPUP_ITEMS: usize = 7;
const FOOTER_HEIGHT: u16 = 2;
const MAX_VISIBLE_TOOLS: usize = 3;
/// Checklist rows the panel shows before it elides the tail. Sized so a long
/// plan cannot crowd out the transcript it is describing.
const MAX_VISIBLE_TODOS: usize = 6;
const TRANSCRIPT_BOTTOM_GAP: u16 = 1;
const AGENTRS_MARK_WIDTH: usize = 24;
/// Upper bound for the approval dialog height. Tool inputs are often
/// multi-line (file contents, patches), so the box is allowed to grow well
/// past the surrounding chrome before it starts eliding lines.
const APPROVAL_MAX_HEIGHT: u16 = 24;
/// Blank rows the approval dialog inserts around its description when the
/// tool input is short enough to spare them.
const SPACER_ROWS: usize = 2;
/// Indent applied to a transcript entry's body so it reads as one block under
/// its header. Matches the tool preview indent.
const BODY_INDENT: &str = "  ";
const BODY_INDENT_WIDTH: u16 = 2;
/// Rows a still-streaming reasoning block may occupy before it starts
/// scrolling its own tail.
const THINKING_LIVE_ROWS: usize = 6;
// A mirror-symmetric line interpretation of the Agentrs mark using colons and spaces only.
const AGENTRS_MARK_LINES: &[&str] = &[
    "          ::::          ",
    "        :::  :::        ",
    "      :::      :::      ",
    "    :::          :::    ",
    "                        ",
    "          ::::          ",
    "                        ",
    " :::                ::: ",
    "   :::            :::   ",
    "      ::::::::::::      ",
];

pub(super) fn render(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    let content = Rect::new(
        area.x.saturating_add(1),
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );

    if content.width < 20 || content.height < 6 {
        frame.render_widget(
            Paragraph::new("Terminal too small\nResize to at least 22×8").style(error(state)),
            content,
        );
        return;
    }

    if state.session_picker.is_visible() {
        render_session_picker(frame, area, state);
        return;
    }

    let composer_width = content.width.saturating_sub(4).max(1);
    let composer_height = state
        .composer
        .visual_height(composer_width, MAX_COMPOSER_HEIGHT)
        .saturating_add(1);
    let todo_height = todo_panel_height(state);
    let reserved_height = composer_height
        .saturating_add(1)
        .saturating_add(FOOTER_HEIGHT)
        .saturating_add(todo_height);
    let max_transcript_height = content.height.saturating_sub(reserved_height);
    let transcript_height = desired_transcript_height(state, content.width, max_transcript_height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(transcript_height),
            Constraint::Length(todo_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
            Constraint::Length(FOOTER_HEIGHT),
            Constraint::Min(0),
        ])
        .split(content);

    render_transcript(frame, chunks[0], state);
    render_todo_panel(frame, chunks[1], state);
    render_composer(frame, chunks[2], state);
    render_footer(frame, chunks[4], state);

    if state.approval.is_some() {
        render_approval(frame, content, state);
    } else if state.popup.is_visible(&state.composer.text()) {
        render_command_popup(frame, content, chunks[2], state);
    }
}

/// Rows the checklist panel needs: a header plus one row per shown entry, or
/// nothing at all when there is no checklist.
fn todo_panel_height(state: &AppState) -> u16 {
    if state.todos.is_empty() {
        return 0;
    }
    let shown = state.todos.len().min(MAX_VISIBLE_TODOS);
    let elided = usize::from(state.todos.len() > MAX_VISIBLE_TODOS);
    // header + entries + optional "N more" line
    (1 + shown + elided).min(u16::MAX as usize) as u16
}

fn render_todo_panel(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    if state.todos.is_empty() || area.height == 0 {
        return;
    }

    let done = state.todos.iter().filter(|todo| todo.status == "completed").count();
    let mut lines = vec![Line::from(Span::styled(
        format!("  Tasks {}/{}", done, state.todos.len()),
        muted(state),
    ))];

    for todo in state.todos.iter().take(MAX_VISIBLE_TODOS) {
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

    if state.todos.len() > MAX_VISIBLE_TODOS {
        lines.push(Line::from(Span::styled(
            format!("  … {} more", state.todos.len() - MAX_VISIBLE_TODOS),
            muted(state),
        )));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn compact_cwd(cwd: &str) -> &str {
    cwd.rsplit(['/', '\\']).find(|part| !part.is_empty()).unwrap_or(cwd)
}

fn desired_transcript_height(state: &AppState, width: u16, max_height: u16) -> u16 {
    if state.pending_transcript().is_empty() && state.show_welcome {
        return max_height;
    }
    let line_count = pending_transcript_lines(state, width).len();
    let transcript_height = if line_count == 0 {
        0
    } else {
        (line_count.min(u16::MAX as usize) as u16).saturating_add(TRANSCRIPT_BOTTOM_GAP)
    };
    let popup_height = if state.popup.is_visible(&state.composer.text()) {
        state.popup.matches().len().min(MAX_POPUP_ITEMS) as u16
    } else {
        0
    };
    let desired = transcript_height.max(popup_height);
    desired.min(max_height)
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let entries = state.pending_transcript();
    if entries.is_empty() {
        if state.show_welcome {
            render_welcome(frame, area, state);
        }
        return;
    }

    let lines = pending_transcript_lines(state, area.width);
    let line_count = lines.len();
    if line_count == 0 {
        return;
    }
    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    let bottom = line_count.saturating_sub(area.height as usize);
    let render_area = bottom_aligned_area(area, line_count, TRANSCRIPT_BOTTOM_GAP);
    frame.render_widget(paragraph.scroll((bottom.min(u16::MAX as usize) as u16, 0)), render_area);
}

pub(super) fn pending_history_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    transcript_lines(state.pending_transcript(), width.max(1), state)
}

pub(super) fn history_prefix_lines(state: &AppState, count: usize, width: u16) -> Vec<Line<'static>> {
    let count = count.min(state.transcript.len());
    transcript_lines(&state.transcript[..count], width.max(1), state)
}

pub(super) struct StreamingHistoryCommit {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) complete_entries: usize,
    pub(super) active_byte_count: usize,
}

pub(super) fn streaming_history_commit(state: &AppState, width: u16) -> Option<StreamingHistoryCommit> {
    if !state.can_commit_streaming_lines() {
        return None;
    }

    let pending = state.pending_transcript();
    let (active, complete) = pending.split_last()?;
    let stable_count = complete
        .iter()
        .take_while(|entry| entry.is_stable_for_history())
        .count();
    if stable_count != complete.len() {
        return None;
    }
    let mut lines = transcript_lines(complete, width.max(1), state);
    // Mirror the in-viewport spacing: a collapsed summary is the header of the
    // answer that follows it, so the flush boundary must not wedge a blank row
    // between them.
    let ends_with_summary = complete.last().is_some_and(|entry| collapses_thinking(entry, state));
    if !complete.is_empty() && !ends_with_summary {
        lines.push(Line::default());
    }

    let visible_text = active.visible_text();
    let active_byte_count = match active.kind {
        EntryKind::Assistant => markdown_commit_boundary(visible_text),
        // A collapsing block must not leak its body into scrollback: once
        // written there it can never be replaced by the summary.
        EntryKind::Thinking if state.thinking_display.collapses_when_finished() => 0,
        EntryKind::Thinking => line_commit_boundary(visible_text),
        _ => 0,
    };
    if active_byte_count > 0 {
        // The boundary sits after a newline; keeping it would render a blank
        // row between every flushed chunk.
        let prefix = visible_text[..active_byte_count]
            .strip_suffix('\n')
            .unwrap_or(&visible_text[..active_byte_count]);
        let mut prefix_entry = TranscriptEntry::new(active.kind, active.label.as_str(), prefix);
        if !active.shows_label() {
            prefix_entry = prefix_entry.into_continuation();
        }
        lines.extend(transcript_lines(&[prefix_entry], width.max(1), state));
        if active.kind == EntryKind::Assistant {
            lines.push(Line::default());
        }
    }

    let lines = compact_blank_runs(lines);

    (!lines.is_empty()).then_some(StreamingHistoryCommit {
        lines,
        complete_entries: complete.len(),
        active_byte_count,
    })
}

pub(super) fn welcome_history_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    welcome_transcript_lines(state, width.max(1))
}

fn transcript_lines(entries: &[TranscriptEntry], width: u16, state: &AppState) -> Vec<Line<'static>> {
    if entries.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        // A collapsed reasoning summary is a single muted line; separating it
        // from the answer it precedes only spends vertical space.
        let after_summary = index > 0 && collapses_thinking(&entries[index - 1], state);
        if index > 0 && !after_summary {
            lines.push(Line::default());
        }
        let entry = &entries[index];
        let tool_group_end = consecutive_tool_end(entries, index);
        if tool_group_end.saturating_sub(index) > 1 {
            lines.extend(tool_group_lines(&entries[index..tool_group_end], width, state));
            index = tool_group_end;
            continue;
        }
        match entry.kind {
            EntryKind::User => {
                lines.extend(user_message_lines(entry.visible_text(), width, state));
            }
            EntryKind::Assistant => {
                lines.extend(render_markdown(entry.visible_text(), width, markdown_theme(state)));
            }
            EntryKind::Thinking if collapses_thinking(entry, state) => {
                lines.push(Line::from(Span::styled(
                    fit_line(&collapsed_thinking_label(entry), width),
                    entry_label_style(entry, state),
                )));
            }
            _ => {
                if entry.shows_label() {
                    for label in wrapped_lines(&entry_label(entry), width) {
                        lines.push(Line::from(Span::styled(label, entry_label_style(entry, state))));
                    }
                }
                if entry.kind == EntryKind::Tool {
                    if let Some(preview) = tool_preview(entry.visible_text(), width) {
                        lines.push(Line::from(Span::styled(preview, entry_style(entry.kind, state))));
                    }
                } else if entry.visible_text().is_empty() {
                    if entry.shows_label() {
                        lines.push(Line::default());
                    }
                } else {
                    lines.extend(indented_body_lines(entry, width, state));
                }
            }
        }
        index += 1;
    }
    compact_blank_runs(lines)
}

/// Whether a reasoning block should render as its one-line summary.
///
/// Only finished blocks collapse: while the model is still reasoning the
/// stream stays visible so the operator can follow along.
fn collapses_thinking(entry: &TranscriptEntry, state: &AppState) -> bool {
    state.thinking_display.collapses_when_finished() && entry.is_finished()
}

/// One-line stand-in for a finished reasoning block.
fn collapsed_thinking_label(entry: &TranscriptEntry) -> String {
    let lines = entry.body_line_count();
    let unit = if lines == 1 { "line" } else { "lines" };
    match entry.elapsed() {
        Some(elapsed) => format!("• Thinking ({lines} {unit} · {:.1}s)", elapsed.as_secs_f64()),
        None => format!("• Thinking ({lines} {unit})"),
    }
}

/// Render a non-tool entry's body indented under its header.
///
/// The indent is what makes a multi-chunk block read as one unit: without it
/// a long thinking stream is indistinguishable from the assistant's answer.
///
/// A live reasoning block is capped to the most recent
/// [`THINKING_LIVE_ROWS`] rows so a long chain of thought cannot push the
/// rest of the conversation off screen.
fn indented_body_lines(entry: &TranscriptEntry, width: u16, state: &AppState) -> Vec<Line<'static>> {
    let body_width = width.saturating_sub(BODY_INDENT_WIDTH).max(1);
    let style = entry_style(entry.kind, state);
    let mut lines = Vec::new();
    for source in entry.visible_text().split('\n') {
        for line in wrapped_lines(source, body_width) {
            lines.push(Line::from(Span::styled(format!("{BODY_INDENT}{line}"), style)));
        }
    }

    let windowed = entry.kind == EntryKind::Thinking && state.thinking_display.collapses_when_finished();
    if windowed && lines.len() > THINKING_LIVE_ROWS {
        lines.drain(..lines.len() - THINKING_LIVE_ROWS);
    }
    lines
}

fn pending_transcript_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    transcript_lines(state.pending_transcript(), width, state)
}

/// Squeeze a rendered block: no leading blank rows, and never two blank rows
/// in a row.
///
/// Blank rows arrive from three independent places — the separator between
/// entries, the model's own markdown, and the boundary each streaming flush
/// appends — so they stack up unless something collapses them. Trailing blanks
/// are capped at one so consecutive flushes stay one row apart.
fn compact_blank_runs(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let is_blank = |line: &Line<'static>| line.spans.iter().all(|span| span.content.trim().is_empty());
    let mut compacted: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    for line in lines {
        if is_blank(&line) && compacted.last().is_none_or(&is_blank) {
            continue;
        }
        compacted.push(line);
    }
    compacted
}

fn line_commit_boundary(text: &str) -> usize {
    text.rfind('\n')
        .map(|index| index + '\n'.len_utf8())
        .filter(|boundary| *boundary < text.len())
        .unwrap_or(0)
}

fn markdown_commit_boundary(text: &str) -> usize {
    let mut in_fence = false;
    let mut offset = 0;
    let mut boundary = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        offset += line.len();
        if !in_fence && trimmed.is_empty() && offset < text.len() {
            boundary = offset;
        }
    }
    boundary
}

fn wrapped_lines(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    for character in text.chars() {
        let character_width = character.width().unwrap_or(1).max(1);
        if line_width > 0 && line_width + character_width > width {
            lines.push(line);
            line = String::new();
            line_width = 0;
        }
        line.push(character);
        line_width += character_width;
    }
    if !line.is_empty() || text.is_empty() {
        lines.push(line);
    }
    lines
}

fn welcome_transcript_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    let mut lines = if width >= 50 {
        agentrs_mark_lines(state)
            .into_iter()
            .map(|line| line.alignment(Alignment::Center))
            .collect()
    } else {
        Vec::new()
    };
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    lines.push(
        Line::from(Span::styled("AgentrsCLI", normal(state).add_modifier(Modifier::BOLD))).alignment(Alignment::Center),
    );
    lines.push(
        Line::from(Span::styled(
            truncate_line("Ask about this project, or type / to see commands.", width.max(1)),
            muted(state),
        ))
        .alignment(Alignment::Center),
    );
    lines.push(Line::default());
    lines
}

fn render_welcome(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let welcome_height = AGENTRS_MARK_LINES.len() as u16 + 3;
    if area.height < welcome_height || area.width < 50 {
        let render_area = bottom_aligned_area(area, 1, TRANSCRIPT_BOTTOM_GAP);
        frame.render_widget(
            Paragraph::new("Ask about this project, or type / to see commands.")
                .alignment(Alignment::Center)
                .style(muted(state)),
            render_area,
        );
        return;
    }

    let top_padding = area
        .height
        .saturating_sub(welcome_height.saturating_add(TRANSCRIPT_BOTTOM_GAP));
    let mut lines = vec![Line::default(); top_padding as usize];
    lines.extend(agentrs_mark_lines(state));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "AgentrsCLI",
        normal(state).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "Ask about this project, or type / to see commands.",
        muted(state),
    )));
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

fn bottom_aligned_area(area: Rect, content_height: usize, preferred_gap: u16) -> Rect {
    let content_height = content_height.min(u16::MAX as usize) as u16;
    if content_height >= area.height {
        return area;
    }
    let available_gap = area.height.saturating_sub(content_height);
    let gap = preferred_gap.min(available_gap);
    Rect::new(
        area.x,
        area.bottom().saturating_sub(content_height).saturating_sub(gap),
        area.width,
        content_height,
    )
}

fn agentrs_mark_lines(state: &AppState) -> Vec<Line<'static>> {
    AGENTRS_MARK_LINES
        .iter()
        .map(|source| {
            debug_assert_eq!(source.len(), AGENTRS_MARK_WIDTH);
            let spans = source
                .chars()
                .map(|glyph| {
                    let style = if glyph == ' ' { Style::default() } else { muted(state) };
                    Span::styled(glyph.to_string(), style)
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect()
}

fn consecutive_tool_end(entries: &[TranscriptEntry], start: usize) -> usize {
    if entries.get(start).is_none_or(|entry| entry.kind != EntryKind::Tool) {
        return start.saturating_add(1).min(entries.len());
    }
    entries[start..]
        .iter()
        .position(|entry| entry.kind != EntryKind::Tool)
        .map_or(entries.len(), |offset| start + offset)
}

fn tool_group_lines(entries: &[TranscriptEntry], width: u16, state: &AppState) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "• Tools",
        muted(state).add_modifier(Modifier::BOLD),
    ))];
    let hidden = entries.len().saturating_sub(MAX_VISIBLE_TOOLS);
    if hidden > 0 {
        lines.push(Line::from(Span::styled(
            truncate_line(&format!("  … {hidden} earlier tools"), width),
            muted(state),
        )));
    }
    let visible = &entries[hidden..];
    for (index, entry) in visible.iter().enumerate() {
        let last = index + 1 == visible.len();
        let branch = if last { "└─" } else { "├─" };
        let status = entry.tool_status.unwrap_or(ToolStepStatus::Queued).label();
        let label = truncate_line(&format!("  {branch} {}  {status}", entry.label), width);
        lines.push(Line::from(Span::styled(label, entry_label_style(entry, state))));
        let indent = if last { "     " } else { "  │  " };
        if let Some(preview) = tool_preview_with_indent(&entry.text, width, indent) {
            lines.push(Line::from(Span::styled(preview, entry_style(entry.kind, state))));
        }
    }
    lines
}

fn conversation_lines(text: &str, width: u16) -> Vec<String> {
    let line_width = usize::from(width.max(2));
    let content_width = line_width.saturating_sub(2).max(1);
    let mut lines = Vec::new();

    for source_line in text.split('\n') {
        let mut chunk = String::new();
        let mut chunk_width = 0usize;
        for character in source_line.chars() {
            let character_width = character.width().unwrap_or(1);
            if chunk_width > 0 && chunk_width + character_width > content_width {
                lines.push(padded_message_line(&chunk, chunk_width, content_width));
                chunk.clear();
                chunk_width = 0;
            }
            chunk.push(character);
            chunk_width += character_width;
        }
        if chunk_width > 0 || source_line.is_empty() {
            lines.push(padded_message_line(&chunk, chunk_width, content_width));
        }
    }
    lines
}

fn user_message_lines(text: &str, width: u16, state: &AppState) -> Vec<Line<'static>> {
    let content = conversation_lines(text, width);
    let mut lines = Vec::with_capacity(content.len().saturating_add(2));
    if !state.no_color {
        lines.push(Line::from(Span::styled(
            "▄".repeat(usize::from(width)),
            user_bubble_cap_style(state),
        )));
    }
    lines.extend(
        content
            .into_iter()
            .map(|line| Line::from(Span::styled(line, entry_style(EntryKind::User, state)))),
    );
    if !state.no_color {
        lines.push(Line::from(Span::styled(
            "▀".repeat(usize::from(width)),
            user_bubble_cap_style(state),
        )));
    }
    lines
}

fn padded_message_line(text: &str, text_width: usize, content_width: usize) -> String {
    format!(" {text}{} ", " ".repeat(content_width.saturating_sub(text_width)))
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let block = Block::default().borders(Borders::TOP).border_style(divider(state));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text = state.composer.text();
    let placeholder = text.is_empty() && !state.busy && !state.initializing;
    let displayed = if state.initializing {
        "Starting AgentrsCLI…".to_string()
    } else if placeholder {
        "Type a message…".to_string()
    } else {
        text
    };
    let (cursor_column, cursor_row) = state.composer.visual_cursor(inner.width.max(1));
    let vertical_scroll = cursor_row.saturating_sub(inner.height.saturating_sub(1));
    let style = if placeholder { muted(state) } else { normal(state) };
    frame.render_widget(
        Paragraph::new(displayed)
            .style(style)
            .wrap(Wrap { trim: false })
            .scroll((vertical_scroll, 0)),
        inner,
    );
    if !state.initializing && !state.busy && state.approval.is_none() && !state.session_picker.is_visible() {
        frame.set_cursor_position((
            inner.x.saturating_add(cursor_column),
            inner.y.saturating_add(cursor_row.saturating_sub(vertical_scroll)),
        ));
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let spinner = ["·", "••", "•••", "••"]
        .get(state.spinner_frame)
        .copied()
        .unwrap_or("·");
    let status = if state.initializing {
        "starting".to_string()
    } else if state.busy {
        // Naming the active task turns an anonymous spinner into progress the
        // user can actually read.
        match state.active_todo_label() {
            Some(label) => format!("{spinner} {label}"),
            None => spinner.to_string(),
        }
    } else {
        "ready".to_string()
    };
    let metadata = format!(
        " AgentrsCLI · {} · {} · {} · {} ",
        state.provider,
        state.model,
        status,
        compact_cwd(&state.cwd)
    );
    let session = format!(" session {} ", state.session_id.as_deref().unwrap_or("new"));
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(metadata, muted(state))),
            Line::from(Span::styled(session, muted(state))),
        ]),
        area,
    );
}

fn render_command_popup(frame: &mut Frame<'_>, bounds: Rect, composer: Rect, state: &AppState) {
    let matches = state.popup.matches();
    let visible = matches.len().min(MAX_POPUP_ITEMS);
    let height = visible as u16;
    let width = bounds.width;
    let x = bounds.x;
    let y = composer.y.saturating_sub(height);
    let area = Rect::new(x, y, width, height);
    let mut lines = Vec::with_capacity(visible);
    for (index, command) in matches.iter().take(visible).enumerate() {
        let selected = index == state.popup.selected();
        let marker = if selected { "›" } else { " " };
        let line = format!("{marker} /{:<12} {}", command.name, command.description);
        let line_width = UnicodeWidthStr::width(line.as_str());
        let line = format!("{line}{}", " ".repeat(width as usize - line_width.min(width as usize)));
        let style = if selected { selected_style(state) } else { normal(state) };
        lines.push(Line::from(Span::styled(line, style)));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_session_picker(frame: &mut Frame<'_>, bounds: Rect, state: &AppState) {
    let sessions = state.session_picker.sessions();
    if sessions.is_empty() {
        return;
    }

    let available_height = bounds.height;
    let visible = usize::from(available_height.saturating_sub(3) / 2)
        .max(1)
        .min(sessions.len());
    let area = bounds;
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title(format!(" Resume session · {} ", sessions.len()))
        .border_style(divider(state));
    let inner = block.inner(area);
    let selected = state.session_picker.selected();
    let start = selected
        .saturating_sub(visible.saturating_sub(1))
        .min(sessions.len().saturating_sub(visible));
    let mut lines = Vec::with_capacity(visible * 2 + 1);

    for (index, session) in sessions.iter().enumerate().skip(start).take(visible) {
        let is_selected = index == selected;
        let marker = if is_selected { "›" } else { " " };
        let summary = fit_line(&format!("{marker} {}", session.summary()), inner.width);
        let details = fit_line(
            &format!(
                "  {} · {} · {} · {} messages",
                session.id(),
                session.model(),
                session.updated_at(),
                session.message_count()
            ),
            inner.width,
        );
        let summary_style = if is_selected {
            selected_style(state)
        } else {
            normal(state)
        };
        let detail_style = if is_selected {
            selected_style(state)
        } else {
            muted(state)
        };
        lines.push(Line::from(Span::styled(summary, summary_style)));
        lines.push(Line::from(Span::styled(details, detail_style)));
    }
    lines.push(Line::from(Span::styled(
        fit_line(" ↑/↓ select · Enter resume · Esc close", inner.width),
        muted(state),
    )));

    frame.render_widget(Clear, bounds);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn fit_line(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    let text_width = UnicodeWidthStr::width(text);
    if text_width <= width {
        return format!("{text}{}", " ".repeat(width - text_width));
    }
    truncate_line(text, width as u16)
}

fn tool_preview(text: &str, width: u16) -> Option<String> {
    tool_preview_with_indent(text, width, "  ")
}

fn tool_preview_with_indent(text: &str, width: u16, indent: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let indent_width = UnicodeWidthStr::width(indent);
    if normalized.is_empty() || usize::from(width) <= indent_width {
        return None;
    }
    let preview_width = width
        .saturating_sub(indent_width.min(u16::MAX as usize) as u16)
        .min(120);
    let preview = truncate_line(&normalized, preview_width);
    Some(format!("{indent}{preview}"))
}

fn truncate_line(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }

    let content_width = width.saturating_sub(1);
    let mut truncated = String::new();
    let mut current_width = 0;
    for character in text.chars() {
        let character_width = character.width().unwrap_or(1);
        if current_width + character_width > content_width {
            break;
        }
        truncated.push(character);
        current_width += character_width;
    }
    truncated.push('…');
    truncated
}

fn render_approval(frame: &mut Frame<'_>, bounds: Rect, state: &AppState) {
    let Some(request) = &state.approval else {
        return;
    };
    let width = bounds.width.saturating_sub(4).clamp(20, 88);
    // The inline viewport is short, so the dialog takes everything it is
    // given: rows spent on margin are rows of tool input the operator cannot
    // see before approving.
    let height = bounds.height.clamp(8, APPROVAL_MAX_HEIGHT);
    let area = centered_rect(width, height, bounds);
    let description = if request.description.trim().is_empty() {
        "This tool needs your approval."
    } else {
        request.description.as_str()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Tool approval ")
        .border_style(warning(state));
    let inner = block.inner(area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)])
        .split(inner);
    let body_height = usize::from(chunks[0].height);
    let mut body_lines = vec![Line::from(Span::styled(
        request.name.clone(),
        normal(state).add_modifier(Modifier::BOLD),
    ))];
    // Spacer rows are the first thing sacrificed when the input is long.
    let input_rows: usize = request
        .input
        .lines()
        .map(|line| wrapped_row_count(line, chunks[0].width))
        .sum();
    let roomy = body_height.saturating_sub(input_rows) >= SPACER_ROWS;
    if roomy {
        body_lines.push(Line::default());
    }
    body_lines.push(Line::from(description.to_string()));
    if roomy {
        body_lines.push(Line::default());
    }
    let hidden_line_count = approval_hidden_line_count(
        &request.input,
        body_height.saturating_sub(body_lines.len()),
        chunks[0].width,
    );
    // The elision marker gets a row of its own. Folding it into the wrapped
    // body would let word wrapping push it off the bottom, turning a partial
    // view into one that looks complete.
    let (body_area, marker_area) = if hidden_line_count > 0 {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(chunks[0]);
        (split[0], Some(split[1]))
    } else {
        (chunks[0], None)
    };

    let input_budget = usize::from(body_area.height).saturating_sub(body_lines.len());
    body_lines.extend(approval_input_lines(
        &request.input,
        input_budget,
        body_area.width,
        state,
    ));
    let body = Text::from(body_lines);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), body_area);
    if let Some(marker_area) = marker_area {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit_line(&format!("… {hidden_line_count} more lines"), marker_area.width),
                muted(state).add_modifier(Modifier::ITALIC),
            ))),
            marker_area,
        );
    }
    frame.render_widget(Paragraph::new(approval_actions(request.choice, inner.width)), chunks[1]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            fit_line(" ←/→ select · Enter confirm · Esc deny", inner.width),
            muted(state),
        ))),
        chunks[2],
    );
}

/// How many source lines of `input` cannot be shown in `budget` rows.
///
/// Returns 0 when the whole payload fits, which is the signal to skip the
/// elision marker entirely.
fn approval_hidden_line_count(input: &str, budget: usize, width: u16) -> usize {
    let source: Vec<&str> = input.lines().collect();
    let total_rows: usize = source.iter().map(|line| wrapped_row_count(line, width)).sum();
    if total_rows <= budget {
        return 0;
    }
    // The marker itself costs a row, so the body is one row shorter.
    let body_budget = budget.saturating_sub(1);
    source
        .len()
        .saturating_sub(fitting_line_count(&source, body_budget, width))
}

/// Number of leading lines whose wrapped height fits in `budget` rows.
fn fitting_line_count(source: &[&str], budget: usize, width: u16) -> usize {
    let mut used = 0;
    let mut count = 0;
    for line in source {
        let rows = wrapped_row_count(line, width);
        if used + rows > budget {
            break;
        }
        used += rows;
        count += 1;
    }
    count
}

/// Split a tool's pretty-printed JSON input into one [`Line`] per source line.
///
/// `Line` does not interpret `\n`, so passing the whole payload as a single
/// span collapses the pretty-printing into an unreadable blob — the operator
/// then cannot see what they are approving.
///
/// `budget` counts *rendered rows*, not source lines: the paragraph wraps, so
/// a long value can occupy several rows. Overflow is left to the caller's
/// elision marker; a partially shown line ends in an ellipsis.
fn approval_input_lines(input: &str, budget: usize, width: u16, state: &AppState) -> Vec<Line<'static>> {
    let style = muted(state);
    let source: Vec<&str> = input.lines().collect();
    if budget == 0 || source.is_empty() {
        return Vec::new();
    }

    let shown = fitting_line_count(&source, budget, width);
    let mut lines: Vec<Line<'static>> = source
        .iter()
        .take(shown)
        .map(|line| Line::from(Span::styled((*line).to_string(), style)))
        .collect();

    // A single oversized line (a file body, typically) would otherwise leave
    // the rest of the box blank. Show as much of its head as fits.
    let used: usize = source
        .iter()
        .take(shown)
        .map(|line| wrapped_row_count(line, width))
        .sum();
    let spare_rows = budget.saturating_sub(used);
    if spare_rows > 0
        && let Some(next) = source.get(shown)
    {
        lines.push(Line::from(Span::styled(
            truncate_to_rows(next, width, spare_rows),
            style,
        )));
    }

    lines
}

/// Rows a single logical line occupies once the paragraph wraps it.
fn wrapped_row_count(line: &str, width: u16) -> usize {
    let width = usize::from(width.max(1));
    let display_width = line.width();
    display_width.div_ceil(width).max(1)
}

/// Cut `line` down to the head that fits in `rows` wrapped rows, marking the
/// cut with an ellipsis so the operator can tell the line continues.
fn truncate_to_rows(line: &str, width: u16, rows: usize) -> String {
    let capacity = usize::from(width.max(1)).saturating_mul(rows).saturating_sub(1);
    let mut truncated = String::new();
    let mut used = 0;
    for character in line.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > capacity {
            break;
        }
        used += character_width;
        truncated.push(character);
    }
    truncated.push('…');
    truncated
}

fn approval_actions(choice: ApprovalChoice, width: u16) -> Line<'static> {
    let labels = if width >= 34 {
        [
            (ApprovalChoice::Once, " Allow once "),
            (ApprovalChoice::Always, " Always allow "),
            (ApprovalChoice::Deny, " Deny "),
        ]
    } else {
        [
            (ApprovalChoice::Once, " Once "),
            (ApprovalChoice::Always, " Always "),
            (ApprovalChoice::Deny, " Deny "),
        ]
    };
    let mut spans = Vec::with_capacity(labels.len() * 2);
    for (index, (action, label)) in labels.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let style = if choice == action {
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(label, style));
    }
    Line::from(spans).alignment(Alignment::Center)
}

fn centered_rect(width: u16, height: u16, bounds: Rect) -> Rect {
    let x = bounds.x + bounds.width.saturating_sub(width) / 2;
    let y = bounds.y + bounds.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(bounds.width), height.min(bounds.height))
}

/// Style for the half-block caps that round off the user bubble.
///
/// The caps must be painted in the bubble's fill colour, which is the
/// terminal's default foreground — the bubble body reverses fg/bg, so its
/// background *is* that colour. Leaving the style at default therefore keeps
/// the caps aligned with the bubble on light and dark terminals alike.
fn user_bubble_cap_style(state: &AppState) -> Style {
    normal(state)
}

fn entry_style(kind: EntryKind, state: &AppState) -> Style {
    match kind {
        EntryKind::User if state.no_color => normal(state),
        // Reversed rather than an explicit fg/bg pair: the bubble then adopts
        // the terminal's own colours instead of assuming a light background.
        EntryKind::User => Style::default().add_modifier(Modifier::REVERSED),
        EntryKind::Assistant => normal(state),
        EntryKind::Thinking => muted(state).add_modifier(Modifier::ITALIC),
        EntryKind::Tool => muted(state),
        EntryKind::Info => muted(state),
        EntryKind::Error => error(state),
    }
}

fn markdown_theme(state: &AppState) -> MarkdownTheme {
    let body = entry_style(EntryKind::Assistant, state);
    if state.no_color {
        return MarkdownTheme {
            body,
            inline_code: body.add_modifier(Modifier::REVERSED),
            code_block: body,
            marker: muted(state),
            rule: muted(state),
        };
    }
    // Code uses a hue rather than a fixed light background patch so it stays
    // legible whatever the terminal background is; block structure is already
    // conveyed by the renderer's indentation.
    MarkdownTheme {
        body,
        inline_code: Style::default().fg(Color::Cyan),
        code_block: Style::default().fg(Color::Cyan),
        marker: muted(state),
        rule: muted(state),
    }
}

fn entry_label(entry: &TranscriptEntry) -> String {
    match entry.kind {
        EntryKind::Thinking => "• Thinking".to_string(),
        EntryKind::Tool => {
            let status = entry.tool_status.unwrap_or(ToolStepStatus::Queued).label();
            format!("• {}  {status}", entry.label)
        }
        _ => entry.label.clone(),
    }
}

fn entry_label_style(entry: &TranscriptEntry, state: &AppState) -> Style {
    match entry.kind {
        EntryKind::Thinking => muted(state).add_modifier(Modifier::BOLD),
        EntryKind::Tool => {
            tool_status_style(entry.tool_status.unwrap_or(ToolStepStatus::Queued), state).add_modifier(Modifier::BOLD)
        }
        _ => entry_style(entry.kind, state).add_modifier(Modifier::BOLD),
    }
}

fn tool_status_style(status: ToolStepStatus, state: &AppState) -> Style {
    if state.no_color {
        return Style::default();
    }
    // ANSI names, not fixed RGB: the terminal maps them to shades that suit
    // its own background, so status stays legible in light and dark themes.
    let color = match status {
        ToolStepStatus::Queued => Color::DarkGray,
        ToolStepStatus::Approval => Color::Yellow,
        ToolStepStatus::Running => Color::Blue,
        ToolStepStatus::Success => Color::Green,
        ToolStepStatus::Error => Color::Red,
        ToolStepStatus::Cancelled => Color::Magenta,
    };
    Style::default().fg(color)
}

fn normal(_state: &AppState) -> Style {
    Style::default()
}

fn warning(state: &AppState) -> Style {
    color(state, Color::Yellow).add_modifier(Modifier::BOLD)
}

fn error(state: &AppState) -> Style {
    color(state, Color::Red).add_modifier(Modifier::BOLD)
}

fn muted(state: &AppState) -> Style {
    color(state, Color::DarkGray)
}

fn selected_style(_state: &AppState) -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn divider(state: &AppState) -> Style {
    color(state, Color::DarkGray)
}

fn color(state: &AppState, color: Color) -> Style {
    if state.no_color {
        Style::default()
    } else {
        Style::default().fg(color)
    }
}

#[cfg(test)]
#[path = "ui_test.rs"]
mod ui_test;

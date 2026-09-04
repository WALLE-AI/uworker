use agentrs_agent::commands::CommandSpec;
use agentrs_protocol::events::TodoSnapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

use super::{
    agentrs_mark_lines, entry_style, pending_history_lines, render, streaming_history_commit, welcome_history_lines,
};
use crate::event::AgentEvent;
use crate::session_picker::TuiSession;
use crate::state::{AppState, ApprovalChoice, ApprovalRequest};
use crate::transcript::{EntryKind, ToolStepStatus, TranscriptEntry};
use agentrs_config::tui::ThinkingDisplay;

#[test]
fn slash_command_popup_renders_an_unframed_selected_command() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.set_commands(vec![CommandSpec {
        name: "help".to_string(),
        aliases: Vec::new(),
        description: "List commands".to_string(),
    }]);
    state.composer.insert_text("/");
    state.popup.update("/");

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");
    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("/help"));
    assert!(!rendered.contains("Commands"));
    assert!(
        !['│', '┌', '┐', '└', '┘']
            .iter()
            .any(|character| rendered.contains(*character))
    );

    let command_row = rendered
        .lines()
        .position(|line| line.contains("/help"))
        .expect("selected command should be visible") as u16;
    let final_content_cell = terminal
        .backend()
        .buffer()
        .cell((78, command_row))
        .expect("selected command should fill the content width");
    assert!(final_content_cell.modifier.contains(Modifier::REVERSED));
}

#[test]
fn slash_command_space_is_released_when_the_popup_closes() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state.set_commands(
        (0..7)
            .map(|index| CommandSpec {
                name: format!("command{index}"),
                aliases: Vec::new(),
                description: "Description".to_string(),
            })
            .collect(),
    );
    state.composer.insert_text("/");
    state.popup.update("/");

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");
    let open = terminal.backend().to_string();
    let open_divider = open
        .lines()
        .position(|line| line.contains("────"))
        .expect("composer divider should be visible");
    assert_eq!(open_divider, 7);

    state.composer.clear();
    state.popup.update("");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");
    let closed = terminal.backend().to_string();
    let closed_divider = closed
        .lines()
        .position(|line| line.contains("────"))
        .expect("composer divider should be visible");
    assert_eq!(closed_divider, 0);
    assert!(!closed.contains("/command"));
}

#[test]
fn conversation_is_not_wrapped_in_an_outer_border() {
    let state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    assert!(
        !['│', '┌', '┐', '└', '┘']
            .iter()
            .any(|character| rendered.contains(*character))
    );
    assert!(rendered.contains("────"));
}

#[test]
fn runtime_metadata_is_rendered_in_the_footer_not_the_top_row() {
    let mut state = AppState::new(
        "gpt-5.5".to_string(),
        "openai".to_string(),
        "/workspace/project".to_string(),
        true,
    );
    state.session_id = Some("session-id".to_string());
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let lines = terminal.backend().to_string();
    let lines = lines.lines().collect::<Vec<_>>();
    assert!(!lines[0].contains("openai"));
    let composer_row = lines
        .iter()
        .position(|line| line.contains("Type a message"))
        .expect("composer should be visible");
    let metadata_row = lines
        .iter()
        .position(|line| line.contains("openai"))
        .expect("metadata should be visible");
    let session_row = lines
        .iter()
        .position(|line| line.contains("session session-id"))
        .expect("session should be visible");
    assert!(metadata_row > composer_row);
    assert_eq!(session_row, metadata_row + 1);
}

#[test]
fn welcome_renders_ascii_agentrs_mark() {
    let state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("::::::::"));
    assert!(rendered.contains("AgentrsCLI"));
}

#[test]
fn approval_modal_keeps_actions_visible_and_marks_the_selected_choice() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.handle_agent_event(AgentEvent::ApprovalRequested {
        call_id: "call-1".to_string(),
        name: "shell".to_string(),
        description: "Run a command".to_string(),
        input: "very long input ".repeat(30),
    });
    state.approval.as_mut().expect("approval should exist").choice = ApprovalChoice::Always;

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("Allow once"));
    assert!(rendered.contains("Always allow"));
    assert!(rendered.contains("Enter confirm"));
    let action_row = rendered
        .lines()
        .position(|line| line.contains("Always allow"))
        .expect("approval actions should be visible") as u16;
    let action_line = rendered
        .lines()
        .nth(usize::from(action_row))
        .expect("action row should exist");
    let action_column = action_line
        .find("Always allow")
        .expect("selected action should be present") as u16;
    let selected = terminal
        .backend()
        .buffer()
        .cell((action_column, action_row))
        .expect("selected action cell should exist");
    assert!(selected.modifier.contains(Modifier::REVERSED));
}

#[test]
fn welcome_header_stays_close_to_the_composer() {
    let state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    let lines = rendered.lines().collect::<Vec<_>>();
    let subtitle_row = lines
        .iter()
        .position(|line| line.contains("Ask about this project"))
        .expect("welcome subtitle should be visible");
    let composer_divider_row = lines
        .iter()
        .position(|line| line.contains("────"))
        .expect("composer divider should be visible");
    assert_eq!(composer_divider_row.saturating_sub(subtitle_row), 2);
}

#[test]
fn welcome_ascii_uses_a_muted_foreground_without_a_background() {
    let state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    let mark = agentrs_mark_lines(&state);
    let colored = mark
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.contains(':'))
        .expect("mark should contain a dense colored cell");
    // A fixed near-black RGB would vanish on a dark terminal; the mark must
    // use a shade the terminal resolves against its own background.
    assert_eq!(colored.style.fg, Some(Color::DarkGray));
    assert_eq!(colored.style.bg, None);
    assert!(
        mark.iter()
            .flat_map(|line| &line.spans)
            .flat_map(|span| span.content.chars())
            .all(|character| matches!(character, ':' | ' '))
    );
    for line in mark {
        let rendered = line.to_string();
        assert_eq!(rendered.chars().count(), 24);
        assert_eq!(rendered, rendered.chars().rev().collect::<String>());
    }
}

#[test]
fn welcome_mark_is_committed_separately_from_the_first_message() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    let welcome = welcome_history_lines(&state, 78)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(welcome.contains("::::::::"));

    state.show_welcome = false;
    state.begin_turn("first message");

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    assert!(!rendered.contains("::::::::"));
    assert!(rendered.contains("first message"));
}

#[test]
fn resumed_history_replay_contains_the_entire_session() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::User, "", "old question"));
    state.transcript.push(TranscriptEntry::new(
        EntryKind::Assistant,
        "",
        (0..30).map(|index| format!("old answer {index}\n")).collect::<String>(),
    ));
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::User, "", "latest question"));
    state.transcript.push(TranscriptEntry::new(
        EntryKind::Assistant,
        "",
        (0..30)
            .map(|index| format!("latest answer {index}\n"))
            .collect::<String>(),
    ));
    let rendered = pending_history_lines(&state, 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("old question"));
    assert!(rendered.contains("old answer 0"));
    assert!(rendered.contains("latest question"));
    assert!(rendered.contains("latest answer 29"));
}

#[test]
fn committed_history_leaves_only_new_transcript_pending() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::User, "", "first question"));
    state.transcript.push(TranscriptEntry::new(
        EntryKind::Assistant,
        "",
        (0..80)
            .map(|index| format!("answer line {index}\n"))
            .collect::<String>(),
    ));
    let history = pending_history_lines(&state, 80);
    assert!(history.iter().any(|line| line.to_string().contains("first question")));
    assert!(history.iter().any(|line| line.to_string().contains("answer line 79")));

    state.mark_transcript_committed();
    assert!(pending_history_lines(&state, 80).is_empty());
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::User, "", "next question"));
    let pending = pending_history_lines(&state, 80);
    assert_eq!(pending.len(), 1);
    assert!(pending[0].to_string().contains("next question"));
}

#[test]
fn streaming_markdown_commits_complete_paragraphs_and_keeps_the_active_tail() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state.begin_turn("question");
    state.mark_transcript_committed();
    state.handle_agent_event(crate::event::AgentEvent::TextDelta(
        "first paragraph\n\nsecond paragraph is still streaming".to_string(),
    ));

    let commit = streaming_history_commit(&state, 32).expect("the completed paragraph should be committed");
    let committed = commit
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(committed.contains("first paragraph"));
    assert!(!committed.contains("second paragraph"));

    state.commit_streaming_prefix(commit.complete_entries, commit.active_byte_count);
    for width in [24, 48] {
        let pending = pending_history_lines(&state, width)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!pending.contains("first paragraph"));
        assert!(pending.contains("second paragraph"));
    }
}

#[test]
fn streaming_markdown_does_not_commit_an_unclosed_code_fence() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state.begin_turn("question");
    state.mark_transcript_committed();
    state.handle_agent_event(crate::event::AgentEvent::TextDelta(
        "```rust\nfn main() {\n\n    println!(\"hello\");\n".to_string(),
    ));

    assert!(streaming_history_commit(&state, 32).is_none());
}

#[test]
fn streaming_history_moves_completed_tool_steps_before_the_active_answer() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state.begin_turn("question");
    state.mark_transcript_committed();
    state
        .transcript
        .push(TranscriptEntry::tool("Read", "file contents", ToolStepStatus::Success));
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::Assistant, "", "answer in progress"));

    let commit = streaming_history_commit(&state, 48).expect("the completed tool should be committed");
    assert_eq!(commit.complete_entries, 1);
    assert_eq!(commit.active_byte_count, 0);
    assert!(commit.lines.iter().any(|line| line.to_string().contains("Read")));
    assert!(
        !commit
            .lines
            .iter()
            .any(|line| line.to_string().contains("answer in progress"))
    );
}

#[test]
fn streaming_history_keeps_a_running_tool_mutable() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state.begin_turn("question");
    state.mark_transcript_committed();
    state
        .transcript
        .push(TranscriptEntry::tool("Read", "request", ToolStepStatus::Running));
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::Assistant, "", "answer in progress"));

    assert!(streaming_history_commit(&state, 48).is_none());
}

#[test]
fn committed_history_does_not_leave_a_blank_transcript_viewport() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::Assistant, "", "completed answer"));
    state.mark_transcript_committed();

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");
    let rendered = terminal.backend().to_string();
    let lines = rendered.lines().collect::<Vec<_>>();
    let composer_divider_row = lines
        .iter()
        .position(|line| line.contains("────"))
        .expect("composer divider should be visible");
    assert_eq!(composer_divider_row, 0);
}

#[test]
fn thinking_and_tools_render_countable_step_markers() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    state.show_welcome = false;
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::Thinking, "Thinking", "checking"));
    state
        .transcript
        .push(TranscriptEntry::tool("Read", "file contents", ToolStepStatus::Success));

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("• Thinking"));
    assert!(rendered.contains("• Read  done"));
}

#[test]
fn consecutive_tools_render_as_one_appended_group() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    state.show_welcome = false;
    state
        .transcript
        .push(TranscriptEntry::tool("Read", "src/main.rs", ToolStepStatus::Success));
    state.transcript.push(TranscriptEntry::tool(
        "Grep",
        "matched two files",
        ToolStepStatus::Success,
    ));

    let backend = TestBackend::new(80, 18);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("• Tools"));
    assert!(rendered.contains("├─ Read  done"));
    assert!(rendered.contains("└─ Grep  done"));
    assert!(rendered.contains("src/main.rs"));
    assert!(rendered.contains("matched two files"));
    assert!(!rendered.contains("• Read"));
    assert!(!rendered.contains("• Grep"));
    assert!(
        rendered.find("├─ Read").expect("Read should be visible")
            < rendered.find("└─ Grep").expect("Grep should be visible")
    );
}

#[test]
fn tool_group_keeps_only_the_three_most_recent_calls() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    for index in 1..=5 {
        state.transcript.push(TranscriptEntry::tool(
            format!("Tool{index}"),
            format!("output {index}"),
            ToolStepStatus::Success,
        ));
    }

    let rendered = pending_history_lines(&state, 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("… 2 earlier tools"));
    assert!(!rendered.contains("Tool1"));
    assert!(!rendered.contains("output 2"));
    assert!(rendered.contains("Tool3"));
    assert!(rendered.contains("Tool4"));
    assert!(rendered.contains("Tool5"));
}

#[test]
fn assistant_markdown_is_rendered_but_user_markdown_stays_literal() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    state.show_welcome = false;
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::User, "", "**literal** `input`"));
    state.transcript.push(TranscriptEntry::new(
        EntryKind::Assistant,
        "",
        "Use `cargo check` and **review**.\n\n```rust\nfn main() {}\n```",
    ));

    let lines = pending_history_lines(&state, 48);
    let rendered = lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(rendered.contains("**literal** `input`"));
    assert!(rendered.contains("Use cargo check and review."));
    assert!(!rendered.contains("```"));
    assert!(!rendered.contains("rust"));

    let code = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "cargo check")
        .expect("inline code should be rendered as a styled span");
    // Distinguished by hue, not by a light background patch that would only
    // read correctly on a light terminal.
    assert_eq!(code.style.fg, Some(Color::Cyan));
    assert_eq!(code.style.bg, None);
    let strong = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "review")
        .expect("strong text should be rendered as a styled span");
    assert!(strong.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn assistant_code_block_refills_after_resize() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    state.show_welcome = false;
    state.transcript.push(TranscriptEntry::new(
        EntryKind::Assistant,
        "",
        "```text\nresponsive code\n```",
    ));

    for width in [40, 64] {
        let lines = pending_history_lines(&state, width);
        let code_line = lines
            .iter()
            .find(|line| line.to_string().contains("responsive code"))
            .expect("code line should be present");
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(code_line.to_string().as_str()),
            width as usize
        );
    }
}

#[test]
fn tool_states_use_distinct_semantic_colors_and_text() {
    let status_rows = [
        ("Queued", ToolStepStatus::Queued),
        ("Approval", ToolStepStatus::Approval),
        ("Running", ToolStepStatus::Running),
        ("Success", ToolStepStatus::Success),
        ("Error", ToolStepStatus::Error),
        ("Cancelled", ToolStepStatus::Cancelled),
    ];
    // ANSI names rather than fixed RGB, so each status keeps its meaning on
    // both light and dark terminal themes.
    let expected_colors = [
        Color::DarkGray,
        Color::Yellow,
        Color::Blue,
        Color::Green,
        Color::Red,
        Color::Magenta,
    ];
    for ((name, status), color) in status_rows.into_iter().zip(expected_colors) {
        let mut state = AppState::new(
            "model".to_string(),
            "provider".to_string(),
            "/workspace".to_string(),
            false,
        );
        state.show_welcome = false;
        state.transcript.push(TranscriptEntry::tool(name, "preview", status));
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
        terminal
            .draw(|frame| render(frame, &state))
            .expect("render should succeed");
        let rendered = terminal.backend().to_string();
        let row = rendered
            .lines()
            .position(|line| line.contains(name))
            .expect("tool status should be visible") as u16;
        let marker = terminal
            .backend()
            .buffer()
            .cell((1, row))
            .expect("tool marker should exist");
        assert_eq!(marker.fg, color);
    }
}

#[test]
fn tool_output_is_a_responsive_single_line_preview() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state.transcript.push(TranscriptEntry::tool(
        "Read",
        "first line\nsecond line with a very long result that should not take over the terminal window",
        ToolStepStatus::Success,
    ));

    let backend = TestBackend::new(44, 12);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("first line second line"));
    assert!(rendered.contains('…'));
    assert!(!rendered.contains("terminal window"));
}

#[test]
fn user_and_assistant_messages_have_distinct_backgrounds() {
    let state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    let user = entry_style(EntryKind::User, &state);
    let assistant = entry_style(EntryKind::Assistant, &state);
    // The user bubble inverts the terminal's own colours instead of pinning a
    // background, and the assistant keeps the default foreground — neither may
    // assume the terminal is light.
    assert!(user.add_modifier.contains(Modifier::REVERSED));
    assert_eq!(user.fg, None);
    assert_eq!(user.bg, None);
    assert!(!user.add_modifier.contains(Modifier::BOLD));
    assert_eq!(assistant.fg, None);
    assert_eq!(assistant.bg, None);
}

#[test]
fn message_background_fills_the_current_render_width() {
    for width in [40, 64] {
        let mut state = AppState::new(
            "model".to_string(),
            "provider".to_string(),
            "/workspace".to_string(),
            false,
        );
        state
            .transcript
            .push(TranscriptEntry::new(EntryKind::User, "", "short message"));
        let backend = TestBackend::new(width, 16);
        let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
        terminal
            .draw(|frame| render(frame, &state))
            .expect("render should succeed");

        let rendered = terminal.backend().to_string();
        let message_row = rendered
            .lines()
            .position(|line| line.contains("short message"))
            .expect("message should be visible") as u16;
        let final_content_column = width - 2;
        let cell = terminal
            .backend()
            .buffer()
            .cell((final_content_column, message_row))
            .expect("last message cell should exist");
        assert!(cell.modifier.contains(Modifier::REVERSED));
    }
}

#[test]
fn user_message_background_has_half_row_vertical_padding() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    state.show_welcome = false;
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::User, "", "message"));

    let lines = pending_history_lines(&state, 24);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].to_string(), "▄".repeat(24));
    assert_eq!(lines[2].to_string(), "▀".repeat(24));
    // Caps are painted in the default foreground so the half blocks match the
    // reversed bubble fill; reversing them too would flip the halves.
    for cap in [&lines[0], &lines[2]] {
        assert_eq!(cap.spans[0].style.fg, None);
        assert!(!cap.spans[0].style.add_modifier.contains(Modifier::REVERSED));
    }
}

#[test]
fn short_transcript_stays_close_to_the_composer_after_resize() {
    for height in [16, 30] {
        let mut state = AppState::new(
            "model".to_string(),
            "provider".to_string(),
            "/workspace".to_string(),
            true,
        );
        state.show_welcome = false;
        state
            .transcript
            .push(TranscriptEntry::new(EntryKind::Assistant, "", "short answer"));
        let backend = TestBackend::new(80, height);
        let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
        terminal
            .draw(|frame| render(frame, &state))
            .expect("render should succeed");

        let rendered = terminal.backend().to_string();
        let lines = rendered.lines().collect::<Vec<_>>();
        let answer_row = lines
            .iter()
            .position(|line| line.contains("short answer"))
            .expect("answer should be visible");
        let composer_divider_row = lines
            .iter()
            .position(|line| line.contains("────"))
            .expect("composer divider should be visible");
        assert_eq!(composer_divider_row.saturating_sub(answer_row), 2);
    }
}

#[test]
fn footer_contains_only_runtime_metadata() {
    let state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    let lines: Vec<_> = rendered.lines().collect();
    assert!(lines.iter().any(|line| line.contains("AgentrsCLI")));
    assert!(lines.iter().any(|line| line.contains("session new")));
    assert!(!rendered.contains("Enter send"));
    assert!(!rendered.contains("Shift+Enter"));
    assert!(!rendered.contains("mouse wheel"));
    assert!(!rendered.contains("drag to select"));
}

#[test]
fn undersized_terminal_shows_resize_message() {
    let state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    let backend = TestBackend::new(21, 7);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");
    assert!(terminal.backend().to_string().contains("Terminal too small"));
}

#[test]
fn initialization_state_replaces_the_terminal_before_bootstrap() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.begin_initialization();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("Starting AgentrsCLI"));
    assert!(rendered.contains("starting"));
    assert!(!rendered.contains("Type a message"));
}

#[test]
fn resume_picker_renders_a_full_screen_divider_window() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    state.session_picker.set_sessions(vec![
        TuiSession::new(
            "session-one".to_string(),
            "gpt-5.5".to_string(),
            "First task".to_string(),
            "2026-08-13 12:00 UTC".to_string(),
            4,
        ),
        TuiSession::new(
            "session-two".to_string(),
            "gpt-5.5".to_string(),
            "Second task".to_string(),
            "2026-08-13 13:00 UTC".to_string(),
            8,
        ),
    ]);
    state.session_picker.open();
    state.session_picker.move_next();

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("Resume session · 2"));
    assert!(rendered.contains("First task"));
    assert!(rendered.contains("Second task"));
    assert!(!rendered.contains("Type a message"));
    assert!(!rendered.contains("AgentrsCLI ·"));
    assert!(!rendered.contains('│'));

    let selected_row = rendered
        .lines()
        .position(|line| line.contains("Second task"))
        .expect("selected session should be visible") as u16;
    let selected_width = (0..80)
        .filter(|column| {
            terminal
                .backend()
                .buffer()
                .cell((*column, selected_row))
                .is_some_and(|cell| cell.modifier.contains(Modifier::REVERSED))
        })
        .count();
    assert!(selected_width >= 70, "selected row should fill the picker width");
}

#[test]
fn approval_dialog_keeps_pretty_json_on_separate_lines() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    state.show_welcome = false;
    state.approval = Some(ApprovalRequest {
        call_id: "call-1".to_string(),
        name: "Write".to_string(),
        description: "Write to /tmp/demo.txt".to_string(),
        input: "{\n  \"file_path\": \"/tmp/demo.txt\",\n  \"content\": \"first\\nsecond\"\n}".to_string(),
        choice: ApprovalChoice::Once,
    });

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");
    let rendered = terminal.backend().to_string();

    // Each JSON line must occupy its own row; collapsing them into one span
    // makes a Write approval unreviewable.
    let file_path_row = rendered
        .lines()
        .position(|line| line.contains("\"file_path\""))
        .expect("file_path line should be visible");
    let content_row = rendered
        .lines()
        .position(|line| line.contains("\"content\""))
        .expect("content line should be visible");
    assert_ne!(file_path_row, content_row);
    assert!(!rendered.contains("{  \"file_path\""));
}

#[test]
fn approval_dialog_reports_how_many_input_lines_it_elided() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        false,
    );
    state.show_welcome = false;
    let input = (0..200)
        .map(|index| format!("  \"line{index}\": {index},"))
        .collect::<Vec<_>>()
        .join("\n");
    state.approval = Some(ApprovalRequest {
        call_id: "call-2".to_string(),
        name: "Write".to_string(),
        description: "Write a long file".to_string(),
        input,
        choice: ApprovalChoice::Once,
    });

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");
    let rendered = terminal.backend().to_string();

    // Truncation must be visible: a silently cut input reads as the whole
    // payload and hides what is being approved.
    assert!(rendered.contains("more lines"));
    assert!(rendered.contains("Allow once"));
}

#[test]
fn streamed_thinking_prints_its_header_once_across_flushes() {
    // Full mode is the one that streams reasoning into scrollback; collapsed
    // mode keeps it in the viewport and never flushes the body.
    let mut state = AppState::with_thinking_display(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
        ThinkingDisplay::Full,
    );
    state.show_welcome = false;
    state.begin_turn("question");
    state.mark_transcript_committed();

    state.handle_agent_event(AgentEvent::Thinking("first thought\ntail".to_string()));
    let first = streaming_history_commit(&state, 40).expect("the first line should be committed");
    let first_text = first.lines.iter().map(ToString::to_string).collect::<Vec<_>>();
    state.commit_streaming_prefix(first.complete_entries, first.active_byte_count);

    state.handle_agent_event(AgentEvent::Thinking(" continues\nsecond thought\ntail".to_string()));
    let second = streaming_history_commit(&state, 40).expect("the second line should be committed");
    let second_text = second.lines.iter().map(ToString::to_string).collect::<Vec<_>>();

    // One header for the whole block: repeating it per chunk turns a single
    // thought into a wall of "Thinking" banners.
    assert_eq!(first_text.iter().filter(|line| line.contains("Thinking")).count(), 1);
    assert_eq!(second_text.iter().filter(|line| line.contains("Thinking")).count(), 0);
    assert!(second_text.iter().any(|line| line.contains("second thought")));
}

#[test]
fn streamed_thinking_does_not_emit_a_blank_row_per_flush() {
    let mut state = AppState::with_thinking_display(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
        ThinkingDisplay::Full,
    );
    state.show_welcome = false;
    state.begin_turn("question");
    state.mark_transcript_committed();
    state.handle_agent_event(AgentEvent::Thinking("only line\nstill streaming".to_string()));

    let commit = streaming_history_commit(&state, 40).expect("the finished line should be committed");
    let blank_rows = commit
        .lines
        .iter()
        .filter(|line| line.to_string().trim().is_empty())
        .count();

    // The commit boundary sits after the newline; keeping it would double the
    // vertical space every streamed line takes.
    assert_eq!(blank_rows, 0);
}

#[test]
fn thinking_body_is_indented_under_its_header() {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.show_welcome = false;
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::Thinking, "Thinking", "a thought"));

    let lines = pending_history_lines(&state, 40)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    let header = lines.iter().position(|line| line.contains("Thinking")).expect("header");
    let body = lines.iter().position(|line| line.contains("a thought")).expect("body");
    assert!(!lines[header].starts_with(' '));
    assert!(lines[body].starts_with("  "));
}

fn collapsed_state() -> AppState {
    let mut state = AppState::with_thinking_display(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
        ThinkingDisplay::Collapsed,
    );
    state.show_welcome = false;
    state
}

#[test]
fn collapsed_thinking_is_never_flushed_into_scrollback_while_streaming() {
    let mut state = collapsed_state();
    state.begin_turn("question");
    state.mark_transcript_committed();
    state.handle_agent_event(AgentEvent::Thinking("line one\nline two\nstill going".to_string()));

    let commit = streaming_history_commit(&state, 40);

    // Scrollback cannot be rewritten, so a body written there could never be
    // replaced by the summary — collapsed mode must hold it in the viewport.
    assert!(commit.is_none_or(|commit| commit.active_byte_count == 0));
}

#[test]
fn finished_thinking_collapses_to_a_single_summary_line() {
    let mut state = collapsed_state();
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::Thinking("one\ntwo\nthree".to_string()));
    state.handle_agent_event(AgentEvent::TextDelta("the answer".to_string()));
    state.finish_turn(1, Default::default());

    let lines = pending_history_lines(&state, 60)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    let summary = lines
        .iter()
        .find(|line| line.contains("Thinking"))
        .expect("the collapsed summary should be present");
    assert!(summary.contains("3 lines"), "got: {summary}");
    assert!(!lines.iter().any(|line| line.contains("two")), "body must be hidden");
    assert!(lines.iter().any(|line| line.contains("the answer")));
}

#[test]
fn unfinished_thinking_stays_expanded_so_the_stream_is_readable() {
    let mut state = collapsed_state();
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::Thinking("one\ntwo".to_string()));

    let lines = pending_history_lines(&state, 60)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    assert!(lines.iter().any(|line| line.contains("two")));
    assert!(!lines.iter().any(|line| line.contains("lines)")));
}

#[test]
fn live_thinking_is_capped_to_a_scrolling_window() {
    let mut state = collapsed_state();
    state.begin_turn("question");
    let body = (1..=20)
        .map(|index| format!("row{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    state.handle_agent_event(AgentEvent::Thinking(body));

    let lines = pending_history_lines(&state, 60)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    // A long chain of thought must not push the conversation off screen; the
    // window keeps the newest rows.
    assert!(lines.iter().any(|line| line.contains("row20")));
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("row1\"") || line.trim() == "row1")
    );
    let body_rows = lines.iter().filter(|line| line.contains("row")).count();
    assert!(body_rows <= 6, "expected at most 6 body rows, got {body_rows}");
}

#[test]
fn thinking_display_off_records_no_reasoning_at_all() {
    let mut state = AppState::with_thinking_display(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
        ThinkingDisplay::Off,
    );
    state.show_welcome = false;
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::Thinking("hidden reasoning".to_string()));
    state.handle_agent_event(AgentEvent::TextDelta("the answer".to_string()));

    let kinds: Vec<_> = state.transcript.iter().map(|entry| entry.kind).collect();
    assert_eq!(kinds, vec![EntryKind::User, EntryKind::Assistant]);
}

#[test]
fn full_mode_keeps_the_whole_reasoning_block_after_it_finishes() {
    let mut state = AppState::with_thinking_display(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
        ThinkingDisplay::Full,
    );
    state.show_welcome = false;
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::Thinking("one\ntwo\nthree".to_string()));
    state.finish_turn(1, Default::default());

    let lines = pending_history_lines(&state, 60)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    assert!(lines.iter().any(|line| line.contains("two")));
    assert!(!lines.iter().any(|line| line.contains("lines)")));
}
#[test]
fn collapsed_summary_sits_directly_above_the_answer() {
    let mut state = collapsed_state();
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::Thinking("one\ntwo\nthree".to_string()));
    state.handle_agent_event(AgentEvent::TextDelta("the answer".to_string()));

    let lines = pending_history_lines(&state, 60)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let summary = lines.iter().position(|l| l.contains("Thinking")).expect("summary");
    let answer = lines.iter().position(|l| l.contains("the answer")).expect("answer");

    // A one-line summary needs no separator; spending a row on it is the
    // difference between a compact transcript and a sparse one.
    assert_eq!(answer, summary + 1, "lines: {lines:?}");
}

#[test]
fn consecutive_blank_rows_are_never_rendered() {
    let mut state = collapsed_state();
    state.begin_turn("question");
    // Blank rows come from the entry separator, the model's own markdown and
    // the flush boundary; unchecked they stack into dead space.
    state.handle_agent_event(AgentEvent::TextDelta("first\n\n\n\nsecond\n\n\nthird".to_string()));

    let lines = pending_history_lines(&state, 60)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    assert!(!lines.first().is_some_and(|line| line.trim().is_empty()));
    assert!(
        !lines
            .windows(2)
            .any(|pair| pair[0].trim().is_empty() && pair[1].trim().is_empty()),
        "lines: {lines:?}"
    );
}

#[test]
fn a_flush_ending_in_a_collapsed_summary_adds_no_trailing_blank() {
    let mut state = collapsed_state();
    state.begin_turn("question");
    state.mark_transcript_committed();
    state.handle_agent_event(AgentEvent::Thinking("one\ntwo".to_string()));
    state.handle_agent_event(AgentEvent::TextDelta("first paragraph\n\nsecond".to_string()));

    let commit = streaming_history_commit(&state, 60).expect("the finished summary should be committed");
    let lines = commit.lines.iter().map(ToString::to_string).collect::<Vec<_>>();
    let summary = lines
        .iter()
        .position(|line| line.contains("Thinking"))
        .expect("summary should be in the flush");

    // The scrollback must match what the viewport showed; a stray blank here
    // reintroduces the gap the collapse was meant to remove.
    assert!(
        lines.get(summary + 1).is_none_or(|line| !line.trim().is_empty()),
        "lines: {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Task checklist panel
// ---------------------------------------------------------------------------

fn todo(content: &str, status: &str, active_form: Option<&str>) -> TodoSnapshot {
    TodoSnapshot {
        content: content.to_string(),
        status: status.to_string(),
        active_form: active_form.map(str::to_string),
    }
}

fn render_with_todos(todos: Vec<TodoSnapshot>, busy: bool) -> String {
    let mut state = AppState::new(
        "model".to_string(),
        "provider".to_string(),
        "/workspace".to_string(),
        true,
    );
    state.handle_agent_event(AgentEvent::TodoUpdated(todos));
    state.busy = busy;

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| render(frame, &state))
        .expect("render should succeed");
    terminal.backend().to_string()
}

#[test]
fn the_panel_shows_each_task_with_a_progress_count() {
    let rendered = render_with_todos(
        vec![
            todo("Read the plan", "completed", None),
            todo("Wire the store", "in_progress", Some("Wiring the store")),
            todo("Add tests", "pending", None),
        ],
        false,
    );

    assert!(rendered.contains("Tasks 1/3"), "got:\n{rendered}");
    assert!(rendered.contains("Read the plan"), "got:\n{rendered}");
    assert!(rendered.contains("Add tests"), "got:\n{rendered}");
}

#[test]
fn the_active_task_renders_its_present_continuous_form() {
    let rendered = render_with_todos(
        vec![todo("Wire the store", "in_progress", Some("Wiring the store"))],
        false,
    );

    assert!(rendered.contains("Wiring the store"), "got:\n{rendered}");
}

#[test]
fn a_pending_task_keeps_the_imperative_it_was_planned_in() {
    let rendered = render_with_todos(vec![todo("Wire the store", "pending", Some("Wiring the store"))], false);

    assert!(rendered.contains("Wire the store"), "got:\n{rendered}");
    assert!(!rendered.contains("Wiring the store"), "got:\n{rendered}");
}

#[test]
fn a_long_checklist_is_elided_rather_than_crowding_the_transcript() {
    let todos: Vec<_> = (0..10)
        .map(|index| todo(&format!("Task number {index}"), "pending", None))
        .collect();
    let rendered = render_with_todos(todos, false);

    assert!(rendered.contains("Task number 0"), "got:\n{rendered}");
    assert!(rendered.contains("4 more"), "got:\n{rendered}");
    assert!(!rendered.contains("Task number 9"), "got:\n{rendered}");
}

#[test]
fn no_checklist_means_no_panel_and_no_wasted_row() {
    let rendered = render_with_todos(Vec::new(), false);
    assert!(!rendered.contains("Tasks "), "got:\n{rendered}");
}

#[test]
fn the_footer_names_the_active_task_while_busy() {
    let rendered = render_with_todos(
        vec![todo(
            "Run the test suite",
            "in_progress",
            Some("Running the test suite"),
        )],
        true,
    );

    let footer = rendered
        .lines()
        .find(|line| line.contains("AgentrsCLI ·"))
        .expect("footer should render");
    assert!(footer.contains("Running the test suite"), "got: {footer}");
}

#[test]
fn the_footer_stays_anonymous_when_idle() {
    let rendered = render_with_todos(
        vec![todo(
            "Run the test suite",
            "in_progress",
            Some("Running the test suite"),
        )],
        false,
    );

    let footer = rendered
        .lines()
        .find(|line| line.contains("AgentrsCLI ·"))
        .expect("footer should render");
    assert!(footer.contains("ready"), "got: {footer}");
    assert!(!footer.contains("Running the test suite"), "got: {footer}");
}

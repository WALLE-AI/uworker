use agentrs_agent::commands::CommandSpec;
use agentrs_protocol::events::TodoSnapshot;
use agentrs_types::compact::{CompactMetadata, CompactTrigger};
use agentrs_types::message::{ContentBlock, Message, Role};

use super::AppState;
use crate::event::AgentEvent;
use crate::transcript::{EntryKind, ToolStepStatus, TranscriptEntry};

fn state() -> AppState {
    AppState::new(
        "test-model".to_string(),
        "test-provider".to_string(),
        ".".to_string(),
        true,
    )
}

#[test]
fn resumed_history_is_rendered_by_role() {
    let mut state = state();
    state.set_history(&[
        Message::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        ),
        Message::new(Role::Assistant, vec![ContentBlock::Text { text: "hi".to_string() }]),
    ]);
    assert_eq!(state.transcript.len(), 2);
    assert_eq!(state.transcript[0].kind, EntryKind::User);
    assert_eq!(state.transcript[1].kind, EntryKind::Assistant);
    assert!(state.transcript[0].label.is_empty());
    assert!(state.transcript[1].label.is_empty());
}

#[test]
fn resumed_history_hides_compact_context_messages() {
    let metadata = CompactMetadata {
        trigger: CompactTrigger::Manual,
        pre_compact_tokens: 42_000,
        messages_summarized: 12,
    };
    let mut state = state();
    state.set_history(&[
        Message::new(
            Role::User,
            vec![ContentBlock::Text {
                text: format!(
                    "[Conversation compacted]\n{}",
                    serde_json::to_string(&metadata).unwrap()
                ),
            }],
        ),
        Message::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "This session is being continued from a previous conversation.\n\nSummary: internal summary"
                    .to_string(),
            }],
        ),
        Message::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "visible question".to_string(),
            }],
        ),
        Message::new(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "visible answer".to_string(),
            }],
        ),
    ]);

    assert_eq!(state.transcript.len(), 2);
    assert_eq!(state.transcript[0].kind, EntryKind::User);
    assert_eq!(state.transcript[0].text, "visible question");
    assert_eq!(state.transcript[1].kind, EntryKind::Assistant);
    assert_eq!(state.transcript[1].text, "visible answer");
}

#[test]
fn protocol_result_suppresses_duplicate_sink_result() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::ProtocolToolResult {
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
        is_error: false,
        content: "done".to_string(),
    });
    state.handle_agent_event(AgentEvent::ToolResult {
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
        is_error: false,
        content: "done".to_string(),
    });
    assert_eq!(state.transcript.len(), 1);
}

#[test]
fn tool_lifecycle_updates_one_countable_step() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::ToolCall {
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
        input: "{\"path\":\"file.rs\"}".to_string(),
    });
    state.handle_agent_event(AgentEvent::ToolRunning {
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
    });
    state.handle_agent_event(AgentEvent::ProtocolToolResult {
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
        is_error: false,
        content: "file contents".to_string(),
    });

    assert_eq!(state.transcript.len(), 1);
    assert_eq!(state.transcript[0].label, "Read");
    assert_eq!(state.transcript[0].text, "file contents");
    assert_eq!(state.transcript[0].tool_status, Some(ToolStepStatus::Success));
}

#[test]
fn resumed_tool_use_and_result_merge_into_one_step() {
    let mut state = state();
    state.set_history(&[
        Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "call-1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({"path": "file.rs"}),
                extra: None,
            }],
        ),
        Message::new(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "file contents".to_string(),
                is_error: false,
            }],
        ),
    ]);

    assert_eq!(state.transcript.len(), 1);
    assert_eq!(state.transcript[0].label, "Read");
    assert_eq!(state.transcript[0].text, "file contents");
    assert_eq!(state.transcript[0].tool_status, Some(ToolStepStatus::Success));
    assert!(!state.show_welcome);
}

#[test]
fn resumed_history_keeps_conversation_text_across_tool_rounds() {
    let mut state = state();
    state.set_history(&[
        Message::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "first question".to_string(),
            }],
        ),
        Message::new(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "first answer".to_string(),
            }],
        ),
        Message::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "second question".to_string(),
            }],
        ),
        Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "call-1".to_string(),
                name: "Read".to_string(),
                input: serde_json::json!({"path": "file.rs"}),
                extra: None,
            }],
        ),
        Message::new(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "file contents".to_string(),
                is_error: false,
            }],
        ),
        Message::new(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "complete final answer".to_string(),
            }],
        ),
    ]);

    let conversation = state
        .transcript
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::User | EntryKind::Assistant))
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        conversation,
        [
            "first question",
            "first answer",
            "second question",
            "complete final answer"
        ]
    );
    assert!(state.transcript.iter().any(|entry| {
        entry.kind == EntryKind::Tool
            && entry.label == "Read"
            && entry.text == "file contents"
            && entry.tool_status == Some(ToolStepStatus::Success)
    }));
}

#[test]
fn streaming_deltas_append_to_one_assistant_entry() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::StreamStart);
    state.handle_agent_event(AgentEvent::TextDelta("hello ".to_string()));
    state.handle_agent_event(AgentEvent::TextDelta("world".to_string()));
    assert_eq!(state.transcript.len(), 1);
    assert_eq!(state.transcript[0].text, "hello world");
}

#[test]
fn history_replay_resets_stream_offsets_and_keeps_active_content_pending() {
    let mut state = state();
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::TextDelta("first\n\nsecond".to_string()));
    state.commit_streaming_prefix(1, "first\n\n".len());
    assert_eq!(state.transcript[1].visible_text(), "second");

    let replayable_count = state.prepare_history_replay();

    assert_eq!(replayable_count, 1);
    assert_eq!(state.committed_transcript, 0);
    assert_eq!(state.transcript[1].visible_text(), "first\n\nsecond");
    state.mark_transcript_prefix_committed(replayable_count);
    assert_eq!(state.pending_transcript().len(), 1);
}

#[test]
fn command_catalog_merges_agent_and_tui_commands() {
    let mut state = state();
    state.set_commands(vec![CommandSpec {
        name: "compact".to_string(),
        aliases: Vec::new(),
        description: "Compress context".to_string(),
    }]);

    let names = state
        .popup
        .commands()
        .iter()
        .map(|command| command.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"compact"));
    assert!(names.contains(&"status"));
    assert!(names.contains(&"permissions"));
    assert!(names.contains(&"resume"));
}

#[test]
fn reset_session_clears_transient_state() {
    let mut state = state();
    state.composer.insert_text("draft");
    state.begin_initialization();
    state.busy = true;
    state.turns = 3;
    state
        .transcript
        .push(TranscriptEntry::new(EntryKind::Info, "Info", "old session"));

    state.reset_session(
        "next-model".to_string(),
        "next-provider".to_string(),
        Some("next-session".to_string()),
        &[],
    );

    assert_eq!(state.model, "next-model");
    assert_eq!(state.session_id.as_deref(), Some("next-session"));
    assert!(state.transcript.is_empty());
    assert!(state.composer.is_empty());
    assert!(!state.initializing);
    assert!(!state.busy);
    assert_eq!(state.turns, 0);
}

#[test]
fn text_after_a_tool_call_starts_a_new_block_below_the_tool() {
    let mut state = state();
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::TextDelta("let me look".to_string()));
    state.handle_agent_event(AgentEvent::ToolCall {
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
        input: "{}".to_string(),
    });
    state.handle_agent_event(AgentEvent::ToolResult {
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
        is_error: false,
        content: "file body".to_string(),
    });
    state.handle_agent_event(AgentEvent::TextDelta("the answer".to_string()));

    let kinds: Vec<_> = state.transcript.iter().map(|entry| entry.kind).collect();
    assert_eq!(
        kinds,
        vec![
            EntryKind::User,
            EntryKind::Assistant,
            EntryKind::Tool,
            EntryKind::Assistant
        ]
    );
    // The answer must not be folded back into the pre-tool block, which would
    // render it above the tool that produced it.
    assert_eq!(state.transcript[1].text, "let me look");
    assert_eq!(state.transcript[3].text, "the answer");
}

#[test]
fn thinking_after_a_tool_call_also_starts_a_new_block() {
    let mut state = state();
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::Thinking("first".to_string()));
    state.handle_agent_event(AgentEvent::ToolCall {
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
        input: "{}".to_string(),
    });
    state.handle_agent_event(AgentEvent::Thinking("second".to_string()));

    let kinds: Vec<_> = state.transcript.iter().map(|entry| entry.kind).collect();
    assert_eq!(
        kinds,
        vec![
            EntryKind::User,
            EntryKind::Thinking,
            EntryKind::Tool,
            EntryKind::Thinking
        ]
    );
}

#[test]
fn thinking_finishes_when_the_answer_starts_without_a_tool_call() {
    let mut state = state();
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::Thinking("reasoning".to_string()));
    state.handle_agent_event(AgentEvent::TextDelta("answer".to_string()));

    // Reasoning normally runs straight into the answer. If that boundary does
    // not finish the block it can never collapse.
    assert!(state.transcript[1].is_finished());
    assert!(!state.transcript[2].is_finished());
}

#[test]
fn a_second_reasoning_block_finishes_the_answer_before_it() {
    let mut state = state();
    state.begin_turn("question");
    state.handle_agent_event(AgentEvent::TextDelta("partial".to_string()));
    state.handle_agent_event(AgentEvent::Thinking("more reasoning".to_string()));

    assert!(state.transcript[1].is_finished());
    assert!(!state.transcript[2].is_finished());
}

// ---------------------------------------------------------------------------
// Task checklist
// ---------------------------------------------------------------------------

fn todo(content: &str, status: &str, active_form: Option<&str>) -> TodoSnapshot {
    TodoSnapshot {
        content: content.to_string(),
        status: status.to_string(),
        active_form: active_form.map(str::to_string),
    }
}

#[test]
fn a_checklist_update_replaces_the_previous_list() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::TodoUpdated(vec![
        todo("A", "pending", None),
        todo("B", "pending", None),
    ]));
    state.handle_agent_event(AgentEvent::TodoUpdated(vec![todo("C", "completed", None)]));

    assert_eq!(state.todos.len(), 1, "the tool replaces, so the view replaces");
    assert_eq!(state.todos[0].content, "C");
}

#[test]
fn an_empty_update_clears_the_checklist() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::TodoUpdated(vec![todo("A", "pending", None)]));
    state.handle_agent_event(AgentEvent::TodoUpdated(Vec::new()));

    assert!(state.todos.is_empty());
}

#[test]
fn the_status_line_names_the_single_active_task() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::TodoUpdated(vec![
        todo("Read the plan", "completed", None),
        todo("Run the test suite", "in_progress", Some("Running the test suite")),
        todo("Write docs", "pending", None),
    ]));

    assert_eq!(state.active_todo_label(), Some("Running the test suite"));
}

#[test]
fn the_status_line_falls_back_to_content_without_an_active_form() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::TodoUpdated(vec![todo(
        "Run the test suite",
        "in_progress",
        None,
    )]));

    assert_eq!(state.active_todo_label(), Some("Run the test suite"));
}

#[test]
fn the_status_line_stays_anonymous_when_several_tasks_are_active() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::TodoUpdated(vec![
        todo("A", "in_progress", Some("Doing A")),
        todo("B", "in_progress", Some("Doing B")),
    ]));

    assert_eq!(
        state.active_todo_label(),
        None,
        "picking one of several would be arbitrary"
    );
}

#[test]
fn the_status_line_is_anonymous_with_no_active_task() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::TodoUpdated(vec![todo("A", "pending", None)]));
    assert_eq!(state.active_todo_label(), None);
}

#[test]
fn the_summary_lists_every_entry_with_a_progress_count() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::TodoUpdated(vec![
        todo("Read the plan", "completed", None),
        todo("Run tests", "in_progress", Some("Running tests")),
        todo("Write docs", "pending", None),
    ]));

    let summary = state.todo_summary();
    assert!(summary.starts_with("1/3 completed"), "got: {summary}");
    assert!(summary.contains("✓ Read the plan"), "got: {summary}");
    assert!(summary.contains("▸ Run tests"), "got: {summary}");
    assert!(summary.contains("○ Write docs"), "got: {summary}");
}

#[test]
fn the_summary_says_so_when_nothing_is_tracked() {
    assert_eq!(state().todo_summary(), "No tasks tracked in this session");
}

#[test]
fn a_new_session_starts_without_a_checklist() {
    let mut state = state();
    state.handle_agent_event(AgentEvent::TodoUpdated(vec![todo("A", "in_progress", None)]));
    state.reset_session("m".to_string(), "p".to_string(), None, &[]);

    assert!(state.todos.is_empty(), "a fresh session must not inherit the old plan");
}

use agentrs_types::message::{ContentBlock, Message, Role};
use serde_json::json;

use super::TodoStore;
use crate::todo::{TodoItem, TodoStatus};

fn item(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: content.to_string(),
        status,
        active_form: None,
    }
}

fn todo_write_call(id: &str, contents: &[(&str, &str)]) -> Message {
    let todos: Vec<_> = contents
        .iter()
        .map(|(content, status)| json!({ "content": content, "status": status }))
        .collect();
    Message::now(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: id.to_string(),
            name: "TodoWrite".to_string(),
            input: json!({ "todos": todos }),
            extra: None,
        }],
    )
}

fn tool_result(id: &str, is_error: bool) -> Message {
    Message::now(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: id.to_string(),
            content: "ok".to_string(),
            is_error,
        }],
    )
}

#[test]
fn replace_then_snapshot_round_trips() {
    let store = TodoStore::new();
    assert!(store.is_empty());

    store.replace(vec![item("A", TodoStatus::InProgress)]);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].content, "A");
    assert!(!store.is_empty());
}

#[test]
fn replace_discards_the_previous_list() {
    let store = TodoStore::new();
    store.replace(vec![item("A", TodoStatus::Pending), item("B", TodoStatus::Pending)]);
    store.replace(vec![item("C", TodoStatus::Completed)]);

    let snapshot = store.snapshot();
    assert_eq!(snapshot.len(), 1, "replacement is whole-list, not a merge");
    assert_eq!(snapshot[0].content, "C");
}

#[test]
fn clear_empties_the_list() {
    let store = TodoStore::new();
    store.replace(vec![item("A", TodoStatus::Pending)]);
    store.clear();
    assert!(store.is_empty());
}

#[test]
fn separate_stores_do_not_share_state() {
    // Stands in for a spawned sub-agent, which builds its own registry and so
    // its own store.
    let parent = TodoStore::new();
    let child = TodoStore::new();

    parent.replace(vec![item("parent task", TodoStatus::InProgress)]);
    child.replace(vec![item("child task", TodoStatus::Pending)]);

    assert_eq!(parent.snapshot()[0].content, "parent task");
    assert_eq!(child.snapshot()[0].content, "child task");
}

#[test]
fn rehydrate_replays_the_last_successful_write() {
    let messages = vec![
        todo_write_call("call-1", &[("First plan", "pending")]),
        tool_result("call-1", false),
        todo_write_call("call-2", &[("Second plan", "in_progress"), ("Third", "pending")]),
        tool_result("call-2", false),
    ];

    let store = TodoStore::new();
    store.rehydrate(&messages, &[]);

    let snapshot = store.snapshot();
    assert_eq!(snapshot.len(), 2, "latest write wins");
    assert_eq!(snapshot[0].content, "Second plan");
    assert_eq!(snapshot[0].status, TodoStatus::InProgress);
}

#[test]
fn rehydrate_skips_a_rejected_write() {
    let messages = vec![
        todo_write_call("call-1", &[("Accepted plan", "pending")]),
        tool_result("call-1", false),
        // Rejected by validation, so it never reached the store.
        todo_write_call("call-2", &[("Never applied", "pending")]),
        tool_result("call-2", true),
    ];

    let store = TodoStore::new();
    store.rehydrate(&messages, &[]);

    let snapshot = store.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].content, "Accepted plan");
}

#[test]
fn rehydrate_forked_history_yields_the_point_in_time_list() {
    let full = [
        todo_write_call("call-1", &[("Early plan", "pending")]),
        tool_result("call-1", false),
        todo_write_call("call-2", &[("Later plan", "pending")]),
        tool_result("call-2", false),
    ];
    // A fork truncates history; replay must follow it without extra bookkeeping.
    let forked = &full[..2];

    let store = TodoStore::new();
    store.rehydrate(forked, &[]);
    assert_eq!(store.snapshot()[0].content, "Early plan");
}

#[test]
fn rehydrate_falls_back_to_the_persisted_snapshot_when_history_is_gone() {
    // What a full autocompact leaves behind: no ToolUse blocks at all.
    let messages = vec![
        Message::now(
            Role::User,
            vec![ContentBlock::Text {
                text: "[compact boundary]".to_string(),
            }],
        ),
        Message::now(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "summary".to_string(),
            }],
        ),
    ];
    let persisted = vec![item("Survived compaction", TodoStatus::InProgress)];

    let store = TodoStore::new();
    store.rehydrate(&messages, &persisted);

    assert_eq!(store.snapshot()[0].content, "Survived compaction");
}

#[test]
fn replay_beats_the_persisted_snapshot() {
    let messages = vec![
        todo_write_call("call-1", &[("From history", "pending")]),
        tool_result("call-1", false),
    ];
    let persisted = vec![item("From snapshot", TodoStatus::Pending)];

    let store = TodoStore::new();
    store.rehydrate(&messages, &persisted);

    assert_eq!(
        store.snapshot()[0].content,
        "From history",
        "history is authoritative whenever it still has the call"
    );
}

#[test]
fn rehydrate_survives_microcompacted_results() {
    // Microcompact blanks ToolResult content but leaves is_error and the
    // originating ToolUse alone, so replay must still work.
    let messages = vec![
        todo_write_call("call-1", &[("Still here", "pending")]),
        Message::now(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: "[tool result cleared]".to_string(),
                is_error: false,
            }],
        ),
    ];

    let store = TodoStore::new();
    store.rehydrate(&messages, &[]);
    assert_eq!(store.snapshot()[0].content, "Still here");
}

#[test]
fn rehydrate_ignores_other_tools() {
    let messages = vec![
        Message::now(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "call-1".to_string(),
                name: "Read".to_string(),
                input: json!({ "todos": [{ "content": "not mine", "status": "pending" }] }),
                extra: None,
            }],
        ),
        tool_result("call-1", false),
    ];

    let store = TodoStore::new();
    store.rehydrate(&messages, &[]);
    assert!(store.is_empty(), "only TodoWrite calls carry the checklist");
}

#[test]
fn rehydrate_clears_when_neither_source_has_anything() {
    let store = TodoStore::new();
    store.replace(vec![item("stale", TodoStatus::Pending)]);
    store.rehydrate(&[], &[]);
    assert!(store.is_empty());
}

#[test]
fn rehydrate_tolerates_an_unreadable_payload() {
    let messages = vec![
        todo_write_call("call-1", &[("Good", "pending")]),
        tool_result("call-1", false),
        Message::now(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "call-2".to_string(),
                name: "TodoWrite".to_string(),
                input: json!({ "todos": "not an array" }),
                extra: None,
            }],
        ),
        tool_result("call-2", false),
    ];

    let store = TodoStore::new();
    store.rehydrate(&messages, &[]);
    assert!(
        store.is_empty(),
        "an unreadable latest call falls back rather than reaching further back"
    );
}

#[test]
fn rehydrate_accepts_parallel_lists_written_under_a_different_policy() {
    let messages = vec![
        todo_write_call("call-1", &[("A", "in_progress"), ("B", "in_progress")]),
        tool_result("call-1", false),
    ];

    let store = TodoStore::new();
    store.rehydrate(&messages, &[]);
    assert_eq!(
        store.snapshot().len(),
        2,
        "history must not be re-judged against the current parallel policy"
    );
}

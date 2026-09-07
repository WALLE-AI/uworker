use std::sync::Arc;

use agentrs_tools::task::{TaskDraft, TaskPatch, TaskStore};
use agentrs_tools::todo::{TodoItem, TodoStatus, TodoStore};
use agentrs_types::message::ContentBlock;
use serde_json::json;
use tempfile::TempDir;

use super::{PlanSource, TodoRuntime};

const REMINDER_TURNS: usize = 10;

fn item(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: content.to_string(),
        status,
        active_form: None,
    }
}

fn runtime(reminder_turns: usize) -> (TodoRuntime, Arc<TodoStore>) {
    let store = Arc::new(TodoStore::new());
    (TodoRuntime::new(PlanSource::List(Arc::clone(&store)), reminder_turns), store)
}

fn todo_write_call() -> Vec<ContentBlock> {
    vec![ContentBlock::ToolUse {
        id: "call-1".to_string(),
        name: "TodoWrite".to_string(),
        input: json!({ "todos": [] }),
        extra: None,
    }]
}

fn other_call() -> Vec<ContentBlock> {
    vec![ContentBlock::ToolUse {
        id: "call-1".to_string(),
        name: "Read".to_string(),
        input: json!({ "file_path": "/tmp/x" }),
        extra: None,
    }]
}

#[test]
fn no_reminder_before_the_threshold() {
    let (mut runtime, _store) = runtime(REMINDER_TURNS);
    for _ in 0..REMINDER_TURNS - 1 {
        runtime.record_turn(&other_call());
        assert!(runtime.take_reminder().is_none());
    }
}

#[test]
fn reminds_once_the_threshold_is_reached() {
    let (mut runtime, _store) = runtime(REMINDER_TURNS);
    for _ in 0..REMINDER_TURNS {
        runtime.record_turn(&other_call());
    }

    let reminder = runtime.take_reminder().expect("reminder is due");
    assert!(reminder.starts_with("<system-reminder>"));
    assert!(reminder.ends_with("</system-reminder>"));
    assert!(reminder.contains("Never mention this reminder to the user"));
}

#[test]
fn a_reminder_is_not_repeated_on_the_very_next_turn() {
    let (mut runtime, _store) = runtime(REMINDER_TURNS);
    for _ in 0..REMINDER_TURNS {
        runtime.record_turn(&other_call());
    }
    assert!(runtime.take_reminder().is_some());

    runtime.record_turn(&other_call());
    assert!(
        runtime.take_reminder().is_none(),
        "spacing counter must gate the next reminder"
    );
}

#[test]
fn reminders_resume_after_the_spacing_interval() {
    let (mut runtime, _store) = runtime(REMINDER_TURNS);
    for _ in 0..REMINDER_TURNS {
        runtime.record_turn(&other_call());
    }
    assert!(runtime.take_reminder().is_some());

    for _ in 0..REMINDER_TURNS {
        runtime.record_turn(&other_call());
    }
    assert!(runtime.take_reminder().is_some(), "a second reminder is due");
}

#[test]
fn calling_todo_write_resets_the_countdown() {
    let (mut runtime, _store) = runtime(REMINDER_TURNS);
    for _ in 0..REMINDER_TURNS - 1 {
        runtime.record_turn(&other_call());
    }
    runtime.record_turn(&todo_write_call());

    for _ in 0..REMINDER_TURNS - 1 {
        runtime.record_turn(&other_call());
        assert!(runtime.take_reminder().is_none(), "countdown restarted from the write");
    }
}

#[test]
fn zero_turns_disables_reminders_entirely() {
    let (mut runtime, _store) = runtime(0);
    for _ in 0..100 {
        runtime.record_turn(&other_call());
        assert!(runtime.take_reminder().is_none());
    }
}

#[test]
fn reminder_carries_the_current_list() {
    let (mut runtime, store) = runtime(REMINDER_TURNS);
    store.replace(vec![
        item("Wire the store", TodoStatus::InProgress),
        item("Add tests", TodoStatus::Pending),
    ]);
    for _ in 0..REMINDER_TURNS {
        runtime.record_turn(&other_call());
    }

    let reminder = runtime.take_reminder().expect("due");
    assert!(reminder.contains("1. [in_progress] Wire the store"));
    assert!(reminder.contains("2. [pending] Add tests"));
}

#[test]
fn reminder_says_so_when_the_list_is_empty() {
    let (mut runtime, _store) = runtime(REMINDER_TURNS);
    for _ in 0..REMINDER_TURNS {
        runtime.record_turn(&other_call());
    }

    let reminder = runtime.take_reminder().expect("due");
    assert!(
        reminder.contains("currently empty"),
        "an empty list is exactly when the nudge matters most"
    );
}

#[test]
fn a_finished_list_is_retired_when_the_next_turn_opens() {
    let (runtime, store) = runtime(REMINDER_TURNS);
    store.replace(vec![item("A", TodoStatus::Completed), item("B", TodoStatus::Completed)]);

    runtime.on_user_turn_start();
    assert!(store.is_empty(), "a fully completed list has served its purpose");
}

#[test]
fn an_unfinished_list_survives_the_next_turn() {
    let (runtime, store) = runtime(REMINDER_TURNS);
    store.replace(vec![
        item("Done", TodoStatus::Completed),
        item("Still going", TodoStatus::InProgress),
    ]);

    runtime.on_user_turn_start();
    assert_eq!(
        store.snapshot().len(),
        2,
        "a mid-plan course correction must not lose outstanding work"
    );
}

#[test]
fn retiring_an_already_empty_list_is_a_no_op() {
    let (runtime, store) = runtime(REMINDER_TURNS);
    runtime.on_user_turn_start();
    assert!(store.is_empty());
}

#[test]
fn snapshot_reflects_the_shared_store() {
    let (runtime, store) = runtime(REMINDER_TURNS);
    store.replace(vec![item("A", TodoStatus::Pending)]);
    assert_eq!(runtime.checklist().len(), 1);
}

// ---------------------------------------------------------------------------
// Graph mode
// ---------------------------------------------------------------------------

fn graph_runtime(reminder_turns: usize) -> (TodoRuntime, Arc<TaskStore>, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(TaskStore::new(dir.path().join("tasks")));
    (
        TodoRuntime::new(PlanSource::Graph(Arc::clone(&store)), reminder_turns),
        store,
        dir,
    )
}

fn task_draft(subject: &str) -> TaskDraft {
    TaskDraft {
        subject: subject.to_string(),
        description: String::new(),
        active_form: None,
        owner: None,
        blocked_by: Vec::new(),
    }
}

fn task_call(name: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::ToolUse {
        id: "call-1".to_string(),
        name: name.to_string(),
        input: json!({}),
        extra: None,
    }]
}

#[test]
fn the_graph_publishes_tasks_with_their_ids() {
    let (mut runtime, store, _dir) = graph_runtime(REMINDER_TURNS);
    store
        .create(vec![task_draft("Design the API"), task_draft("Build it")])
        .expect("create");

    let published = runtime.take_changed_snapshot().expect("the graph changed");
    assert_eq!(published.len(), 2);
    assert_eq!(
        published[0].content, "#1 Design the API",
        "the id has to survive so a blocked-by message can be followed"
    );
    assert_eq!(published[0].status, "pending");
}

#[test]
fn an_unchanged_graph_does_not_republish() {
    let (mut runtime, store, _dir) = graph_runtime(REMINDER_TURNS);
    store.create(vec![task_draft("Only task")]).expect("create");

    assert!(runtime.take_changed_snapshot().is_some());
    assert!(runtime.take_changed_snapshot().is_none());
}

#[test]
fn any_task_tool_resets_the_graph_countdown() {
    for tool in ["TaskCreate", "TaskList", "TaskGet", "TaskUpdate"] {
        let (mut runtime, _store, _dir) = graph_runtime(REMINDER_TURNS);
        for _ in 0..REMINDER_TURNS - 1 {
            runtime.record_turn(&other_call());
        }
        runtime.record_turn(&task_call(tool));

        for _ in 0..REMINDER_TURNS - 1 {
            runtime.record_turn(&other_call());
            assert!(
                runtime.take_reminder().is_none(),
                "{tool} should have reset the countdown"
            );
        }
    }
}

#[test]
fn todo_write_does_not_count_as_tracking_in_graph_mode() {
    let (mut runtime, _store, _dir) = graph_runtime(REMINDER_TURNS);
    for _ in 0..REMINDER_TURNS {
        runtime.record_turn(&todo_write_call());
    }

    assert!(
        runtime.take_reminder().is_some(),
        "TodoWrite is not registered in graph mode, so it cannot count as progress"
    );
}

#[test]
fn the_graph_reminder_names_the_task_tools() {
    let (mut runtime, _store, _dir) = graph_runtime(REMINDER_TURNS);
    for _ in 0..REMINDER_TURNS {
        runtime.record_turn(&other_call());
    }

    let reminder = runtime.take_reminder().expect("due");
    assert!(reminder.contains("task tools"), "got: {reminder}");
    assert!(!reminder.contains("TodoWrite"), "got: {reminder}");
}

#[test]
fn graph_tasks_are_never_retired_behind_the_models_back() {
    let (runtime, store, _dir) = graph_runtime(REMINDER_TURNS);
    store.create(vec![task_draft("Done")]).expect("create");
    store
        .update(
            "1",
            TaskPatch {
                status: Some(TodoStatus::Completed),
                ..TaskPatch::default()
            },
        )
        .expect("complete");

    runtime.on_user_turn_start();

    assert_eq!(
        store.list().expect("read").len(),
        1,
        "tasks are addressable by id; dropping one would strand every dependency naming it"
    );
}

#[test]
fn the_session_snapshot_stays_empty_in_graph_mode() {
    let (runtime, store, _dir) = graph_runtime(REMINDER_TURNS);
    store.create(vec![task_draft("On disk")]).expect("create");

    assert!(
        runtime.checklist().is_empty(),
        "the graph is already durable on disk; mirroring it into the session file would be a second source of truth"
    );
}

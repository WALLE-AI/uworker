use super::{RawTodoItem, TodoCounts, TodoError, TodoStatus, normalize};

fn raw(content: &str, status: &str) -> RawTodoItem {
    RawTodoItem {
        content: Some(content.to_string()),
        status: Some(status.to_string()),
        active_form: None,
    }
}

fn raw_with_form(content: &str, status: &str, form: &str) -> RawTodoItem {
    RawTodoItem {
        content: Some(content.to_string()),
        status: Some(status.to_string()),
        active_form: Some(form.to_string()),
    }
}

#[test]
fn normalizes_and_preserves_order() {
    let todos = normalize(
        vec![
            raw("  Read the config  ", "completed"),
            raw("Wire the store", "in_progress"),
            raw("Add tests", "pending"),
        ],
        false,
    )
    .expect("valid list");

    assert_eq!(todos.len(), 3);
    assert_eq!(todos[0].content, "Read the config", "content should be trimmed");
    assert_eq!(todos[0].status, TodoStatus::Completed);
    assert_eq!(todos[1].content, "Wire the store", "input order must be preserved");
    assert_eq!(todos[2].status, TodoStatus::Pending);
}

#[test]
fn rejects_empty_content() {
    let err = normalize(vec![raw("   ", "pending")], false).unwrap_err();
    assert_eq!(err, TodoError::EmptyContent { index: 0 });
}

#[test]
fn rejects_empty_content_reports_its_index() {
    let err = normalize(vec![raw("First", "pending"), raw("", "pending")], false).unwrap_err();
    assert_eq!(err, TodoError::EmptyContent { index: 1 });
}

#[test]
fn rejects_duplicate_content_after_trimming() {
    let err = normalize(vec![raw("Run tests", "pending"), raw("  Run tests ", "pending")], false).unwrap_err();
    assert_eq!(
        err,
        TodoError::DuplicateContent {
            content: "Run tests".to_string()
        }
    );
}

#[test]
fn rejects_multiple_in_progress_when_parallel_disallowed() {
    let err = normalize(vec![raw("A", "in_progress"), raw("B", "in_progress")], false).unwrap_err();
    assert_eq!(err, TodoError::TooManyInProgress { count: 2 });
}

#[test]
fn allows_multiple_in_progress_when_parallel_allowed() {
    let todos = normalize(vec![raw("A", "in_progress"), raw("B", "in_progress")], true).expect("parallel allowed");
    assert_eq!(todos.len(), 2);
}

#[test]
fn single_in_progress_is_always_allowed() {
    let todos = normalize(vec![raw("A", "in_progress"), raw("B", "pending")], false).expect("one active is fine");
    assert_eq!(todos[0].status, TodoStatus::InProgress);
}

#[test]
fn rejects_unknown_status() {
    let err = normalize(vec![raw("A", "blocked")], false).unwrap_err();
    assert_eq!(
        err,
        TodoError::UnknownStatus {
            status: "blocked".to_string()
        }
    );
}

#[test]
fn rejects_missing_fields() {
    let missing_content = RawTodoItem {
        content: None,
        status: Some("pending".to_string()),
        active_form: None,
    };
    assert_eq!(
        normalize(vec![missing_content], false).unwrap_err(),
        TodoError::NotAString { field: "content" }
    );

    let missing_status = RawTodoItem {
        content: Some("A".to_string()),
        status: None,
        active_form: None,
    };
    assert_eq!(
        normalize(vec![missing_status], false).unwrap_err(),
        TodoError::NotAString { field: "status" }
    );
}

#[test]
fn blank_active_form_folds_to_none() {
    let todos = normalize(vec![raw_with_form("A", "in_progress", "   ")], false).expect("valid");
    assert!(
        todos[0].active_form.is_none(),
        "whitespace-only activeForm should fold to None"
    );
    assert_eq!(todos[0].content, "A", "content remains the display fallback");
}

#[test]
fn active_form_is_trimmed_and_kept() {
    let todos = normalize(
        vec![raw_with_form("Run tests", "in_progress", "  Running tests  ")],
        false,
    )
    .expect("valid");
    assert_eq!(todos[0].active_form.as_deref(), Some("Running tests"));
}

#[test]
fn empty_list_is_valid() {
    let todos = normalize(Vec::new(), false).expect("clearing the list is legal");
    assert!(todos.is_empty());
}

#[test]
fn counts_each_status() {
    let todos = normalize(
        vec![
            raw("A", "pending"),
            raw("B", "pending"),
            raw("C", "in_progress"),
            raw("D", "completed"),
        ],
        false,
    )
    .expect("valid");

    let counts = TodoCounts::of(&todos);
    assert_eq!(counts.pending, 2);
    assert_eq!(counts.in_progress, 1);
    assert_eq!(counts.completed, 1);
}

#[test]
fn status_wire_form_round_trips() {
    for status in [TodoStatus::Pending, TodoStatus::InProgress, TodoStatus::Completed] {
        let json = serde_json::to_string(&status).expect("serialize");
        assert_eq!(json, format!("\"{}\"", status.as_str()));
        let back: TodoStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, status);
    }
}

#[test]
fn item_serialization_omits_absent_active_form() {
    let todos = normalize(vec![raw("A", "pending")], false).expect("valid");
    let json = serde_json::to_string(&todos[0]).expect("serialize");
    assert!(
        !json.contains("active_form"),
        "absent activeForm must not be serialized: {json}"
    );
}

#[test]
fn snapshots_carry_the_wire_status_and_optional_active_form() {
    let todos = normalize(
        vec![
            raw_with_form("Run tests", "in_progress", "Running tests"),
            raw("Write docs", "pending"),
        ],
        false,
    )
    .expect("valid");

    let snapshots = super::to_snapshots(&todos);
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].content, "Run tests");
    assert_eq!(snapshots[0].status, "in_progress");
    assert_eq!(snapshots[0].active_form.as_deref(), Some("Running tests"));
    assert_eq!(snapshots[1].status, "pending");
    assert!(snapshots[1].active_form.is_none());
}

#[test]
fn snapshotting_an_empty_list_yields_an_empty_list() {
    assert!(super::to_snapshots(&[]).is_empty());
}

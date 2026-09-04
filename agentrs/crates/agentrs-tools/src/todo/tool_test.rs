use std::sync::Arc;

use agentrs_protocol::events::ToolCategory;
use serde_json::json;

use super::{TODO_WRITE_TOOL_NAME, TodoWriteTool};
use crate::Tool;
use crate::todo::{TodoStatus, TodoStore};

fn tool(allow_parallel: bool) -> (TodoWriteTool, Arc<TodoStore>) {
    let store = Arc::new(TodoStore::new());
    (TodoWriteTool::new(Arc::clone(&store), allow_parallel), store)
}

#[tokio::test]
async fn writes_the_list_and_reports_counts() {
    let (tool, store) = tool(false);
    let result = tool
        .execute(json!({
            "todos": [
                { "content": "Read the plan", "status": "completed" },
                { "content": "Wire the store", "status": "in_progress", "activeForm": "Wiring the store" },
                { "content": "Add tests", "status": "pending" }
            ]
        }))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(
        result.content.contains("1 pending, 1 in progress, 1 completed"),
        "receipt should carry counts: {}",
        result.content
    );

    let snapshot = store.snapshot();
    assert_eq!(snapshot.len(), 3);
    assert_eq!(snapshot[1].status, TodoStatus::InProgress);
    assert_eq!(snapshot[1].active_form.as_deref(), Some("Wiring the store"));
}

#[tokio::test]
async fn receipt_does_not_echo_the_list() {
    let (tool, _store) = tool(false);
    let result = tool
        .execute(json!({ "todos": [{ "content": "Some private task text", "status": "pending" }] }))
        .await;

    assert!(
        !result.content.contains("Some private task text"),
        "the list is already in the call arguments; echoing it wastes context"
    );
}

#[tokio::test]
async fn replaces_rather_than_merges() {
    let (tool, store) = tool(false);
    tool.execute(json!({ "todos": [{ "content": "A", "status": "pending" }] }))
        .await;
    tool.execute(json!({ "todos": [{ "content": "B", "status": "pending" }] }))
        .await;

    let snapshot = store.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].content, "B");
}

#[tokio::test]
async fn an_empty_list_clears_the_checklist() {
    let (tool, store) = tool(false);
    tool.execute(json!({ "todos": [{ "content": "A", "status": "pending" }] }))
        .await;
    let result = tool.execute(json!({ "todos": [] })).await;

    assert!(!result.is_error);
    assert!(store.is_empty());
}

#[tokio::test]
async fn rejects_two_in_progress_and_leaves_the_store_untouched() {
    let (tool, store) = tool(false);
    tool.execute(json!({ "todos": [{ "content": "Kept", "status": "pending" }] }))
        .await;

    let result = tool
        .execute(json!({
            "todos": [
                { "content": "A", "status": "in_progress" },
                { "content": "B", "status": "in_progress" }
            ]
        }))
        .await;

    assert!(result.is_error);
    assert!(
        result.content.contains("at most one task may be in_progress"),
        "error must tell the model how to fix it: {}",
        result.content
    );
    assert_eq!(
        store.snapshot()[0].content,
        "Kept",
        "a rejected call must not partially apply"
    );
}

#[tokio::test]
async fn accepts_two_in_progress_when_configured_for_parallel_work() {
    let (tool, store) = tool(true);
    let result = tool
        .execute(json!({
            "todos": [
                { "content": "A", "status": "in_progress" },
                { "content": "B", "status": "in_progress" }
            ]
        }))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert_eq!(store.snapshot().len(), 2);
}

#[tokio::test]
async fn rejects_duplicates_and_blank_content() {
    let (tool, _store) = tool(false);

    let duplicate = tool
        .execute(json!({
            "todos": [
                { "content": "Run tests", "status": "pending" },
                { "content": "Run tests", "status": "pending" }
            ]
        }))
        .await;
    assert!(duplicate.is_error);
    assert!(duplicate.content.contains("duplicate content"));

    let blank = tool
        .execute(json!({ "todos": [{ "content": "   ", "status": "pending" }] }))
        .await;
    assert!(blank.is_error);
    assert!(blank.content.contains("non-empty"));
}

#[tokio::test]
async fn rejects_a_malformed_payload() {
    let (tool, _store) = tool(false);

    let missing = tool.execute(json!({})).await;
    assert!(missing.is_error);

    let wrong_type = tool.execute(json!({ "todos": "not an array" })).await;
    assert!(wrong_type.is_error);

    let unknown_status = tool
        .execute(json!({ "todos": [{ "content": "A", "status": "blocked" }] }))
        .await;
    assert!(unknown_status.is_error);
    assert!(unknown_status.content.contains("unknown status"));
}

#[test]
fn advertises_the_expected_contract() {
    let (tool, _store) = tool(false);

    assert_eq!(tool.name(), TODO_WRITE_TOOL_NAME);
    assert_eq!(tool.category(), ToolCategory::Info, "must stay usable in plan mode");
    assert!(!tool.is_deferred(), "a checklist the model must search for goes unused");
    assert!(
        !tool.is_concurrency_safe(&json!({})),
        "whole-list replacement is order-sensitive"
    );
}

#[test]
fn description_tracks_the_parallel_policy() {
    let (single, _) = tool(false);
    let (parallel, _) = tool(true);

    assert!(single.description().contains("AT MOST ONE task in_progress"));
    assert!(parallel.description().contains("several at once"));
}

#[test]
fn schema_rejects_unknown_item_keys() {
    let (tool, _store) = tool(false);
    let schema = tool.input_schema();
    let items = &schema["properties"]["todos"]["items"];

    assert_eq!(items["additionalProperties"], json!(false));
    assert_eq!(items["required"], json!(["content", "status"]));
    assert_eq!(
        items["properties"]["status"]["enum"],
        json!(["pending", "in_progress", "completed"])
    );
    assert!(
        items["properties"].get("activeForm").is_some(),
        "activeForm is offered but optional"
    );
}

#[test]
fn describe_summarizes_without_leaking_task_text() {
    let (tool, _store) = tool(false);
    let input = json!({
        "todos": [
            { "content": "Secret task", "status": "in_progress" },
            { "content": "Another", "status": "pending" }
        ]
    });

    let described = tool.describe(&input);
    assert_eq!(described, "Update todo list (2 items, 1 in progress)");
    assert!(!described.contains("Secret task"));
}

#[test]
fn describe_degrades_gracefully_on_bad_input() {
    let (tool, _store) = tool(false);
    assert_eq!(tool.describe(&json!({ "todos": "nonsense" })), "Update todo list");
}

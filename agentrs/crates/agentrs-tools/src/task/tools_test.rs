use std::sync::Arc;

use agentrs_protocol::events::ToolCategory;
use serde_json::json;
use tempfile::TempDir;

use super::{TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool};
use crate::Tool;
use crate::task::store::TaskStore;

struct Fixture {
    create: TaskCreateTool,
    list: TaskListTool,
    get: TaskGetTool,
    update: TaskUpdateTool,
    _dir: TempDir,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(TaskStore::new(dir.path().join("tasks")));
    Fixture {
        create: TaskCreateTool::new(Arc::clone(&store)),
        list: TaskListTool::new(Arc::clone(&store)),
        get: TaskGetTool::new(Arc::clone(&store)),
        update: TaskUpdateTool::new(store),
        _dir: dir,
    }
}

#[tokio::test]
async fn create_reports_the_ids_the_model_will_need() {
    let f = fixture();
    let result = f
        .create
        .execute(json!({
            "tasks": [
                { "subject": "Design the API", "activeForm": "Designing the API" },
                { "subject": "Build it", "blockedBy": ["1"] }
            ]
        }))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("Created 2 task(s)"), "{}", result.content);
    assert!(result.content.contains("#1 [pending] Design the API"), "{}", result.content);
    assert!(result.content.contains("blocked by: 1"), "{}", result.content);
}

#[tokio::test]
async fn create_rejects_a_blank_subject_without_writing_anything() {
    let f = fixture();
    let result = f.create.execute(json!({ "tasks": [{ "subject": "  " }] })).await;

    assert!(result.is_error);
    assert!(result.content.contains("non-empty"), "{}", result.content);
    assert_eq!(f.list.execute(json!({})).await.content, "No tasks.");
}

#[tokio::test]
async fn create_rejects_a_malformed_payload() {
    let f = fixture();
    assert!(f.create.execute(json!({})).await.is_error);
    assert!(f.create.execute(json!({ "tasks": "nope" })).await.is_error);
}

#[tokio::test]
async fn list_filters_by_status_and_owner() {
    let f = fixture();
    f.create
        .execute(json!({
            "tasks": [
                { "subject": "Mine", "owner": "alice" },
                { "subject": "Theirs", "owner": "bob" }
            ]
        }))
        .await;
    f.update.execute(json!({ "taskId": "1", "status": "in_progress" })).await;

    let by_owner = f.list.execute(json!({ "owner": "alice" })).await;
    assert!(by_owner.content.contains("Mine"));
    assert!(!by_owner.content.contains("Theirs"));

    let by_status = f.list.execute(json!({ "status": "pending" })).await;
    assert!(by_status.content.contains("Theirs"));
    assert!(!by_status.content.contains("Mine"));
}

#[tokio::test]
async fn list_reports_an_empty_graph_plainly() {
    let f = fixture();
    assert_eq!(f.list.execute(json!({})).await.content, "No tasks.");
}

#[tokio::test]
async fn list_rejects_an_unknown_status_filter() {
    let f = fixture();
    let result = f.list.execute(json!({ "status": "blocked" })).await;
    assert!(result.is_error);
    assert!(result.content.contains("unknown status"), "{}", result.content);
}

#[tokio::test]
async fn get_shows_both_sides_of_a_dependency() {
    let f = fixture();
    f.create
        .execute(json!({
            "tasks": [{ "subject": "Design" }, { "subject": "Build", "blockedBy": ["1"] }]
        }))
        .await;

    let design = f.get.execute(json!({ "taskId": "1" })).await;
    assert!(design.content.contains("blocks: 2"), "{}", design.content);

    let build = f.get.execute(json!({ "taskId": "2" })).await;
    assert!(build.content.contains("blocked by: 1"), "{}", build.content);
}

#[tokio::test]
async fn get_points_an_unknown_id_at_the_list() {
    let f = fixture();
    let result = f.get.execute(json!({ "taskId": "9" })).await;

    assert!(result.is_error);
    assert!(result.content.contains("TaskList"), "the model needs a way out: {}", result.content);
}

#[tokio::test]
async fn update_refuses_to_start_a_blocked_task() {
    let f = fixture();
    f.create
        .execute(json!({
            "tasks": [{ "subject": "Design" }, { "subject": "Build", "blockedBy": ["1"] }]
        }))
        .await;

    let result = f.update.execute(json!({ "taskId": "2", "status": "in_progress" })).await;
    assert!(result.is_error);
    assert!(result.content.contains("blocked by 1"), "{}", result.content);
    assert!(
        result.content.contains("remove_blocked_by"),
        "the error must offer the escape hatch: {}",
        result.content
    );
}

#[tokio::test]
async fn update_deletes_on_request() {
    let f = fixture();
    f.create.execute(json!({ "tasks": [{ "subject": "Doomed" }] })).await;

    let result = f.update.execute(json!({ "taskId": "1", "delete": true })).await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(f.list.execute(json!({})).await.content, "No tasks.");
}

#[tokio::test]
async fn an_update_that_changes_nothing_is_rejected() {
    let f = fixture();
    f.create.execute(json!({ "tasks": [{ "subject": "Task" }] })).await;

    let result = f.update.execute(json!({ "taskId": "1" })).await;
    assert!(result.is_error, "a no-op update is a model mistake worth reporting");
    assert!(result.content.contains("nothing to update"), "{}", result.content);
}

#[tokio::test]
async fn update_rejects_a_missing_task_id() {
    let f = fixture();
    assert!(f.update.execute(json!({ "status": "completed" })).await.is_error);
}

#[test]
fn the_tools_advertise_the_expected_contract() {
    let f = fixture();

    for tool in [
        &f.create as &dyn Tool,
        &f.list as &dyn Tool,
        &f.get as &dyn Tool,
        &f.update as &dyn Tool,
    ] {
        assert_eq!(tool.category(), ToolCategory::Info, "{} category", tool.name());
        assert!(!tool.is_deferred(), "{} must stay advertised", tool.name());
    }

    // Reads may run alongside anything; writes are read-modify-write and are
    // serialized so two calls in one round cannot interleave.
    assert!(f.list.is_concurrency_safe(&json!({})));
    assert!(f.get.is_concurrency_safe(&json!({})));
    assert!(!f.create.is_concurrency_safe(&json!({})));
    assert!(!f.update.is_concurrency_safe(&json!({})));
}

#[test]
fn describe_summarizes_without_leaking_task_text() {
    let f = fixture();

    let described = f
        .create
        .describe(&json!({ "tasks": [{ "subject": "Secret work" }, { "subject": "More" }] }));
    assert_eq!(described, "Create 2 task(s)");
    assert!(!described.contains("Secret work"));

    assert_eq!(f.update.describe(&json!({ "taskId": "3" })), "Update task #3");
    assert_eq!(f.get.describe(&json!({ "taskId": "3" })), "Read task #3");
    assert_eq!(f.list.describe(&json!({})), "List tasks");
}

#[test]
fn create_schema_rejects_unknown_item_keys() {
    let f = fixture();
    let items = &f.create.input_schema()["properties"]["tasks"]["items"];

    assert_eq!(items["additionalProperties"], json!(false));
    assert_eq!(items["required"], json!(["subject"]));
}

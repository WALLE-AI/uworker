use tempfile::TempDir;

use super::TaskStore;
use crate::task::model::{TaskDraft, TaskError, TaskPatch, TaskStatus};

fn store() -> (TaskStore, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let store = TaskStore::new(dir.path().join("tasks"));
    (store, dir)
}

fn draft(subject: &str) -> TaskDraft {
    TaskDraft {
        subject: subject.to_string(),
        description: String::new(),
        active_form: None,
        owner: None,
        blocked_by: Vec::new(),
    }
}

fn blocked_draft(subject: &str, blocked_by: &[&str]) -> TaskDraft {
    TaskDraft {
        blocked_by: blocked_by.iter().map(|id| id.to_string()).collect(),
        ..draft(subject)
    }
}

fn status_patch(status: TaskStatus) -> TaskPatch {
    TaskPatch {
        status: Some(status),
        ..TaskPatch::default()
    }
}

#[test]
fn creates_tasks_with_sequential_ids() {
    let (store, _dir) = store();
    let created = store
        .create(vec![draft("First"), draft("Second")])
        .expect("create should succeed");

    assert_eq!(created[0].id, "1");
    assert_eq!(created[1].id, "2");
    assert_eq!(created[0].status, TaskStatus::Pending, "new tasks start pending");
}

#[test]
fn trims_and_rejects_a_blank_subject() {
    let (store, _dir) = store();
    let created = store.create(vec![draft("  Padded  ")]).expect("create");
    assert_eq!(created[0].subject, "Padded");

    assert_eq!(store.create(vec![draft("   ")]).unwrap_err(), TaskError::EmptySubject);
}

#[test]
fn ids_are_never_reused_after_a_delete() {
    let (store, _dir) = store();
    store.create(vec![draft("First")]).expect("create");
    store.delete("1").expect("delete");

    let created = store.create(vec![draft("Second")]).expect("create");
    assert_eq!(
        created[0].id, "2",
        "reusing id 1 would silently re-point anything still naming it"
    );
}

#[test]
fn a_dependency_is_recorded_on_both_sides() {
    let (store, _dir) = store();
    store.create(vec![draft("Design"), blocked_draft("Build", &["1"])]).expect("create");

    let design = store.get("1").expect("read").expect("exists");
    let build = store.get("2").expect("read").expect("exists");

    assert_eq!(build.blocked_by, vec!["1".to_string()]);
    assert_eq!(design.blocks, vec!["2".to_string()], "the mirror side must be kept in sync");
}

#[test]
fn a_batch_may_depend_on_a_sibling_created_in_the_same_call() {
    let (store, _dir) = store();
    let created = store
        .create(vec![draft("Design"), blocked_draft("Build", &["1"])])
        .expect("a batch should resolve its own ids");

    assert_eq!(created[1].blocked_by, vec!["1".to_string()]);
}

#[test]
fn depending_on_an_unknown_task_is_refused() {
    let (store, _dir) = store();
    let error = store.create(vec![blocked_draft("Build", &["99"])]).unwrap_err();
    assert_eq!(error, TaskError::NotFound { id: "99".to_string() });
}

#[test]
fn a_task_cannot_block_itself() {
    let (store, _dir) = store();
    store.create(vec![draft("Solo")]).expect("create");

    let patch = TaskPatch {
        add_blocked_by: vec!["1".to_string()],
        ..TaskPatch::default()
    };
    assert_eq!(
        store.update("1", patch).unwrap_err(),
        TaskError::SelfDependency { id: "1".to_string() }
    );
}

#[test]
fn a_dependency_cycle_is_refused_and_named() {
    let (store, _dir) = store();
    store
        .create(vec![draft("A"), blocked_draft("B", &["1"]), blocked_draft("C", &["2"])])
        .expect("create");

    // A already waits on nothing; making A wait on C closes A -> B -> C -> A.
    let patch = TaskPatch {
        add_blocked_by: vec!["3".to_string()],
        ..TaskPatch::default()
    };
    let error = store.update("1", patch).unwrap_err();

    let TaskError::DependencyCycle { cycle } = error else {
        panic!("expected a cycle error, got {error:?}");
    };
    assert!(cycle.contains("1"), "the loop must name the task: {cycle}");
    assert!(cycle.contains("3"), "the loop must name the new edge: {cycle}");
}

#[test]
fn a_diamond_is_not_mistaken_for_a_cycle() {
    let (store, _dir) = store();
    store
        .create(vec![
            draft("Root"),
            blocked_draft("Left", &["1"]),
            blocked_draft("Right", &["1"]),
        ])
        .expect("create");

    // Both branches converging on one task is a diamond, not a loop.
    let patch = TaskPatch {
        add_blocked_by: vec!["2".to_string(), "3".to_string()],
        ..TaskPatch::default()
    };
    let merged = store.create(vec![draft("Merge")]).expect("create");
    store.update(&merged[0].id, patch).expect("a diamond is legal");
}

#[test]
fn a_blocked_task_cannot_be_started_or_completed() {
    let (store, _dir) = store();
    store.create(vec![draft("Design"), blocked_draft("Build", &["1"])]).expect("create");

    for status in [TaskStatus::InProgress, TaskStatus::Completed] {
        let error = store.update("2", status_patch(status)).unwrap_err();
        let TaskError::Blocked { id, blockers, .. } = error else {
            panic!("expected a blocked error, got {error:?}");
        };
        assert_eq!(id, "2");
        assert_eq!(blockers, "1", "the error must name what to finish first");
    }
}

#[test]
fn finishing_the_blocker_unblocks_the_task() {
    let (store, _dir) = store();
    store.create(vec![draft("Design"), blocked_draft("Build", &["1"])]).expect("create");

    store.update("1", status_patch(TaskStatus::Completed)).expect("blocker done");
    let build = store.update("2", status_patch(TaskStatus::InProgress)).expect("now startable");

    assert_eq!(build.status, TaskStatus::InProgress);
}

#[test]
fn a_blocked_task_may_still_be_left_pending() {
    let (store, _dir) = store();
    store.create(vec![draft("Design"), blocked_draft("Build", &["1"])]).expect("create");

    store
        .update("2", status_patch(TaskStatus::Pending))
        .expect("pending is always allowed");
}

#[test]
fn dropping_the_dependency_and_starting_in_one_call_is_allowed() {
    let (store, _dir) = store();
    store.create(vec![draft("Design"), blocked_draft("Build", &["1"])]).expect("create");

    let patch = TaskPatch {
        status: Some(TaskStatus::InProgress),
        remove_blocked_by: vec!["1".to_string()],
        ..TaskPatch::default()
    };
    let build = store
        .update("2", patch)
        .expect("status is judged against the dependencies the call leaves behind");

    assert_eq!(build.status, TaskStatus::InProgress);
    assert!(build.blocked_by.is_empty());
    assert!(
        store.get("1").expect("read").expect("exists").blocks.is_empty(),
        "the mirror side must be cleared too"
    );
}

#[test]
fn a_patch_only_touches_the_fields_it_names() {
    let (store, _dir) = store();
    store
        .create(vec![TaskDraft {
            description: "the long form".to_string(),
            active_form: Some("Doing it".to_string()),
            owner: Some("alice".to_string()),
            ..draft("Original")
        }])
        .expect("create");

    let patch = TaskPatch {
        status: Some(TaskStatus::InProgress),
        ..TaskPatch::default()
    };
    let updated = store.update("1", patch).expect("update");

    assert_eq!(updated.subject, "Original");
    assert_eq!(updated.description, "the long form");
    assert_eq!(updated.active_form.as_deref(), Some("Doing it"));
    assert_eq!(updated.owner.as_deref(), Some("alice"));
}

#[test]
fn deleting_a_task_removes_every_edge_naming_it() {
    let (store, _dir) = store();
    store
        .create(vec![draft("Design"), blocked_draft("Build", &["1"]), draft("Ship")])
        .expect("create");
    store.delete("1").expect("delete");

    let build = store.get("2").expect("read").expect("exists");
    assert!(
        build.blocked_by.is_empty(),
        "a dangling dependency would block the task forever"
    );
    assert!(store.get("1").expect("read").is_none());
}

#[test]
fn updating_or_deleting_an_unknown_task_is_refused() {
    let (store, _dir) = store();
    let missing = TaskError::NotFound { id: "7".to_string() };

    assert_eq!(store.update("7", status_patch(TaskStatus::Pending)).unwrap_err(), missing);
    assert_eq!(store.delete("7").unwrap_err(), missing);
}

#[test]
fn the_graph_survives_a_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("tasks");

    let first = TaskStore::new(path.clone());
    first.create(vec![draft("Design"), blocked_draft("Build", &["1"])]).expect("create");

    let second = TaskStore::new(path);
    let tasks = second.list().expect("read");
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[1].blocked_by, vec!["1".to_string()]);

    let created = second.create(vec![draft("Ship")]).expect("create");
    assert_eq!(created[0].id, "3", "the id counter must survive too");
}

#[test]
fn an_empty_store_reads_as_no_tasks() {
    let (store, _dir) = store();
    assert!(store.list().expect("read").is_empty());
    assert!(store.get("1").expect("read").is_none());
}

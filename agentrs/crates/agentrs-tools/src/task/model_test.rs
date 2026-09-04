use super::{Task, TaskPatch, TaskStatus, join_ids};

fn task(status: TaskStatus, active_form: Option<&str>) -> Task {
    Task {
        id: "1".to_string(),
        subject: "Run the tests".to_string(),
        description: String::new(),
        active_form: active_form.map(str::to_string),
        owner: None,
        status,
        blocks: Vec::new(),
        blocked_by: Vec::new(),
    }
}

#[test]
fn only_a_completed_task_is_closed() {
    assert!(task(TaskStatus::Pending, None).is_open());
    assert!(task(TaskStatus::InProgress, None).is_open());
    assert!(!task(TaskStatus::Completed, None).is_open());
}

#[test]
fn display_falls_back_to_the_subject() {
    assert_eq!(task(TaskStatus::InProgress, None).display_active(), "Run the tests");
    assert_eq!(
        task(TaskStatus::InProgress, Some("Running the tests")).display_active(),
        "Running the tests"
    );
}

#[test]
fn an_untouched_patch_is_empty() {
    assert!(TaskPatch::default().is_empty());
}

#[test]
fn any_named_field_makes_a_patch_non_empty() {
    let with_status = TaskPatch {
        status: Some(TaskStatus::Completed),
        ..TaskPatch::default()
    };
    assert!(!with_status.is_empty());

    let with_dependency = TaskPatch {
        add_blocked_by: vec!["2".to_string()],
        ..TaskPatch::default()
    };
    assert!(!with_dependency.is_empty());

    let with_removal = TaskPatch {
        remove_blocked_by: vec!["2".to_string()],
        ..TaskPatch::default()
    };
    assert!(!with_removal.is_empty());
}

#[test]
fn ids_render_as_a_readable_list() {
    assert_eq!(join_ids(&["1".to_string(), "2".to_string()]), "1, 2");
    assert_eq!(join_ids(&[]), "");
}

#[test]
fn an_absent_optional_field_is_omitted_from_storage() {
    let json = serde_json::to_string(&task(TaskStatus::Pending, None)).expect("serialize");
    for absent in ["active_form", "owner", "description", "blocks", "blocked_by"] {
        assert!(!json.contains(absent), "{absent} should be omitted: {json}");
    }
}

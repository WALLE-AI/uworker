use agentrs_types::message::ContentBlock;
use serde_json::json;

use super::observe_memory_writes;

fn call(name: &str, path: &std::path::Path) -> ContentBlock {
    ContentBlock::ToolUse {
        id: "call".into(),
        name: name.into(),
        input: json!({ "file_path": path }),
        extra: None,
    }
}

#[test]
fn detects_write_and_edit_inside_memory_only() {
    let temp = tempfile::tempdir().unwrap();
    let memory = temp.path().join("memory");
    std::fs::create_dir_all(&memory).unwrap();
    assert!(
        observe_memory_writes(&[call("Write", &memory.join("a.md"))], Some(&memory), false)
            .unwrap()
            .first_write
    );
    assert!(
        !observe_memory_writes(&[call("Edit", &memory.join("a.md"))], Some(&memory), true)
            .unwrap()
            .first_write
    );
    assert!(observe_memory_writes(&[call("Write", &temp.path().join("outside.md"))], Some(&memory), false).is_none());
    assert!(observe_memory_writes(&[call("Read", &memory.join("a.md"))], Some(&memory), false).is_none());
}

#[test]
fn disabled_or_traversing_paths_do_not_trigger() {
    let temp = tempfile::tempdir().unwrap();
    let memory = temp.path().join("memory");
    std::fs::create_dir_all(&memory).unwrap();
    let traversing = memory.join("..").join("outside.md");
    assert!(observe_memory_writes(&[call("Write", &traversing)], Some(&memory), false).is_none());
    assert!(observe_memory_writes(&[call("Write", &memory.join("a.md"))], None, false).is_none());
}

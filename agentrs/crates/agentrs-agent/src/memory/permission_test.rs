use serde_json::json;

use super::is_memory_carve_out;

#[test]
fn covers_reads_and_writes_inside_the_memory_directory() {
    let temp = tempfile::tempdir().unwrap();
    let memory = temp.path().join("memory");
    std::fs::create_dir_all(&memory).unwrap();
    let inside = json!({ "file_path": memory.join("user_role.md") });

    for tool in ["Read", "Write", "Edit"] {
        assert!(
            is_memory_carve_out(tool, &inside, Some(&memory)),
            "{tool} should be covered"
        );
    }
}

#[test]
fn does_not_cover_paths_outside_the_memory_directory() {
    let temp = tempfile::tempdir().unwrap();
    let memory = temp.path().join("memory");
    std::fs::create_dir_all(&memory).unwrap();

    let outside = json!({ "file_path": temp.path().join("secrets.env") });
    assert!(!is_memory_carve_out("Write", &outside, Some(&memory)));

    // `..` must not escape the directory even though the prefix matches.
    let traversing = json!({ "file_path": memory.join("..").join("secrets.env") });
    assert!(!is_memory_carve_out("Write", &traversing, Some(&memory)));
}

#[test]
fn does_not_cover_other_tools_or_malformed_input() {
    let temp = tempfile::tempdir().unwrap();
    let memory = temp.path().join("memory");
    std::fs::create_dir_all(&memory).unwrap();

    // ExecCommand could touch the same file without naming it in `file_path`,
    // so it never qualifies.
    let shell = json!({ "command": format!("rm {}", memory.join("a.md").display()) });
    assert!(!is_memory_carve_out("ExecCommand", &shell, Some(&memory)));

    assert!(!is_memory_carve_out("Write", &json!({}), Some(&memory)));
    assert!(!is_memory_carve_out(
        "Write",
        &json!({ "file_path": 42 }),
        Some(&memory)
    ));
}

#[test]
fn overridden_or_disabled_memory_directories_keep_normal_approval() {
    let temp = tempfile::tempdir().unwrap();
    let memory = temp.path().join("memory");
    std::fs::create_dir_all(&memory).unwrap();
    let inside = json!({ "file_path": memory.join("user_role.md") });

    // `None` is what the caller passes for an explicitly overridden directory
    // and for a session with memory disabled.
    assert!(!is_memory_carve_out("Write", &inside, None));
}

use std::fs;

use agentrs_memory::paths::ENTRYPOINT_NAME;

use super::memory_shape;

fn memory_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("memory");
    fs::create_dir_all(&directory).unwrap();
    (temp, directory)
}

#[test]
fn an_empty_directory_reports_zeroes_rather_than_failing() {
    let (_temp, directory) = memory_dir();

    let shape = memory_shape(&directory);

    assert_eq!(shape.entries, 0);
    assert_eq!(shape.index_lines, 0);
    assert_eq!(shape.index_bytes, 0);
    assert!(!shape.index_truncated);
    assert_eq!(shape.oldest_entry_days, None);
}

#[test]
fn a_missing_directory_is_measurable_too() {
    let temp = tempfile::tempdir().unwrap();

    let shape = memory_shape(&temp.path().join("absent"));

    assert_eq!(shape.entries, 0);
    assert_eq!(shape.oldest_entry_days, None);
}

#[test]
fn entries_exclude_the_index_itself() {
    let (_temp, directory) = memory_dir();
    fs::write(directory.join(ENTRYPOINT_NAME), "- [Role](user_role.md) - who\n").unwrap();
    fs::write(directory.join("user_role.md"), "---\nname: user_role\n---\n\nbody").unwrap();
    fs::write(
        directory.join("feedback_tests.md"),
        "---\nname: feedback_tests\n---\n\nbody",
    )
    .unwrap();

    let shape = memory_shape(&directory);

    assert_eq!(shape.entries, 2, "MEMORY.md must not be counted as a memory");
    assert_eq!(shape.index_lines, 1);
    assert_eq!(shape.oldest_entry_days, Some(0));
}

#[test]
fn an_oversized_index_is_reported_at_its_pre_truncation_size() {
    let (_temp, directory) = memory_dir();
    let lines = 400;
    let content = "- [Entry](entry.md) - a summary long enough to add up\n".repeat(lines);
    fs::write(directory.join(ENTRYPOINT_NAME), &content).unwrap();

    let shape = memory_shape(&directory);

    assert!(shape.index_truncated, "400 lines must exceed the index budget");
    assert_eq!(
        shape.index_lines, lines,
        "the shape must describe what is on disk, not the trimmed prompt"
    );
    assert_eq!(
        shape.index_bytes,
        content.trim_end().len(),
        "byte count excludes the surrounding whitespace the index reader trims"
    );
}

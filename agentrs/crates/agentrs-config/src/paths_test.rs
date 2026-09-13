use std::fs;

use super::{is_within, resolve};

#[test]
fn a_child_of_the_root_is_contained() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let file = root.join("child.txt");
    fs::write(&file, "x").unwrap();

    assert!(is_within(&file, &root));
    assert!(is_within(&root, &root), "a root contains itself");
}

// Containment is normally checked before the file exists — that is the whole
// point of confining a write — so this is the load-bearing case.
#[test]
fn a_path_that_does_not_exist_yet_is_still_judged() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir_all(&root).unwrap();

    assert!(is_within(&root.join("not_written_yet.md"), &root));
    assert!(is_within(&root.join("nested").join("deep.md"), &root));
    assert!(!is_within(&temp.path().join("elsewhere.md"), &root));
}

#[test]
fn a_sibling_is_not_contained() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let sibling = temp.path().join("sibling");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&sibling).unwrap();

    assert!(!is_within(&sibling, &root));
    assert!(!is_within(&sibling.join("file.txt"), &root));
}

// A prefix match on the raw string would accept `root-backup` as a child of
// `root`; component-wise comparison must not.
#[test]
fn a_name_that_merely_starts_with_the_root_is_not_contained() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let decoy = temp.path().join("root-backup");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&decoy).unwrap();

    assert!(!is_within(&decoy.join("file.txt"), &root));
}

#[test]
fn traversal_cannot_escape_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let outside = temp.path().join("secrets.env");
    fs::write(&outside, "TOKEN=1").unwrap();

    // Resolvable through the filesystem: canonicalization settles the `..`.
    assert!(!is_within(&root.join("..").join("secrets.env"), &root));
    // Unresolvable: refused rather than guessed at.
    assert!(!is_within(&root.join("..").join("absent.env"), &root));
}

#[test]
fn an_unresolvable_path_yields_none() {
    let temp = tempfile::tempdir().unwrap();

    assert!(resolve(&temp.path().join("..").join("absent")).is_none());
    assert!(
        resolve(&temp.path().join("absent")).is_some(),
        "a missing leaf under an existing parent is still resolvable"
    );
}

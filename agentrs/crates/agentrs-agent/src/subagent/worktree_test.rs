use std::process::Command;

use tempfile::tempdir;

use super::{Cleanup, Worktree};
use agentrs_types::subagent::SubAgentId;

fn repository() -> tempfile::TempDir {
    let directory = tempdir().expect("temp repository");
    run(directory.path(), &["init"]);
    std::fs::write(directory.path().join("tracked.txt"), "base\n").expect("write fixture");
    run(directory.path(), &["add", "tracked.txt"]);
    run(
        directory.path(),
        &[
            "-c",
            "user.name=AgentRS Test",
            "-c",
            "user.email=agentrs@example.invalid",
            "commit",
            "-m",
            "fixture",
        ],
    );
    directory
}

fn run(cwd: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[tokio::test]
async fn clean_worktree_is_removed() {
    let repository = repository();
    let worktree = Worktree::create(repository.path(), &SubAgentId::new(uuid::Uuid::now_v7().to_string()))
        .await
        .expect("create worktree");
    let path = worktree.path().to_path_buf();

    assert!(matches!(worktree.cleanup_if_clean().await.unwrap(), Cleanup::Removed));
    assert!(!path.exists());
}

#[tokio::test]
async fn dirty_worktree_is_preserved() {
    let repository = repository();
    let worktree = Worktree::create(repository.path(), &SubAgentId::new(uuid::Uuid::now_v7().to_string()))
        .await
        .expect("create worktree");
    let path = worktree.path().to_path_buf();
    std::fs::write(path.join("new.txt"), "child change\n").expect("write child change");

    let cleanup = worktree.cleanup_if_clean().await.unwrap();
    assert!(matches!(cleanup, Cleanup::Preserved(ref preserved) if preserved == &path));
    assert!(path.exists());

    run(
        repository.path(),
        &["worktree", "remove", "--force", &path.to_string_lossy()],
    );
}

#[tokio::test]
async fn dropping_a_clean_worktree_removes_it_after_an_early_exit() {
    let repository = repository();
    let worktree = Worktree::create(repository.path(), &SubAgentId::new(uuid::Uuid::now_v7().to_string()))
        .await
        .expect("create worktree");
    let path = worktree.path().to_path_buf();

    drop(worktree);

    assert!(!path.exists());
}

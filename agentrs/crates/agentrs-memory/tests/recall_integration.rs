// Integration tests for relevance-based memory recall.
//
// Written against the public surface an embedder uses: implement
// `MemorySelector`, hand `find_relevant_memories` a directory, render what
// comes back. No LLM calls are involved — the selector is the seam.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use agentrs_memory::recall::{
    MemorySelector, RecallRequest, SelectionRequest, find_relevant_memories, render_recalled_memories,
};

/// Returns whatever it was constructed with, and counts how often it ran.
struct FixedSelector {
    answer: Vec<String>,
    calls: AtomicUsize,
}

impl FixedSelector {
    fn new(answer: &[&str]) -> Self {
        Self {
            answer: answer.iter().map(|name| (*name).to_owned()).collect(),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl MemorySelector for FixedSelector {
    async fn select(&self, _request: SelectionRequest<'_>) -> Vec<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

/// A selector that always fails, the way a timed-out or erroring model does.
struct BrokenSelector;

#[async_trait::async_trait]
impl MemorySelector for BrokenSelector {
    async fn select(&self, _request: SelectionRequest<'_>) -> Vec<String> {
        Vec::new()
    }
}

fn memory_dir() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("memory");
    fs::create_dir_all(&dir).unwrap();
    (temp, dir)
}

fn write(dir: &Path, name: &str, description: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(
        &path,
        format!("---\nname: {name}\ndescription: {description}\ntype: user\n---\n\n{body}\n"),
    )
    .unwrap();
    path
}

fn recall<'a>(dir: &'a Path, surfaced: &'a HashSet<PathBuf>, limit: usize) -> RecallRequest<'a> {
    RecallRequest {
        query: "how do I run the migration",
        memory_dir: dir,
        recent_tools: &[],
        already_surfaced: surfaced,
        limit,
    }
}

// AC3-1: a directory larger than the budget still yields at most `limit`.
#[tokio::test]
async fn recall_never_exceeds_the_configured_limit() {
    let (_temp, dir) = memory_dir();
    let names: Vec<String> = (0..10).map(|index| format!("m{index}.md")).collect();
    for name in &names {
        write(&dir, name, "d", "body");
    }
    let selector = FixedSelector::new(&names.iter().map(String::as_str).collect::<Vec<_>>());

    let selected = find_relevant_memories(recall(&dir, &HashSet::new(), 5), &selector).await;

    assert_eq!(selected.len(), 5);
}

// AC3-2: the same file is not injected two turns running.
#[tokio::test]
async fn a_file_injected_once_is_not_injected_again() {
    let (_temp, dir) = memory_dir();
    write(&dir, "user_role.md", "who the user is", "A senior Rust engineer.");
    write(
        &dir,
        "feedback_tests.md",
        "how to test",
        "Integration tests hit a real DB.",
    );
    let selector = FixedSelector::new(&["user_role.md", "feedback_tests.md"]);

    let mut surfaced = HashSet::new();
    let first = find_relevant_memories(recall(&dir, &surfaced, 5), &selector).await;
    assert_eq!(first.len(), 2);
    for header in &first {
        surfaced.insert(header.file_path.clone());
    }

    let second = find_relevant_memories(recall(&dir, &surfaced, 5), &selector).await;
    assert!(second.is_empty(), "everything has already been surfaced");
}

// AC3-3: a filename the model invented is dropped without an error.
#[tokio::test]
async fn filenames_that_do_not_exist_are_discarded_silently() {
    let (_temp, dir) = memory_dir();
    write(&dir, "real.md", "d", "body");
    let selector = FixedSelector::new(&["imagined.md", "real.md", "also_imagined.md"]);

    let selected = find_relevant_memories(recall(&dir, &HashSet::new(), 5), &selector).await;

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].filename, "real.md");
}

// AC3-4: a selector that yields nothing leaves the caller with an empty list,
// not an error to handle.
#[tokio::test]
async fn a_failing_selector_degrades_to_injecting_nothing() {
    let (_temp, dir) = memory_dir();
    write(&dir, "real.md", "d", "body");

    let selected = find_relevant_memories(recall(&dir, &HashSet::new(), 5), &BrokenSelector).await;

    assert!(selected.is_empty());
    assert!(render_recalled_memories(&selected).is_none());
}

// AC3-5: an empty directory costs no side query at all.
#[tokio::test]
async fn an_empty_directory_produces_no_selection_call() {
    let (_temp, dir) = memory_dir();
    let selector = FixedSelector::new(&["anything.md"]);

    let selected = find_relevant_memories(recall(&dir, &HashSet::new(), 5), &selector).await;

    assert!(selected.is_empty());
    assert_eq!(selector.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_rendered_block_carries_the_bodies_and_is_addressed_to_the_model() {
    let (_temp, dir) = memory_dir();
    write(&dir, "user_role.md", "who the user is", "A senior Rust engineer.");
    let selector = FixedSelector::new(&["user_role.md"]);

    let selected = find_relevant_memories(recall(&dir, &HashSet::new(), 5), &selector).await;
    let rendered = render_recalled_memories(&selected).expect("a block");

    assert!(rendered.starts_with("<system-reminder>"));
    assert!(rendered.ends_with("</system-reminder>"));
    assert!(rendered.contains("## user_role.md (updated today)"));
    assert!(rendered.contains("A senior Rust engineer."));
}

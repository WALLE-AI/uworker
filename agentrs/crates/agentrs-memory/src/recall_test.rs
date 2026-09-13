use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

use super::{MemorySelector, RecallRequest, SelectionRequest, find_relevant_memories, render_recalled_memories_at};
use crate::types::{MemoryHeader, MemoryType};

/// One recorded invocation of [`StubSelector`].
struct RecordedCall {
    query: String,
    manifest: String,
    recent_tools: Vec<String>,
    limit: usize,
}

/// Selector that returns a canned answer and records what it was asked.
struct StubSelector {
    answer: Vec<String>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl StubSelector {
    fn new(answer: &[&str]) -> Self {
        Self {
            answer: answer.iter().map(|name| (*name).to_owned()).collect(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn last_manifest(&self) -> String {
        self.calls.lock().unwrap().last().unwrap().manifest.clone()
    }
}

#[async_trait::async_trait]
impl MemorySelector for StubSelector {
    async fn select(&self, request: SelectionRequest<'_>) -> Vec<String> {
        self.calls.lock().unwrap().push(RecordedCall {
            query: request.query.to_owned(),
            manifest: request.manifest.to_owned(),
            recent_tools: request.recent_tools.to_vec(),
            limit: request.limit,
        });
        self.answer.clone()
    }
}

fn write_memory_file(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(
        &path,
        format!("---\nname: {name}\ndescription: d\ntype: user\n---\n\n{body}"),
    )
    .unwrap();
    path
}

fn recall_dir() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("memory");
    fs::create_dir_all(&dir).unwrap();
    (temp, dir)
}

#[tokio::test]
async fn selection_is_capped_at_the_limit() {
    let (_temp, dir) = recall_dir();
    for index in 0..10 {
        write_memory_file(&dir, &format!("m{index}.md"), "body");
    }
    let names: Vec<String> = (0..10).map(|index| format!("m{index}.md")).collect();
    let selector = StubSelector::new(&names.iter().map(String::as_str).collect::<Vec<_>>());

    let selected = find_relevant_memories(
        RecallRequest {
            query: "q",
            memory_dir: &dir,
            recent_tools: &[],
            already_surfaced: &HashSet::new(),
            limit: 5,
        },
        &selector,
    )
    .await;

    assert_eq!(selected.len(), 5, "an over-eager selector must not blow the budget");
}

#[tokio::test]
async fn already_surfaced_files_are_withheld_from_the_selector() {
    let (_temp, dir) = recall_dir();
    let seen = write_memory_file(&dir, "seen.md", "body");
    write_memory_file(&dir, "fresh.md", "body");
    let selector = StubSelector::new(&["fresh.md"]);
    let already_surfaced = HashSet::from([seen.clone()]);

    let selected = find_relevant_memories(
        RecallRequest {
            query: "q",
            memory_dir: &dir,
            recent_tools: &[],
            already_surfaced: &already_surfaced,
            limit: 5,
        },
        &selector,
    )
    .await;

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].filename, "fresh.md");
    assert!(
        !selector.last_manifest().contains("seen.md"),
        "the budget must not be spent ranking a file the model can already see"
    );
}

#[tokio::test]
async fn no_candidates_means_no_side_query() {
    let (_temp, dir) = recall_dir();
    let only = write_memory_file(&dir, "only.md", "body");
    let selector = StubSelector::new(&["only.md"]);

    let empty_dir = find_relevant_memories(
        RecallRequest {
            query: "q",
            memory_dir: &dir.join("absent"),
            recent_tools: &[],
            already_surfaced: &HashSet::new(),
            limit: 5,
        },
        &selector,
    )
    .await;
    assert!(empty_dir.is_empty());

    let all_seen = find_relevant_memories(
        RecallRequest {
            query: "q",
            memory_dir: &dir,
            recent_tools: &[],
            already_surfaced: &HashSet::from([only]),
            limit: 5,
        },
        &selector,
    )
    .await;
    assert!(all_seen.is_empty());

    assert_eq!(
        selector.call_count(),
        0,
        "recall must not spend an LLM round trip when there is nothing to rank"
    );
}

#[tokio::test]
async fn invented_and_duplicated_filenames_are_dropped() {
    let (_temp, dir) = recall_dir();
    write_memory_file(&dir, "real.md", "body");
    let selector = StubSelector::new(&["nonexistent.md", "real.md", "real.md", "../escape.md"]);

    let selected = find_relevant_memories(
        RecallRequest {
            query: "q",
            memory_dir: &dir,
            recent_tools: &[],
            already_surfaced: &HashSet::new(),
            limit: 5,
        },
        &selector,
    )
    .await;

    assert_eq!(selected.len(), 1, "only names matching a real candidate survive");
    assert_eq!(selected[0].filename, "real.md");
}

#[tokio::test]
async fn a_zero_limit_short_circuits_before_scanning() {
    let (_temp, dir) = recall_dir();
    write_memory_file(&dir, "real.md", "body");
    let selector = StubSelector::new(&["real.md"]);

    let selected = find_relevant_memories(
        RecallRequest {
            query: "q",
            memory_dir: &dir,
            recent_tools: &[],
            already_surfaced: &HashSet::new(),
            limit: 0,
        },
        &selector,
    )
    .await;

    assert!(selected.is_empty());
    assert_eq!(selector.call_count(), 0);
}

#[tokio::test]
async fn the_selector_receives_the_query_and_recent_tools() {
    let (_temp, dir) = recall_dir();
    write_memory_file(&dir, "real.md", "body");
    let selector = StubSelector::new(&[]);

    find_relevant_memories(
        RecallRequest {
            query: "how do I run the migration",
            memory_dir: &dir,
            recent_tools: &["Grep".to_string()],
            already_surfaced: &HashSet::new(),
            limit: 5,
        },
        &selector,
    )
    .await;

    let calls = selector.calls.lock().unwrap();
    let call = calls.last().unwrap();
    assert_eq!(call.query, "how do I run the migration");
    assert_eq!(call.recent_tools, ["Grep".to_string()]);
    assert_eq!(call.limit, 5);
}

// -- Rendering ----------------------------------------------------------------

fn header(dir: &std::path::Path, name: &str, age_days: i64, now: DateTime<Utc>) -> MemoryHeader {
    MemoryHeader {
        filename: name.to_owned(),
        file_path: dir.join(name),
        // Offset from the same `now` the renderer is given; deriving it from a
        // fresh clock instead would floor the age one day short.
        mtime: now - Duration::days(age_days),
        description: Some("d".to_owned()),
        memory_type: Some(MemoryType::User),
    }
}

#[test]
fn stale_entries_carry_a_revalidation_caveat_and_fresh_ones_do_not() {
    let (_temp, dir) = recall_dir();
    fs::write(dir.join("fresh.md"), "fresh body").unwrap();
    fs::write(dir.join("stale.md"), "stale body").unwrap();
    let now = Utc::now();

    let rendered = render_recalled_memories_at(
        &[header(&dir, "fresh.md", 0, now), header(&dir, "stale.md", 47, now)],
        now,
    )
    .unwrap();

    assert!(rendered.contains("## fresh.md (updated today)"));
    assert!(rendered.contains("## stale.md (updated 47 days ago)"));
    assert!(rendered.contains("This memory is 47 days old"));
    assert_eq!(
        rendered.matches("This memory is").count(),
        1,
        "a same-day memory needs no revalidation caveat"
    );
    assert!(rendered.contains("fresh body") && rendered.contains("stale body"));
}

#[test]
fn a_file_deleted_since_the_scan_is_skipped_rather_than_failing_the_turn() {
    let (_temp, dir) = recall_dir();
    fs::write(dir.join("present.md"), "present body").unwrap();
    let now = Utc::now();

    let rendered = render_recalled_memories_at(
        &[header(&dir, "vanished.md", 1, now), header(&dir, "present.md", 1, now)],
        now,
    )
    .unwrap();

    assert!(!rendered.contains("vanished.md"));
    assert!(rendered.contains("present body"));
}

#[test]
fn nothing_renders_when_no_file_can_be_read() {
    let (_temp, dir) = recall_dir();
    let now = Utc::now();

    assert!(render_recalled_memories_at(&[], now).is_none());
    assert!(render_recalled_memories_at(&[header(&dir, "vanished.md", 1, now)], now).is_none());
}

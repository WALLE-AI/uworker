use std::path::Path;

use agentrs_memory::age::memory_age_days;
use agentrs_memory::index::{read_index, truncate_index};
use agentrs_memory::paths::{is_memory_path, memory_entrypoint};
use agentrs_memory::store::scan_memory_files;
use agentrs_types::message::ContentBlock;
use chrono::Utc;

/// How much memory a session starts with.
///
/// These are the denominators for judging whether memory is earning its
/// context budget: a large index that is never read means the injected tokens
/// are wasted, and a high `oldest_entry_days` means what is injected is stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryShape {
    /// Memory files excluding `MEMORY.md`, capped by the scanner's own limit.
    pub(crate) entries: usize,
    /// Index lines before truncation.
    pub(crate) index_lines: usize,
    /// Index bytes before truncation.
    pub(crate) index_bytes: usize,
    /// Whether the index exceeded its budget and was cut down for the prompt.
    pub(crate) index_truncated: bool,
    /// Age of the least recently touched memory, or `None` when there are none.
    pub(crate) oldest_entry_days: Option<i64>,
}

/// Measure the memory directory without reading any memory bodies.
pub(crate) fn memory_shape(memory_dir: &Path) -> MemoryShape {
    let headers = scan_memory_files(memory_dir).unwrap_or_default();
    let now = Utc::now();
    // The scan sorts newest first, so the tail is the stalest entry.
    let oldest_entry_days = headers.last().map(|header| memory_age_days(header.mtime, now));
    let index = truncate_index(&read_index(&memory_entrypoint(memory_dir)));

    MemoryShape {
        entries: headers.len(),
        index_lines: index.line_count,
        index_bytes: index.byte_count,
        index_truncated: index.was_truncated,
        oldest_entry_days,
    }
}

/// Record the memory shape once per session.
///
/// Shape only — filenames, descriptions, and bodies never reach the log.
pub(crate) fn log_memory_shape(memory_dir: Option<&Path>) {
    let Some(memory_dir) = memory_dir else {
        tracing::debug!(target: "agentrs_memory", "memory disabled for this session");
        return;
    };
    let shape = memory_shape(memory_dir);
    tracing::debug!(
        target: "agentrs_memory",
        entries = shape.entries,
        index_lines = shape.index_lines,
        index_bytes = shape.index_bytes,
        index_truncated = shape.index_truncated,
        oldest_entry_days = shape.oldest_entry_days,
        "memory shape at session start",
    );
}

/// Count memory files the model read in one tool round.
///
/// Paired with the session-start entry count, this answers the question the
/// whole memory system rests on: does the model actually reach for what it
/// stored? A count of zero is recorded as a non-event — the ratio needs a
/// denominator, so silence here is itself the signal.
pub(crate) fn record_memory_reads(tool_calls: &[ContentBlock], memory_dir: Option<&Path>) {
    let Some(memory_dir) = memory_dir else { return };
    let reads = tool_calls
        .iter()
        .filter(|call| {
            let ContentBlock::ToolUse { name, input, .. } = call else {
                return false;
            };
            name == "Read"
                && input
                    .get("file_path")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|path| is_memory_path(Path::new(path), memory_dir))
        })
        .count();
    tracing::debug!(target: "agentrs_memory", reads, "memory files read in tool round");
}

#[cfg(test)]
#[path = "telemetry_test.rs"]
mod telemetry_test;

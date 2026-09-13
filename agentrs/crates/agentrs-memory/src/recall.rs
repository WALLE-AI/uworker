// Relevance-based memory recall.
//
// The MEMORY.md index is capped at 200 lines / 25 KB, which bounds how much
// memory a project can accumulate before the prompt stops describing all of
// it. Recall lifts that ceiling: instead of injecting the whole index, a side
// query picks the few files that matter for the current turn.
//
// The selector trait lives here rather than in the agent crate so the pure
// candidate-filtering pipeline can sit beside it. The LLM-backed
// implementation lives in `agentrs-agent`, mirroring the
// `TextSummarizer` / `ProviderSummarizer` split.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::age::{memory_age, memory_freshness_note};
use crate::store::{format_memory_manifest, scan_memory_files};
use crate::types::MemoryHeader;

/// Everything a selector needs to rank one manifest.
pub struct SelectionRequest<'a> {
    /// The user's message for this turn.
    pub query: &'a str,
    /// Candidate listing produced by [`format_memory_manifest`].
    pub manifest: &'a str,
    /// Tools used in the preceding round.
    ///
    /// Reference material for a tool already in play is redundant — the
    /// conversation holds working examples. Warnings about that same tool are
    /// not: being mid-use is exactly when a gotcha matters most.
    pub recent_tools: &'a [String],
    /// Upper bound on returned filenames.
    pub limit: usize,
}

/// Picks the memories most relevant to a query out of a candidate manifest.
#[async_trait]
pub trait MemorySelector: Send + Sync {
    /// Return the filenames to surface, at most `limit`.
    ///
    /// Implementations must return an empty vec on failure rather than
    /// erroring: recall is best-effort context, never a hard dependency of the
    /// turn it decorates.
    async fn select(&self, request: SelectionRequest<'_>) -> Vec<String>;
}

/// Inputs to one recall pass.
pub struct RecallRequest<'a> {
    pub query: &'a str,
    pub memory_dir: &'a Path,
    pub recent_tools: &'a [String],
    /// Files already injected earlier in this session.
    pub already_surfaced: &'a HashSet<PathBuf>,
    pub limit: usize,
}

/// Scan, filter, select, and resolve — the full recall pipeline.
///
/// Returns the selected headers in the order the selector ranked them. An
/// empty result is normal and means "inject nothing this turn".
pub async fn find_relevant_memories(request: RecallRequest<'_>, selector: &dyn MemorySelector) -> Vec<MemoryHeader> {
    if request.limit == 0 {
        return Vec::new();
    }
    let Ok(headers) = scan_memory_files(request.memory_dir) else {
        return Vec::new();
    };

    // Drop already-injected files *before* the selector runs, not after. A
    // Top-N budget spent on files the model can already see buys nothing; the
    // point is to surface what it has not seen yet.
    let candidates: Vec<MemoryHeader> = headers
        .into_iter()
        .filter(|header| !request.already_surfaced.contains(&header.file_path))
        .collect();
    if candidates.is_empty() {
        tracing::debug!(target: "agentrs_memory", candidates = 0, selected = 0, "memory recall skipped");
        return Vec::new();
    }

    let manifest = format_memory_manifest(&candidates);
    let chosen = selector
        .select(SelectionRequest {
            query: request.query,
            manifest: &manifest,
            recent_tools: request.recent_tools,
            limit: request.limit,
        })
        .await;

    let selected = resolve_selection(&candidates, &chosen, request.limit);
    // Empty selections are recorded too: a selection rate needs a denominator.
    tracing::debug!(
        target: "agentrs_memory",
        candidates = candidates.len(),
        returned = chosen.len(),
        selected = selected.len(),
        "memory recall completed",
    );
    selected
}

/// Map selector output back onto real candidates.
///
/// Filenames the selector invented are dropped rather than reported: a
/// hallucinated name is a miss, not an error, and recall degrades to injecting
/// less rather than failing the turn.
fn resolve_selection(candidates: &[MemoryHeader], chosen: &[String], limit: usize) -> Vec<MemoryHeader> {
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for name in chosen {
        let name = name.trim();
        if !seen.insert(name.to_owned()) {
            continue;
        }
        if let Some(header) = candidates.iter().find(|candidate| candidate.filename == name) {
            selected.push(header.clone());
        }
        if selected.len() == limit {
            break;
        }
    }
    selected
}

/// Render recalled memories as a prompt block.
///
/// Each entry is stamped with its age and, once stale, a caveat: models do not
/// do date arithmetic well, so an ISO timestamp would not trigger the
/// staleness reasoning that "47 days ago" does.
///
/// Returns `None` when nothing was recalled or every file failed to read.
pub fn render_recalled_memories(headers: &[MemoryHeader]) -> Option<String> {
    render_recalled_memories_at(headers, Utc::now())
}

fn render_recalled_memories_at(headers: &[MemoryHeader], now: DateTime<Utc>) -> Option<String> {
    let mut sections = Vec::new();
    for header in headers {
        let Ok(body) = fs::read_to_string(&header.file_path) else {
            // The file was deleted between the scan and now. Skipping it is
            // the whole recovery: the turn proceeds without that memory.
            continue;
        };
        let age = memory_age(header.mtime, now);
        let mut section = format!("## {} (updated {age})\n", header.filename);
        if let Some(note) = memory_freshness_note(header.mtime, now) {
            section.push_str(&note);
            section.push('\n');
        }
        section.push_str(body.trim());
        sections.push(section);
    }
    if sections.is_empty() {
        return None;
    }
    Some(format!(
        "<system-reminder>\nRelevant memories for this query:\n\n{}\n</system-reminder>",
        sections.join("\n\n")
    ))
}

#[cfg(test)]
#[path = "recall_test.rs"]
mod recall_test;

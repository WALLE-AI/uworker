// Ported from aionrs (Apache-2.0).
//   Source: crates/aion-types/src/file_state.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 逐字复制，仅改 crate 路径；文档待二次优化补齐。

#![allow(missing_docs, reason = "aionrs 逐字移植，文档待二次优化补齐")]

/// Cached state of a file that the model has seen.
///
/// Stored in an LRU cache keyed by normalized file path.
/// Used by Read/Edit/Write tools for dedup detection and staleness checks.
#[derive(Debug, Clone)]
pub struct FileState {
    /// File content as seen by the model (with line numbers).
    pub content: String,
    /// File modification time when last read (milliseconds since UNIX epoch).
    pub mtime_ms: u64,
    /// Line offset of partial read (None = full read).
    pub offset: Option<usize>,
    /// Line limit of partial read (None = full read).
    pub limit: Option<usize>,
}

impl FileState {
    /// Byte size of the cached content (used for cache size accounting).
    pub fn content_bytes(&self) -> usize {
        self.content.len()
    }
}

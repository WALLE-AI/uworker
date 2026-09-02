// Ported from aionrs (Apache-2.0), crates/aion-types.
//   Source: crates/aion-types/src/compact.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: crate 路径改写

use serde::{Deserialize, Serialize};

/// How a compaction was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger {
    /// Triggered automatically when token usage exceeded the context threshold.
    Auto,
    /// Triggered manually by the user (e.g. `/compact` command).
    Manual,
}

/// Metadata stored in the compact boundary marker message.
///
/// After an autocompact or manual compact, a system-role message is
/// inserted whose content carries this metadata serialized as JSON.
/// It records *what happened* so that downstream code (and the model
/// itself) can reason about the compaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactMetadata {
    /// How this compaction was triggered.
    pub trigger: CompactTrigger,
    /// Input token count reported by the API *before* compaction.
    pub pre_compact_tokens: u64,
    /// Number of conversation messages that were summarized.
    pub messages_summarized: usize,
}


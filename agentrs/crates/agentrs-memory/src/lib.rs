//! AgentRS `memory` —— 记忆选择（架构 §9.2）。
//!
//! ```text
//! MemoryRetriever.candidates  →  memorySelector  →  最多 N 条  →  load
//!        （Core 的索引与权限）      （内核的选择）
//! ```
//!
//! **AgentRS 不管 SQLite FTS、不管 Markdown 文件。** 索引、权限、保留策略
//! 全归 Core；内核只从 Core 交来的候选里挑几条。这个切法让可见范围由 Core
//! 单方面决定——selector 再怎么写错，也变不出一条 Core 没给它的记忆。
//!
//! - ✅ [`selector`] 选择、校验与排名降级
//! - ✅ [`index`] `MEMORY.md` 索引的裁剪（自 aionrs 移植的纯逻辑那半）
//! - ⬜ 记忆写入路径与索引读写（**归 Core**）

#![forbid(unsafe_code)]

pub mod index;
pub mod selector;

pub use index::{truncate_index, IndexTruncation, MAX_INDEX_BYTES, MAX_INDEX_LINES};
pub use selector::{by_rank, decide, parse_output, Selection, SelectionSource, DEFAULT_LIMIT};

//! AgentRS `observability` —— Trajectory / Projection / Replay。
//!
//! - [`projection`]：版本化 Projection Registry 与 Phase B 首批投影（§4.4）
//! - [`redact`]：脱敏导出与 replay bundle（§16，M2 出口标准）
//!
//! trace/metrics 按迭代计划后续填充。
//!
//! **投影不是第二份事实源**——只读 Event Ledger，纯同步 fold，不写回。

#![forbid(unsafe_code)]

pub mod projection;
pub mod redact;

pub use redact::{BundleSalt, Leak, LeakKind, Redactor, ReplayBundle, Rule};

pub use projection::{
    page, payload_kind, record_key, CacheInvalidation, CacheVerdict, Cursor, EventFilter, Page,
    ProjectionDefinition, ProjectionError, ProjectionKey, ProjectionRegistry, Snapshot, SnapshotCache,
    ToolPath,
};

//! 内容寻址存储契约（架构 §4.2）。
//!
//! `ContentRef` 是模型可见内容的**两个物理来源之一**（另一个是 durable event ledger）。
//! 任何"只存在于内存里、下一轮还要发给模型"的状态都是设计缺陷——它会在崩溃恢复后
//! 凭空消失，使可重建标准失效。
//!
//! 职责切点：**内核拿 ref 语义与 liveness，Core 拿存储、加密、GC、配额**（架构 §1.1）。

use serde::{Deserialize, Serialize};

use crate::ids::{Digest, EventSequence, RunId, Timestamp};

/// 内容的可见范围。相同字节在同一 scope 内必然产生相同 [`ContentRef`]。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ContentScope {
    /// 仅本 Run 可见。子 Run 产出的证据写入父 Run 的 Run scope。
    Run {
        /// 所属 Run。
        run_id: RunId,
    },
    /// 工作区内可见，可跨 Run 复用。
    Workspace {
        /// 工作区标识。
        workspace_id: String,
    },
    /// 全局可见（如内置提示词、schema）。
    Global,
}

/// 内容引用。不可变、按内容寻址。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentRef {
    /// 内容摘要。
    pub digest: Digest,
    /// 字节长度。
    pub len: u64,
    /// 编码/媒体类型，例如 `text/plain; charset=utf-8`。
    pub media_type: String,
    /// 可见范围。
    pub scope: ContentScope,
}

/// 写入时附带的元信息。内核不解释其语义，仅透传给存储实现。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentMeta {
    /// 媒体类型。
    pub media_type: Option<String>,
    /// 产生该内容的 durable 事件序号，用于溯源。
    pub source_seq: Option<EventSequence>,
    /// 自由标签，供 Core 的存储策略使用（如冷热分层）。
    #[serde(default)]
    pub tags: Vec<String>,
}

/// 内容元数据查询结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentStat {
    /// 字节长度。
    pub len: u64,
    /// 媒体类型。
    pub media_type: String,
    /// 写入时刻。
    pub created_at: Timestamp,
}

/// 字节区间，闭开。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    /// 起始偏移（含）。
    pub start: u64,
    /// 结束偏移（不含）。
    pub end: u64,
}

/// 保留声明的持有者。
///
/// checkpoint 保存前必须对其引用的全部 ref 调用 `retain`；Run 归档后由 Core `release`。
/// **内核不实现 GC，但必须保证不产生未 retain 的悬空引用。**
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RetentionOwner {
    /// 某个 Run 的某个 checkpoint。
    Checkpoint {
        /// Run 标识。
        run_id: RunId,
        /// checkpoint 覆盖到的序号。
        up_to_seq: EventSequence,
    },
    /// 整个 Run 存续期间。
    Run {
        /// Run 标识。
        run_id: RunId,
    },
}

/// 内容存储的错误。
#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    /// 内容已不存在（多半已被 GC）。降级为占位摘要，见 [`UnresolvedReason`]。
    #[error("content not found: {digest}")]
    NotFound {
        /// 缺失内容的摘要。
        digest: Digest,
    },
    /// 可见性收窄导致不可读。**必须直接剔除，不得向模型暴露其存在性。**
    #[error("content forbidden")]
    Forbidden,
    /// 传输或后端错误，按可重试处理。
    #[error("content backend error: {message}")]
    Backend {
        /// 已脱敏的错误描述。
        message: String,
    },
}

/// 解引用失败时的降级原因。任何降级都必须进入 `ModelRequestManifest`，
/// 使"这次请求为什么和上次不同"可解释（架构 §4.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum UnresolvedReason {
    /// 已过期：降级为占位摘要，保留原长度与来源。
    Expired {
        /// 原始字节长度。
        original_len: u64,
    },
    /// 无权访问：直接剔除，不降级，不暴露存在性。
    Forbidden,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 样例_ref(scope: ContentScope) -> ContentRef {
        ContentRef {
            digest: Digest::from_hex("abc123"),
            len: 42,
            media_type: "text/plain".into(),
            scope,
        }
    }

    #[test]
    fn 相同字节不同_scope_是不同的_ref() {
        let a = 样例_ref(ContentScope::Global);
        let b = 样例_ref(ContentScope::Run { run_id: "r1".into() });
        assert_ne!(a, b, "scope 参与 ref 身份，否则跨 Run 可见性会被绕过");
    }

    #[test]
    fn retention_owner_可序列化且带判别标签() {
        let owner = RetentionOwner::Checkpoint {
            run_id: "r1".into(),
            up_to_seq: EventSequence(7),
        };
        let json = serde_json::to_string(&owner).unwrap();
        assert!(json.contains("\"kind\":\"checkpoint\""), "{json}");
    }

    #[test]
    fn forbidden_降级不携带任何内容信息() {
        // Forbidden 变体没有字段，从类型上就无法泄漏长度等旁路信息。
        let json = serde_json::to_string(&UnresolvedReason::Forbidden).unwrap();
        assert_eq!(json, "{\"reason\":\"forbidden\"}");
    }
}

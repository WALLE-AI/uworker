//! ModelSurface：模型历史是 log 的投影，不是第二份可变状态（架构 §4.3.1）。
//!
//! **规则：durable log 永远 append-only；模型看到的历史是 log 上一个叫 Surface 的有序投影。**
//!
//! ```text
//! durable log            [e1][e2][e3][e4][e5][e6][e7][e8]   ← 永远只追加
//!                         ↓   ↓        ↓        ↓
//! Surface（append-origin）[m1][m2]    [m3]     [m4]
//!                                      └── Replace(range=m1..m3) ──┐
//!                                                                   ↓
//! 模型看到的历史          [摘要节点][m4]
//! 人类看到的 transcript   [m1][m2][m3][m4]   ← 用 append-origin，不受遮蔽
//! ```
//!
//! 四条推论，每条都替代了一段专门设计：
//!
//! - 压缩 = 追加一个 `Replace` 节点遮蔽一段 range，历史从不被改写；
//! - 人类 transcript 用 append-origin 事件，压缩不会抹掉用户已看到的对话；
//! - 任何请求可由「log 前缀 + 同一个 fold 函数」精确重建；
//! - 缓存前缀失效点是可精确计算的 `range.start`，不需要靠约定近似。

use serde::{Deserialize, Serialize};

use crate::ids::EventSequence;

/// Surface 上的位置区间，闭开。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceRange {
    /// 起始位置（含）。缓存失效点即此值。
    pub start: EventSequence,
    /// 结束位置（不含）。
    pub end: EventSequence,
}

/// 替换代际。用于判定一次压缩是否**确实推进了状态**。
///
/// 溢出触发的压缩之后，只有当代际前进时才允许开启新的重试 Turn；
/// 否则原始请求错误保持权威。这杜绝了"压缩没压下去 → 再请求 → 再溢出"的无限循环
/// （架构 §9.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SurfaceGeneration(pub u64);

impl SurfaceGeneration {
    /// 推进一代。
    pub fn advance(self) -> Self {
        Self(self.0 + 1)
    }
}

/// 一个 Surface 事件相对 Surface 的操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum SurfaceOp {
    /// 在 Surface 尾部追加，且自身从不是替换副本。
    ///
    /// **append-origin 事件是人类 transcript 的来源**——replacement 副本是 model-only，
    /// 用它渲染 transcript 会抹掉用户已经看到的对话。
    Append,
    /// 遮蔽一段已有 Surface range，用于压缩。log 本身不被改写。
    Replace {
        /// 被遮蔽的区间。
        range: SurfaceRange,
        /// 替换代际。
        generation: SurfaceGeneration,
    },
}

impl SurfaceOp {
    /// 是否为 append-origin（人类 transcript 的来源）。
    pub fn is_append_origin(&self) -> bool {
        matches!(self, Self::Append)
    }

    /// 该操作导致的缓存前缀失效点。`Append` 不使前缀失效。
    pub fn cache_invalidation_point(&self) -> Option<EventSequence> {
        match self {
            Self::Append => None,
            Self::Replace { range, .. } => Some(range.start),
        }
    }
}

/// 能够进入 Surface 的事件种类。**只有三种。**
///
/// 新增一种模型可见内容 = 新增一个 Surface 事件类型 + 扩展投影规则，
/// 不允许有旁路（内核不变量 11、13）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceEventKind {
    /// 用户消息：直接输入、steering 注入、`ExternalFact` 收件。
    UserMessage,
    /// 助手消息。
    AssistantMessage,
    /// 工具结果。
    ToolResult,
    /// 经 ContentStore 解引用并持久化来源引用的上下文。
    ContextAttached,
}

impl SurfaceEventKind {
    /// 全部变体，供投影器穷举。
    pub const ALL: [SurfaceEventKind; 4] = [
        SurfaceEventKind::UserMessage,
        SurfaceEventKind::AssistantMessage,
        SurfaceEventKind::ToolResult,
        SurfaceEventKind::ContextAttached,
    ];
}

/// Surface 事件的标记，附着在 durable 事件 envelope 上。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceMarker {
    /// 事件种类。
    pub kind: SurfaceEventKind,
    /// 相对 Surface 的操作。
    pub op: SurfaceOp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_不使缓存前缀失效() {
        assert_eq!(SurfaceOp::Append.cache_invalidation_point(), None);
    }

    #[test]
    fn replace_的失效点就是_range_起点() {
        let op = SurfaceOp::Replace {
            range: SurfaceRange {
                start: EventSequence(10),
                end: EventSequence(20),
            },
            generation: SurfaceGeneration(1),
        };
        assert_eq!(op.cache_invalidation_point(), Some(EventSequence(10)));
    }

    #[test]
    fn 只有_append_是人类_transcript_来源() {
        assert!(SurfaceOp::Append.is_append_origin());
        let replace = SurfaceOp::Replace {
            range: SurfaceRange {
                start: EventSequence(0),
                end: EventSequence(1),
            },
            generation: SurfaceGeneration(0),
        };
        assert!(!replace.is_append_origin(), "replacement 副本是 model-only");
    }

    #[test]
    fn 代际推进可用于判定压缩是否有效() {
        let g = SurfaceGeneration(3);
        assert!(g.advance() > g, "无法推进代际时不得开启重试 Turn");
    }

    #[test]
    fn surface_种类恰好四种() {
        assert_eq!(
            SurfaceEventKind::ALL.len(),
            4,
            "新增种类必须同步更新投影规则与本断言"
        );
    }
}

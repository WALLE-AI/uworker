//! 模型请求清单与缓存前缀（架构 §4.1、§9.1.1）。
//!
//! 每次请求都必须能由 `RunSpec + durable events + ContentStore refs` 重建。
//! manifest 是这条保证的落地物：它记录了**这次请求由什么构成**，而不是内容本身。

use serde::{Deserialize, Serialize};

use crate::authority::{CapabilityViewDigest, PermissionMode};
use crate::content::{ContentRef, UnresolvedReason};
use crate::ids::{AuthorityEnvelopeId, Digest, EventRange, MemoryId, RequestId, SkillId, ToolCallId};

/// 一次 operation 冻结的依赖视图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationView {
    /// 授权信封标识。
    pub authority_id: AuthorityEnvelopeId,
    /// 权限模式。
    pub permission_mode: PermissionMode,
    /// 能力视图摘要。**一次 operation 内不变**——这是 P0 的唯一依赖视图不变式。
    pub capability_digest: CapabilityViewDigest,
    // P1 追加：provider / tool_catalog / middleware / prompt 各自的 Generation
}

/// 缓存分段索引。稳定前缀为 S0–S2，可变段为 S3–S4（架构 §9.1.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheSegment {
    /// S0 稳定系统规则 + PermissionMode 段。
    S0SystemRules,
    /// S1 工具目录（含已加载的 deferred schema）。**只能追加，不得重排。**
    S1ToolCatalog,
    /// S2 ModelSurface 投影。稳定性是 Surface 的推论，不需额外规则。
    S2Surface,
    /// S3 精选记忆 / skill fragment / 文件工作集。每轮可变。
    S3Selected,
    /// S4 近期工具结果 + 当前 claim 的用户输入。每轮可变。
    S4Recent,
}

impl CacheSegment {
    /// 是否属于稳定前缀（参与 `cache_prefix_digest`）。
    pub fn is_stable_prefix(&self) -> bool {
        matches!(self, Self::S0SystemRules | Self::S1ToolCatalog | Self::S2Surface)
    }
}

/// 缓存断裂的归因。每次 miss 都必须能落到其中之一。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheBreakCause {
    /// 首次请求，无前缀可命中。
    FirstRequest,
    /// 系统提示变化。
    SystemPromptChanged,
    /// 工具目录变化。
    ToolCatalogChanged,
    /// 历史被 `Replace` 节点遮蔽。
    HistoryRewritten,
    /// 权限模式切换。
    PermissionModeChanged,
    /// provider 切换（含授权内 fallback）。
    ProviderSwitched,
    /// steering 注入。
    SteeringInjected,
    /// 服务端缓存过期。
    TtlExpiry,
}

/// 历史合法化操作（架构 §8.1）。
///
/// durable 事实流与"provider 当下能接受的请求"之间存在不可消除的阻抗：
/// reasoning 签名会失效、`tool_use` 可能残缺、各厂商对 block 类型的约束不同。
///
/// `HistoryLegalization` 是**唯一允许对事实做投影期修复的位置**，且必须留痕。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum LegalizationOp {
    /// 为残缺的 `tool_use` 补一个合成结果。**必须与真实结果在 trajectory 上可区分。**
    SyntheticToolResult {
        /// 对应的调用 id。
        tool_use_id: ToolCallId,
        /// 原因。
        reason: LegalizationReason,
    },
    /// 丢弃失效的 reasoning 签名。
    DroppedReasoningSignature {
        /// 消息下标。
        message_index: usize,
    },
    /// 丢弃目标模型不支持的 block（如图像）。
    DroppedUnsupportedBlock {
        /// 消息下标。
        message_index: usize,
        /// block 类型。
        kind: String,
    },
    /// 合并相邻同角色消息。
    MergedAdjacentMessages {
        /// 起始下标。
        start: usize,
        /// 结束下标。
        end: usize,
    },
    /// 丢弃孤儿 `tool_result`——它引用的 `tool_use` 已不在历史里。
    ///
    /// 与 [`SyntheticToolResult`](Self::SyntheticToolResult) 是**同一问题的两面**：
    /// 前者补缺的结果，后者删多余的结果。压缩截断或从中途分叉都会造出孤儿结果，
    /// 而 Anthropic 族对"引用了不存在的 tool_use"直接 400。
    DroppedOrphanToolResult {
        /// 被丢弃的结果引用的调用 id。
        tool_use_id: ToolCallId,
    },
    /// 丢弃合法化之后内容为空的消息。
    ///
    /// 块级过滤可能把一条消息掏空。**空消息多数端点直接拒收**，
    /// 且它对模型没有任何信息量。
    DroppedEmptyMessage {
        /// 消息下标。
        message_index: usize,
    },
    /// 重写工具调用 id 以满足厂商格式。
    RewrittenToolCallId {
        /// 原 id。
        from: ToolCallId,
        /// 新 id。
        to: ToolCallId,
    },
}

/// 需要合法化的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegalizationReason {
    /// 执行被中断，结果未知。
    Interrupted,
    /// 崩溃导致历史残缺。
    CrashTruncated,
    /// 取消导致未 settlement。
    Canceled,
    /// 历史被压缩，`Replace` 节点遮蔽了原始区间。
    Compacted,
}

/// token 账本。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAccounting {
    /// 保守估算的输入 token（内核内置估算器，只高估不低估）。
    pub estimated_input: u64,
    /// 精确输入 token（`TokenCounter` port 提供，可缺失）。
    pub exact_input: Option<u64>,
    /// 输出预留。
    pub reserved_output: u64,
    /// 命中缓存的输入 token。
    pub cache_read: Option<u64>,
    /// 写入缓存的输入 token。
    pub cache_creation: Option<u64>,
}

/// 一次模型请求的完整构成说明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequestManifest {
    /// 请求标识。
    pub request_id: RequestId,
    /// 冻结的依赖视图。
    pub operation_view: OperationView,
    /// 本次请求覆盖的 durable 事件区间。
    pub source_event_range: EventRange,
    /// 系统段内容引用。
    #[serde(default)]
    pub system_sections: Vec<ContentRef>,
    /// 选中的记忆片段。
    #[serde(default)]
    pub memory_fragments: Vec<MemoryId>,
    /// 启用的技能片段。
    #[serde(default)]
    pub skill_fragments: Vec<SkillId>,
    /// 压缩摘要引用。
    #[serde(default)]
    pub compaction_refs: Vec<ContentRef>,
    /// 工具目录摘要。
    pub tool_catalog_digest: Digest,
    /// 稳定前缀摘要（覆盖 S0–S2）。
    pub cache_prefix_digest: Digest,
    /// 断点位置。
    #[serde(default)]
    pub cache_breakpoints: Vec<CacheSegment>,
    /// 本次执行的合法化操作。**留痕使"这次为什么和上次不同"可解释。**
    #[serde(default)]
    pub legalization_ops: Vec<LegalizationOp>,
    /// 解引用失败导致的降级。同样必须留痕。
    #[serde(default)]
    pub unresolved: Vec<UnresolvedReason>,
    /// token 账本。
    pub token_accounting: TokenAccounting,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 稳定前缀恰好是_s0_到_s2() {
        assert!(CacheSegment::S0SystemRules.is_stable_prefix());
        assert!(CacheSegment::S1ToolCatalog.is_stable_prefix());
        assert!(CacheSegment::S2Surface.is_stable_prefix());
        assert!(!CacheSegment::S3Selected.is_stable_prefix());
        assert!(!CacheSegment::S4Recent.is_stable_prefix());
    }

    #[test]
    fn 段序等于拼装顺序() {
        let mut segs = [
            CacheSegment::S4Recent,
            CacheSegment::S0SystemRules,
            CacheSegment::S2Surface,
        ];
        segs.sort();
        assert_eq!(
            segs[0],
            CacheSegment::S0SystemRules,
            "排序即前缀顺序，供装配器直接使用"
        );
    }

    #[test]
    fn 合法化操作可_round_trip() {
        let op = LegalizationOp::SyntheticToolResult {
            tool_use_id: "tc1".into(),
            reason: LegalizationReason::Interrupted,
        };
        let json = serde_json::to_string(&op).unwrap();
        let back: LegalizationOp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, op);
    }

    #[test]
    fn 账本区分保守估算与精确计数() {
        // 二者职责不同：估算器保证内核能独立防超窗，port 保证成本数字准确。
        let t = TokenAccounting {
            estimated_input: 1000,
            exact_input: Some(963),
            ..Default::default()
        };
        assert!(t.estimated_input >= t.exact_input.unwrap(), "估算器只允许高估");
    }
}

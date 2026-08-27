// Ported from aionrs (Apache-2.0).
//   Source: aionrs/crates/aion-types/src/llm.rs @ a5df989d110fb424bcd496b413e7ce7e20754414
//   Copied: 2026-08-25   Modified: yes
//   Changes:
//     - ToolUseId 改为 contracts::ToolCallId；model 改为 contracts::ModelId。
//     - LlmRequest 增加 request_id 与 cache_prefix_digest，使流事件能回指
//       ModelRequestManifest（架构 §4.1 的可重建要求）。
//     - LlmEvent 增加 Usage 变体：vLLM 等 OpenAI 兼容端点在末帧单独下发用量。

//! provider 无关的请求与流事件模型。
//!
//! 这一层是 LLM seam 的**词汇表**：`ProviderPort` 的签名用它表达，
//! 各厂商适配器负责把它投影为自家 wire format。厂商差异集中在
//! `ProviderCompat` 数据表与投影器里，**绝不进主循环分支**。

use serde_json::Value;

use agentrs_contracts::ids::{Digest, ModelId, RequestId, ToolCallId};

use crate::message::{Message, StopReason, TokenUsage};

/// 一次模型请求。
#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    /// 请求标识，与 `ModelRequestManifest` 一一对应。
    pub request_id: RequestId,
    /// 目标模型。档位到 id 的映射归 Core。
    pub model: ModelId,
    /// 系统段。
    pub system: String,
    /// 消息序列。**必须来自 `derive_messages` 的投影**，不得另行构造。
    pub messages: Vec<Message>,
    /// 模型可见的工具目录。
    ///
    /// **只追加不重排**——目录是缓存前缀 S1 段，重排会让前缀失效（架构 §9.1.1）。
    pub tools: Vec<crate::tool::ToolDef>,
    /// 输出上限。
    pub max_tokens: Option<u32>,
    /// 扩展思考配置。
    pub thinking: Option<ThinkingConfig>,
    /// reasoning 强度（OpenAI 系）。
    pub reasoning_effort: Option<String>,
    /// 稳定前缀摘要，供缓存归因比对。
    pub cache_prefix_digest: Option<Digest>,
}

/// 扩展思考配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingConfig {
    /// 启用，并给出预算。
    Enabled {
        /// 思考 token 预算。
        budget_tokens: u32,
    },
    /// 关闭。
    Disabled,
}

/// 来自模型的流事件。
#[derive(Debug, Clone, PartialEq)]
pub enum LlmEvent {
    /// 文本增量。**live，可丢**——committed 内容以 `AssistantMessage` 事件为准。
    TextDelta(String),
    /// 工具调用的流式增量。
    ///
    /// OpenAI 兼容端点把一次调用切成多帧：首帧带 `id`/`name`，
    /// 后续帧只带参数片段。由 provider 层累积成 [`LlmEvent::ToolUse`] 后才对上层可见。
    ToolCallDelta {
        /// 同一响应内的调用序号。
        index: usize,
        /// 调用 id（通常只在首帧出现）。
        id: Option<String>,
        /// 工具名（通常只在首帧出现）。
        name: Option<String>,
        /// 参数片段，需要跨帧拼接。
        arguments: String,
    },
    /// 完整的工具调用（流式增量累积完成后发出）。
    ToolUse {
        /// 调用标识。
        id: ToolCallId,
        /// 工具名。
        name: String,
        /// 参数。
        input: Value,
        /// provider 私有元数据，原样 round-trip。
        extra: Option<Value>,
    },
    /// 思考增量。
    ThinkingDelta(String),
    /// 当前思考块的 provider 签名。**有生命周期**，失效时由 HistoryLegalization 丢弃。
    ThinkingSignature(String),
    /// 不透明的 provider 输出项，必须持久化并重放。
    ProviderItem {
        /// 归属 provider。
        provider: String,
        /// 原样载荷。
        item: Value,
    },
    /// 用量。部分 OpenAI 兼容端点（如 vLLM）在末帧单独下发。
    Usage(TokenUsage),
    /// 响应结束。
    Done {
        /// 停止原因。
        stop_reason: StopReason,
        /// 累计用量。
        usage: TokenUsage,
    },
    /// 来自 API 的错误。**正文已脱敏**，不透传响应体与密钥。
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 文本增量是_live_的而结束事件携带用量() {
        let e = LlmEvent::TextDelta("a".into());
        assert!(matches!(e, LlmEvent::TextDelta(_)));

        let d = LlmEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 2,
                ..Default::default()
            },
        };
        match d {
            LlmEvent::Done { usage, .. } => assert_eq!(usage.input_tokens, 10),
            _ => unreachable!(),
        }
    }

    #[test]
    fn 请求可携带缓存前缀摘要以便归因() {
        let r = LlmRequest {
            request_id: "req1".into(),
            model: "m".into(),
            system: String::new(),
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            thinking: None,
            reasoning_effort: None,
            cache_prefix_digest: Some(Digest::from_hex("abc")),
        };
        assert!(r.cache_prefix_digest.is_some());
    }
}

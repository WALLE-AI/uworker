//! 函数式子 Agent（架构 §11.1）。
//!
//! ```text
//! 输入 → 独立受限上下文 → schema 化结果
//! ```
//!
//! `Explore`、`Plan`、`memorySelector`、`ToolSearch`、`compact`、`contextSummary`
//! 都属于这一类。它们**没有持久身份、默认零工具、不能发消息、不能建命令**。
//!
//! ## 主 Agent 只拿结论，不拿过程
//!
//! 过程推理、草稿、token 流与原始上下文**不自动回灌**。理由不是省钱，
//! 是注意力：把子 Agent 的思考过程灌回主上下文，等于让主 Agent 在
//! 一堆它没参与的推理里找结论，效果比只给结论差。
//!
//! 这条在类型上表达为：[`run`] 只返回 [`SubagentOutput`]，
//! **没有任何通道能把中间态带出来**。
//!
//! ## 零工具是结构保证，不是约定
//!
//! [`build_request`] 恒把 `tools` 置空。子 Agent 想调工具在类型上就做不到——
//! 它拿不到 `ToolRoundDeps`，也没有地方能塞进去。

use agentrs_types::{ContentBlock, LlmEvent, LlmRequest, Message, Role};

/// 一次子 Agent 调用的输入。
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentInput {
    /// 任务描述，进系统段。
    pub task: String,
    /// **独立受限上下文**：只有这里给的内容，没有父 Run 的其余历史。
    pub context: Vec<Message>,
    /// 使用的模型。
    pub model: agentrs_contracts::ids::ModelId,
    /// 输出上限。
    pub max_tokens: Option<u32>,
}

/// 子 Agent 的产出。
///
/// **这是主 Agent 能拿到的全部**。没有过程推理、没有草稿、没有原始上下文。
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentOutput {
    /// 结论正文。
    pub conclusion: String,
    /// 产出的 token 用量，用于成本归集。
    pub usage: agentrs_types::TokenUsage,
}

/// 子 Agent 调用失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubagentError {
    /// 模型调用失败。已脱敏。
    #[error("subagent model call failed: {code}")]
    ModelFailed {
        /// 稳定错误码。
        code: String,
    },
    /// 模型返回了空结论。
    ///
    /// **必须当作失败而不是"空摘要"。** 调用方拿它去做压缩的话，
    /// 就等于把一段历史换成了空白——比不压缩糟得多。
    #[error("subagent produced no conclusion")]
    Empty,
    /// 产出不满足 schema 要求。
    #[error("subagent output missing required sections: {missing:?}")]
    SchemaViolation {
        /// 缺失的必填项。
        missing: Vec<String>,
    },
}

/// 把输入投影成一次**零工具**的模型请求。
///
/// 三条约束都在这里落地：零工具、独立上下文、有界输出。
pub fn build_request(
    input: &SubagentInput,
    request_id: agentrs_contracts::ids::RequestId,
    system: &str,
) -> LlmRequest {
    LlmRequest {
        request_id,
        model: input.model.clone(),
        system: if input.task.trim().is_empty() {
            system.to_string()
        } else {
            format!("{system}\n\n任务：{}", input.task)
        },
        // **独立受限上下文**：只有调用方明确交出来的那部分。
        messages: input.context.clone(),
        // **零工具**。不是"我们不打算给"，是这里根本不填。
        tools: Vec::new(),
        max_tokens: input.max_tokens,
        // 子 Agent 不做 reasoning round-trip：它的思考不回灌，
        // 带 thinking 只会让输出更长、更贵，却没人看。
        thinking: None,
        reasoning_effort: None,
        // 子 Agent 的上下文每次都不同，没有可复用前缀。
        cache_prefix_digest: None,
    }
}

/// 从流事件里收敛出结论。
///
/// 只取 `TextDelta` 与 `ToolUse` 之外的可见文本——**思考增量被丢弃**，
/// 这是"过程不回灌"在解析层的落点。
pub fn collect(events: &[LlmEvent]) -> Result<SubagentOutput, SubagentError> {
    let mut conclusion = String::new();
    let mut usage = agentrs_types::TokenUsage::default();

    for e in events {
        match e {
            LlmEvent::TextDelta(t) => conclusion.push_str(t),
            LlmEvent::Done { usage: u, .. } => usage = *u,
            LlmEvent::Usage(u) => usage = *u,
            LlmEvent::Error(code) => return Err(SubagentError::ModelFailed { code: code.clone() }),
            // 思考增量、签名、工具调用一律不进结论。
            // 子 Agent 本来就没有工具；真出现了说明模型在幻想，忽略即可。
            _ => {}
        }
    }

    if conclusion.trim().is_empty() {
        return Err(SubagentError::Empty);
    }
    Ok(SubagentOutput { conclusion, usage })
}

/// 把结论包成一条可进 Surface 的助手消息。
pub fn as_message(out: &SubagentOutput) -> Message {
    Message::new(Role::Assistant, vec![ContentBlock::text(out.conclusion.clone())])
}

#[cfg(test)]
mod tests {
    use agentrs_types::{StopReason, TokenUsage};

    use super::*;

    fn 输入() -> SubagentInput {
        SubagentInput {
            task: "总结".into(),
            context: vec![Message::new(Role::User, vec![ContentBlock::text("一些历史")])],
            model: "m".into(),
            max_tokens: Some(512),
        }
    }

    #[test]
    fn 请求里永远没有工具() {
        // 零工具是**结构保证**：这里根本不填，不是"我们不打算给"。
        let r = build_request(&输入(), "q".into(), "你是摘要器");
        assert!(r.tools.is_empty());
    }

    #[test]
    fn 上下文只有调用方交出来的那部分() {
        // 独立受限上下文——父 Run 的其余历史不会漏进来。
        let r = build_request(&输入(), "q".into(), "sys");
        assert_eq!(r.messages.len(), 1);
        assert_eq!(r.system, "sys\n\n任务：总结");
    }

    #[test]
    fn 不带_thinking() {
        // 子 Agent 的思考不回灌，带它只会让输出更长更贵却没人看。
        let r = build_request(&输入(), "q".into(), "sys");
        assert!(r.thinking.is_none());
        assert!(r.reasoning_effort.is_none());
    }

    #[test]
    fn 收敛只取可见文本() {
        let out = collect(&[
            LlmEvent::ThinkingDelta("我先想想".into()),
            LlmEvent::TextDelta("结论：".into()),
            LlmEvent::ThinkingSignature("sig".into()),
            LlmEvent::TextDelta("一切正常".into()),
            LlmEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                },
            },
        ])
        .unwrap();

        assert_eq!(out.conclusion, "结论：一切正常");
        assert!(!out.conclusion.contains("我先想想"), "思考过程漏进了结论");
        assert_eq!(out.usage.output_tokens, 5);
    }

    #[test]
    fn 空结论是失败而不是空字符串() {
        // 调用方拿它去压缩的话，等于把一段历史换成空白——比不压缩糟得多。
        assert_eq!(
            collect(&[LlmEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage::default(),
            }]),
            Err(SubagentError::Empty)
        );
    }

    #[test]
    fn 只有空白的结论也算空() {
        assert_eq!(
            collect(&[LlmEvent::TextDelta("   \n  ".into())]),
            Err(SubagentError::Empty)
        );
    }

    #[test]
    fn 错误事件透出稳定码不透出正文() {
        assert_eq!(
            collect(&[LlmEvent::Error("overloaded".into())]),
            Err(SubagentError::ModelFailed {
                code: "overloaded".into()
            })
        );
    }

    #[test]
    fn 幻想出来的工具调用被忽略() {
        // 子 Agent 本来就没有工具。模型硬要"调"一个，忽略即可——
        // 但不能因此把整次调用判为失败，正文可能是好的。
        let out = collect(&[
            LlmEvent::ToolUse {
                id: "c1".into(),
                name: "Read".into(),
                input: serde_json::json!({}),
                extra: None,
            },
            LlmEvent::TextDelta("正文".into()),
        ])
        .unwrap();
        assert_eq!(out.conclusion, "正文");
    }
}

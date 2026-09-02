// Ported from aionrs (Apache-2.0), crates/aion-agent.
//   Source: crates/aion-agent/src/compact/auto.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: crate 路径改写

//! Autocompact: context-threshold-triggered LLM summarization.
//!
//! When the best-known context size exceeds the configured threshold, this module
//! calls the LLM to produce a structured summary of the conversation,
//! then replaces the full history with a compact boundary marker and the
//! summary.  A circuit breaker prevents runaway retries.

use super::config::CompactConfig;

use agentrs_types::compact::{CompactMetadata, CompactTrigger};
use agentrs_types::llm::{LlmEvent, LlmRequest};
use agentrs_types::message::{ContentBlock, Message, Role, TokenUsage};

use super::prompt::{
    COMPACT_MAX_OUTPUT_TOKENS, COMPACT_SYSTEM_PROMPT, build_compact_prompt, build_summary_content,
    format_compact_summary,
};
use super::state::CompactState;

/// Maximum number of prompt-too-long retries.
const MAX_PTL_RETRIES: u32 = 2;

/// Content prefix for the compact boundary marker message.
pub const BOUNDARY_PREFIX: &str = "[Conversation compacted]";

// ── Public types ────────────────────────────────────────────────────────────

/// Result of a successful autocompact operation.
#[derive(Debug, Clone)]
pub struct CompactResult {
    /// Post-compact messages that replace the original conversation.
    /// Contains a boundary marker and a summary message.
    pub messages: Vec<Message>,
    /// How many original messages were summarized.
    pub messages_summarized: usize,
    /// Best-known context token count before compaction.
    pub pre_compact_tokens: u64,
}

/// Errors specific to autocompact.
#[derive(Debug, thiserror::Error)]
pub enum CompactError {
    #[error("LLM provider error: {0}")]
    Provider(SummarizeError),
    #[error("Prompt too long after {attempts} retries")]
    PromptTooLong { attempts: u32 },
    #[error("Empty response from LLM")]
    EmptyResponse,
    #[error("Stream error: {0}")]
    StreamError(String),
    #[error("Circuit breaker tripped after {failures} consecutive failures")]
    CircuitBroken { failures: u32 },
}

// ── Trigger check ───────────────────────────────────────────────────────────

/// Check if autocompact should trigger based on the best-known context size.
///
/// When `autocompact_threshold_pct` is set, threshold = context_window * pct / 100.
/// Otherwise falls back to: `threshold = context_window - output_reserve - autocompact_buffer`
pub fn should_autocompact(context_tokens: u64, config: &CompactConfig) -> bool {
    if !config.enabled {
        return false;
    }
    let threshold = if let Some(pct) = config.autocompact_threshold_pct {
        config.context_window * pct as usize / 100
    } else {
        let effective_window = config.context_window.saturating_sub(config.output_reserve);
        effective_window.saturating_sub(config.autocompact_buffer)
    };
    context_tokens as usize >= threshold
}

// ── Core autocompact ────────────────────────────────────────────────────────

/// Execute autocompact: call LLM to summarize the conversation.
///
/// 1. Build a summary prompt and send conversation + prompt to the LLM.
/// 2. If the prompt is too long, truncate oldest 20% messages and retry
///    (up to [`MAX_PTL_RETRIES`] times).
/// 3. Parse the `<summary>` from the response.
/// 4. Return a [`CompactResult`] with boundary marker + summary messages.
///
/// On failure, increments `state.consecutive_failures`.
/// On success, resets the failure counter.
/// 摘要失败的两种形状。
///
/// 本地定义而不是复用 `agentrs_provider::ProviderError`：那会把分层反过来。
/// 只有 `ContextTooLong` 需要被单独认出——它触发截断重试，其余一律终止。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummarizeError {
    /// 上下文超长，可截断后重试。
    ContextTooLong,
    /// 其他已脱敏失败。
    Other(String),
}

impl std::fmt::Display for SummarizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContextTooLong => write!(f, "context too long"),
            Self::Other(m) => write!(f, "{m}"),
        }
    }
}

/// 摘要一次请求。
///
/// **注入而不是直连 provider。** `agentrs-context` 依赖 `agentrs-provider` 会把
/// 分层反过来（provider 在下、context 在上，而 runtime 才装配两者），
/// 而且会让这个纯 crate 拖进 reqwest。这与 runtime 用 `StepDriver` 而不是直连
/// provider 是同一个理由。
#[async_trait::async_trait]
pub trait Summarizer: Send + Sync {
    /// 发一次请求，返回全部事件。
    async fn summarize(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, SummarizeError>;
}

pub async fn autocompact(
    provider: &dyn Summarizer,
    messages: &[Message],
    model: &str,
    config: &CompactConfig,
    state: &mut CompactState,
) -> Result<CompactResult, CompactError> {
    // Circuit breaker check
    if state.is_circuit_broken(config) {
        return Err(CompactError::CircuitBroken {
            failures: state.consecutive_failures,
        });
    }

    let pre_compact_tokens = state.last_input_tokens;
    let messages_summarized = messages.len();

    // Build messages for the compact LLM call: conversation + summary prompt
    let prompt = build_compact_prompt();
    let mut conv_messages = messages.to_vec();
    conv_messages.push(Message::new(Role::User, vec![ContentBlock::Text { text: prompt }]));

    let mut ptl_attempts = 0u32;

    let summary_text = loop {
        let request = LlmRequest {
            request_id: agentrs_contracts::ids::RequestId::new("compact"),
            cache_prefix_digest: None,
            model: agentrs_contracts::ids::ModelId::new(model),
            system: COMPACT_SYSTEM_PROMPT.to_string(),
            messages: conv_messages.clone(),
            tools: vec![],
            max_tokens: Some(COMPACT_MAX_OUTPUT_TOKENS),
            thinking: None,
            reasoning_effort: None,
        };

        match provider.summarize(request).await {
            Ok(events) => match collect_stream_text(events) {
                Ok((text, _usage)) => break text,
                Err(e) => {
                    state.record_failure();
                    return Err(e);
                }
            },
            Err(SummarizeError::ContextTooLong) if ptl_attempts < MAX_PTL_RETRIES => {
                ptl_attempts += 1;
                // Remove the summary prompt (last msg), truncate, re-add prompt
                let conversation_part = &conv_messages[..conv_messages.len() - 1];
                match truncate_for_retry(conversation_part) {
                    Some(mut truncated) => {
                        truncated.push(Message::new(
                            Role::User,
                            vec![ContentBlock::Text {
                                text: build_compact_prompt(),
                            }],
                        ));
                        conv_messages = truncated;
                    }
                    None => {
                        state.record_failure();
                        return Err(CompactError::PromptTooLong { attempts: ptl_attempts });
                    }
                }
            }
            Err(SummarizeError::ContextTooLong) => {
                state.record_failure();
                return Err(CompactError::PromptTooLong { attempts: ptl_attempts });
            }
            Err(e) => {
                state.record_failure();
                return Err(CompactError::Provider(e));
            }
        }
    };

    if summary_text.trim().is_empty() {
        state.record_failure();
        return Err(CompactError::EmptyResponse);
    }

    // Format and build post-compact messages
    let formatted = format_compact_summary(&summary_text);
    let summary_content = build_summary_content(&formatted, true);

    let metadata = CompactMetadata {
        trigger: CompactTrigger::Auto,
        pre_compact_tokens,
        messages_summarized,
    };

    let boundary_text = format!(
        "{BOUNDARY_PREFIX}\n{}",
        serde_json::to_string(&metadata).expect("CompactMetadata serialization cannot fail")
    );

    let boundary_msg = Message::new(Role::User, vec![ContentBlock::Text { text: boundary_text }]);

    let summary_msg = Message::new(Role::User, vec![ContentBlock::Text { text: summary_content }]);

    state.record_success();

    Ok(CompactResult {
        messages: vec![boundary_msg, summary_msg],
        messages_summarized,
        pre_compact_tokens,
    })
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Collect all text from a streaming LLM response.
///
/// 收 `Vec<LlmEvent>` 而不是 `mpsc::Receiver`：本仓库的 provider 契约就是
/// 一次返回全部事件（`ProviderPort::stream`），于是这里不再需要 tokio。
fn collect_stream_text(events: Vec<LlmEvent>) -> Result<(String, TokenUsage), CompactError> {
    let mut text = String::new();

    for event in events {
        match event {
            LlmEvent::TextDelta(delta) => text.push_str(&delta),
            LlmEvent::Done { usage, .. } => return Ok((text, usage)),
            LlmEvent::Error(e) => return Err(CompactError::StreamError(e)),
            // Ignore thinking deltas and tool calls (shouldn't happen in compact)
            _ => {}
        }
    }

    // Channel closed without a Done event
    Err(CompactError::EmptyResponse)
}

/// Truncate the oldest ~20% of messages for PTL retry.
///
/// Returns `None` if there are too few messages to truncate meaningfully.
fn truncate_for_retry(messages: &[Message]) -> Option<Vec<Message>> {
    if messages.len() < 2 {
        return None;
    }

    let drop_count = (messages.len() / 5).max(1);
    if drop_count >= messages.len() {
        return None;
    }

    let remaining = &messages[drop_count..];
    let mut result = Vec::with_capacity(remaining.len() + 1);

    // Ensure the first message is User role for API compatibility
    if remaining.first().map(|m| m.role) != Some(Role::User) {
        result.push(Message::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "[earlier conversation truncated for compaction retry]".to_string(),
            }],
        ));
    }

    result.extend_from_slice(remaining);
    Some(result)
}

/// Check if a message is a compact boundary marker.
pub fn is_compact_boundary(message: &Message) -> bool {
    message.content.iter().any(|block| {
        if let ContentBlock::Text { text } = block {
            text.starts_with(BOUNDARY_PREFIX)
        } else {
            false
        }
    })
}

/// Extract [`CompactMetadata`] from a boundary marker message.
pub fn extract_compact_metadata(message: &Message) -> Option<CompactMetadata> {
    for block in &message.content {
        // 原文用 let-chain，本仓库是 edition 2021，改写为嵌套 if。
        if let ContentBlock::Text { text } = block {
            if let Some(json_str) = text.strip_prefix(BOUNDARY_PREFIX) {
                let json_str = json_str.trim_start_matches('\n');
                return serde_json::from_str(json_str).ok();
            }
        }
    }
    None
}


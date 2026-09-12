use super::*;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agentrs_providers::{LlmProvider, ProviderError};
    use agentrs_types::llm::{LlmEvent, LlmRequest};
    use agentrs_types::message::{ContentBlock, Message, Role, StopReason, TokenUsage};
    use tokio::sync::mpsc;

    use super::*;
    use crate::commands::{CommandContext, CommandRegistry};
    use crate::compact::state::CompactState;
    use crate::context_usage::{ContextState, PromptUsage};
    use crate::output::OutputSink;
    use crate::output::null_sink::NullSink;
    use std::sync::Mutex;

    /// Captures emitted info lines so tests can assert on user-facing text.
    #[derive(Default)]
    struct RecordingSink {
        info: Mutex<Vec<String>>,
    }

    impl OutputSink for RecordingSink {
        fn emit_text_delta(&self, _text: &str, _msg_id: &str) {}
        fn emit_thinking(&self, _text: &str, _msg_id: &str) {}
        fn emit_tool_call(&self, _tool_use_id: &str, _name: &str, _input: &str) {}
        fn emit_tool_result(&self, _tool_use_id: &str, _name: &str, _is_error: bool, _content: &str) {}
        fn emit_stream_start(&self, _msg_id: &str) {}
        fn emit_stream_end(&self, _msg_id: &str, _turns: usize, _input: u64, _output: u64, _create: u64, _read: u64) {}
        fn emit_error(&self, _msg: &str) {}

        fn emit_info(&self, msg: &str) {
            self.info
                .lock()
                .expect("sink mutex is not poisoned")
                .push(msg.to_string());
        }
    }

    struct NullProvider;
    #[async_trait::async_trait]
    impl LlmProvider for NullProvider {
        async fn stream(&self, _: &LlmRequest) -> Result<tokio::sync::mpsc::Receiver<LlmEvent>, ProviderError> {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }
    }

    struct SuccessfulProvider;

    #[async_trait::async_trait]
    impl LlmProvider for SuccessfulProvider {
        async fn stream(&self, request: &LlmRequest) -> Result<mpsc::Receiver<LlmEvent>, ProviderError> {
            assert!(
                request.thinking.is_none(),
                "compact requests must omit provider thinking parameters"
            );
            let (tx, rx) = mpsc::channel(4);
            tx.send(LlmEvent::TextDelta("<summary>manual summary</summary>".into()))
                .await
                .unwrap();
            tx.send(LlmEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage::default(),
            })
            .await
            .unwrap();
            Ok(rx)
        }
    }

    #[tokio::test]
    async fn compact_already_compact_guard() {
        let provider: Arc<dyn LlmProvider> = Arc::new(NullProvider);
        let registry = CommandRegistry::new();
        let output = NullSink;
        let mut messages = vec![Message::new(Role::User, vec![ContentBlock::Text { text: "hi".into() }])];
        let mut state = CompactState::new();
        let mut context_state = ContextState::default();
        let prompt_usage = PromptUsage::default();
        let context_tools = Vec::new();
        let config = agentrs_config::compact::CompactConfig::default();

        let mut ctx = CommandContext {
            messages: &mut messages,
            compact_state: &mut state,
            compact_config: &config,
            provider,
            model: "test-model",
            internal_summarizer: None,
            output: &output,
            registry: &registry,
            context_state: &mut context_state,
            prompt_usage: &prompt_usage,
            context_tools: &context_tools,
            dynamic_system_tokens: 0,
        };

        let cmd = CompactCommand;
        let result = cmd.execute(&mut ctx, "").await.unwrap();
        assert_eq!(result, CommandResult::Continue);
        assert_eq!(ctx.messages.len(), 1);
    }

    #[tokio::test]
    async fn compact_resets_circuit_breaker() {
        let provider: Arc<dyn LlmProvider> = Arc::new(NullProvider);
        let registry = CommandRegistry::new();
        let output = NullSink;
        let mut messages: Vec<Message> = (0..10)
            .map(|i| {
                let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
                Message::new(
                    role,
                    vec![ContentBlock::Text {
                        text: format!("msg-{i}"),
                    }],
                )
            })
            .collect();
        let mut state = CompactState::new();
        state.consecutive_failures = 5;
        let mut context_state = ContextState::default();
        let prompt_usage = PromptUsage::default();
        let context_tools = Vec::new();
        let config = agentrs_config::compact::CompactConfig::default();

        let mut ctx = CommandContext {
            messages: &mut messages,
            compact_state: &mut state,
            compact_config: &config,
            provider,
            model: "test-model",
            internal_summarizer: None,
            output: &output,
            registry: &registry,
            context_state: &mut context_state,
            prompt_usage: &prompt_usage,
            context_tools: &context_tools,
            dynamic_system_tokens: 0,
        };

        let cmd = CompactCommand;
        let _ = cmd.execute(&mut ctx, "").await;
        // Circuit breaker was reset to 0 before the call, then failure increments it
        assert!(ctx.compact_state.consecutive_failures <= 1);
    }

    #[tokio::test]
    async fn successful_manual_compact_increments_count_and_requests_persistence() {
        let provider: Arc<dyn LlmProvider> = Arc::new(SuccessfulProvider);
        let registry = CommandRegistry::new();
        let output = NullSink;
        let mut messages: Vec<Message> = (0..6)
            .map(|index| {
                Message::new(
                    if index % 2 == 0 { Role::User } else { Role::Assistant },
                    vec![ContentBlock::Text {
                        text: format!("message-{index}"),
                    }],
                )
            })
            .collect();
        let mut state = CompactState::new();
        state.last_input_tokens = 50_000;
        let mut context_state = ContextState {
            context_usage: 50_000,
            ..ContextState::default()
        };
        let prompt_usage = PromptUsage::default();
        let context_tools = Vec::new();
        let config = agentrs_config::compact::CompactConfig::default();
        let mut ctx = CommandContext {
            messages: &mut messages,
            compact_state: &mut state,
            compact_config: &config,
            provider,
            model: "test-model",
            internal_summarizer: None,
            output: &output,
            registry: &registry,
            context_state: &mut context_state,
            prompt_usage: &prompt_usage,
            context_tools: &context_tools,
            dynamic_system_tokens: 0,
        };

        let result = CompactCommand.execute(&mut ctx, "").await.unwrap();

        assert_eq!(result, CommandResult::ContextChanged);
        assert_eq!(ctx.context_state.compact_count, 1);
        assert_eq!(ctx.messages.len(), 2);
    }

    #[tokio::test]
    async fn manual_compact_reports_the_token_count_on_both_sides_of_the_arrow() {
        let provider: Arc<dyn LlmProvider> = Arc::new(SuccessfulProvider);
        let registry = CommandRegistry::new();
        let output = RecordingSink::default();
        let mut messages: Vec<Message> = (0..6)
            .map(|index| {
                Message::new(
                    if index % 2 == 0 { Role::User } else { Role::Assistant },
                    vec![ContentBlock::Text {
                        text: format!("message-{index}"),
                    }],
                )
            })
            .collect();
        let mut state = CompactState::new();
        state.last_input_tokens = 50_000;
        let mut context_state = ContextState {
            context_usage: 50_000,
            ..ContextState::default()
        };
        let prompt_usage = PromptUsage::default();
        let context_tools = Vec::new();
        let config = agentrs_config::compact::CompactConfig::default();
        let mut ctx = CommandContext {
            messages: &mut messages,
            compact_state: &mut state,
            compact_config: &config,
            provider,
            model: "test-model",
            internal_summarizer: None,
            output: &output,
            registry: &registry,
            context_state: &mut context_state,
            prompt_usage: &prompt_usage,
            context_tools: &context_tools,
            dynamic_system_tokens: 0,
        };

        CompactCommand.execute(&mut ctx, "").await.unwrap();

        let info = output.info.lock().expect("sink mutex is not poisoned");
        let message = info.last().expect("compact should report its result");
        assert!(message.starts_with("Context compacted: 50k → "), "got: {message}");
        // The right-hand side must be a real post-compaction size, not a
        // literal word left over from the format string.
        assert!(!message.contains("→ compact"), "got: {message}");
        assert!(message.contains("messages summarized"), "got: {message}");
    }
}

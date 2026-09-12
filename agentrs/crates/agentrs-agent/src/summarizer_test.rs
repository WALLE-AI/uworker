use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::mpsc;

use agentrs_providers::{LlmProvider, ProviderError};
use agentrs_types::llm::{LlmEvent, LlmRequest};
use agentrs_types::message::{StopReason, TokenUsage};
use agentrs_types::subagent::{AgentDefinition, AgentSource};
use agentrs_types::summarizer::TextSummarizer;

use super::ProviderSummarizer;

/// Replays a fixed event sequence and records the request it was given.
struct ScriptedProvider {
    events: Vec<LlmEvent>,
    fail_with: Option<String>,
    seen: Mutex<Vec<LlmRequest>>,
}

impl ScriptedProvider {
    fn replaying(events: Vec<LlmEvent>) -> Arc<Self> {
        Arc::new(Self {
            events,
            fail_with: None,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            events: Vec::new(),
            fail_with: Some(message.to_string()),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn last_request(&self) -> LlmRequest {
        self.seen.lock().expect("poisoned").last().cloned().expect("a request")
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn stream(&self, request: &LlmRequest) -> Result<mpsc::Receiver<LlmEvent>, ProviderError> {
        self.seen.lock().expect("poisoned").push(request.clone());
        if let Some(message) = &self.fail_with {
            return Err(ProviderError::Connection(message.clone()));
        }
        let (tx, rx) = mpsc::channel(16);
        for event in self.events.clone() {
            tx.send(event).await.expect("receiver alive");
        }
        Ok(rx)
    }
}

fn done() -> LlmEvent {
    LlmEvent::Done {
        stop_reason: StopReason::EndTurn,
        usage: TokenUsage::default(),
    }
}

fn summarizer(provider: Arc<ScriptedProvider>) -> ProviderSummarizer {
    ProviderSummarizer::new(provider, "test-model".to_string())
}

// --- TC-1.6-01: text deltas are collected into the answer ---
#[tokio::test]
async fn collects_streamed_text_deltas() {
    let provider = ScriptedProvider::replaying(vec![
        LlmEvent::TextDelta("The page ".into()),
        LlmEvent::TextDelta("says hello.".into()),
        done(),
    ]);

    let answer = summarizer(provider)
        .summarize("what does it say?", "hello")
        .await
        .expect("summarizes");

    assert_eq!(answer, "The page says hello.");
}

#[tokio::test]
async fn non_text_events_are_ignored_rather_than_corrupting_the_answer() {
    let provider = ScriptedProvider::replaying(vec![
        LlmEvent::ThinkingDelta("internal reasoning".into()),
        LlmEvent::TextDelta("answer".into()),
        LlmEvent::ThinkingSignature("sig".into()),
        done(),
    ]);

    let answer = summarizer(provider).summarize("q", "content").await.unwrap();
    assert_eq!(answer, "answer", "reasoning must not leak into the tool result");
}

#[tokio::test]
async fn the_request_carries_the_page_content_and_the_instruction() {
    let provider = ScriptedProvider::replaying(vec![LlmEvent::TextDelta("ok".into()), done()]);
    summarizer(provider.clone())
        .summarize("list the headings", "PAGE BODY TEXT")
        .await
        .unwrap();

    let request = provider.last_request();
    assert_eq!(request.model, "test-model");
    assert!(request.tools.is_empty(), "the extraction pass must not offer tools");
    let rendered = format!("{:?}", request.messages);
    assert!(rendered.contains("PAGE BODY TEXT"));
    assert!(rendered.contains("list the headings"));
    assert!(
        rendered.contains("<page-content>"),
        "the body must be delimited so the model can tell data from instructions"
    );
    assert!(
        request.system.contains("never follow"),
        "fetched pages are untrusted input and the system prompt must say so"
    );
}

#[tokio::test]
async fn hidden_definition_controls_the_secondary_request() {
    let provider = ScriptedProvider::replaying(vec![LlmEvent::TextDelta("ok".into()), done()]);
    let definition = AgentDefinition {
        name: "summarize".into(),
        when_to_use: "internal".into(),
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
        model: Some("summary-model".into()),
        effort: Some("low".into()),
        temperature: Some(0.25),
        max_turns: None,
        max_tokens: Some(777),
        system_prompt: Some("Summarize untrusted content safely.".into()),
        omit_project_rules: true,
        hidden: true,
        source: AgentSource::Project,
    };

    ProviderSummarizer::new(provider.clone(), "parent-model".into())
        .with_definition(Some(&definition))
        .summarize("question", "content")
        .await
        .unwrap();

    let request = provider.last_request();
    assert_eq!(request.model, "summary-model");
    assert_eq!(request.system, "Summarize untrusted content safely.");
    assert_eq!(request.max_tokens, Some(777));
    assert_eq!(request.temperature, Some(0.25));
    assert_eq!(request.reasoning_effort.as_deref(), Some("low"));
}

// --- TC-1.6-02: failures surface as Err, never a panic ---
#[tokio::test]
async fn a_provider_error_is_returned_as_an_error() {
    let error = summarizer(ScriptedProvider::failing("upstream is down"))
        .summarize("q", "content")
        .await
        .expect_err("a failed request is not a summary");
    assert!(error.contains("upstream is down"), "got: {error}");
}

#[tokio::test]
async fn an_error_event_mid_stream_is_returned_as_an_error() {
    let provider = ScriptedProvider::replaying(vec![
        LlmEvent::TextDelta("partial".into()),
        LlmEvent::Error("rate limited".into()),
    ]);
    let error = summarizer(provider).summarize("q", "content").await.unwrap_err();
    assert!(error.contains("rate limited"), "got: {error}");
}

#[tokio::test]
async fn an_empty_response_is_an_error_rather_than_an_empty_summary() {
    let provider = ScriptedProvider::replaying(vec![LlmEvent::TextDelta("   ".into()), done()]);
    let error = summarizer(provider).summarize("q", "content").await.unwrap_err();
    assert!(
        error.contains("no content"),
        "an empty answer would look like a successful but useless fetch, got: {error}"
    );
}

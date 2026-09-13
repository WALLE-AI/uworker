use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentrs_memory::recall::{MemorySelector, SelectionRequest};
use agentrs_providers::{LlmProvider, ProviderError};
use agentrs_types::llm::{LlmEvent, LlmRequest};
use agentrs_types::message::{StopReason, TokenUsage};
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::{ProviderMemorySelector, build_prompt, parse_filenames};

#[test]
fn a_bare_json_array_parses() {
    assert_eq!(
        parse_filenames("[\"user_role.md\", \"feedback_tests.md\"]"),
        vec!["user_role.md".to_string(), "feedback_tests.md".to_string()]
    );
    assert!(parse_filenames("[]").is_empty());
}

// Wrapping the answer in prose or a fence is the common failure, not getting
// the shape wrong, so those answers must still be usable.
#[test]
fn fenced_and_prose_wrapped_answers_still_parse() {
    assert_eq!(parse_filenames("```json\n[\"a.md\"]\n```"), vec!["a.md".to_string()]);
    assert_eq!(parse_filenames("```\n[\"a.md\"]\n```"), vec!["a.md".to_string()]);
    assert_eq!(
        parse_filenames("Sure! Here are the relevant files: [\"a.md\", \"b.md\"] — hope that helps."),
        vec!["a.md".to_string(), "b.md".to_string()]
    );
}

// A model that wraps the array in an object still named the right files, so
// the array is lifted out rather than discarded.
#[test]
fn an_array_nested_in_an_object_is_lifted_out() {
    assert_eq!(parse_filenames("{\"files\": [\"a.md\"]}"), vec!["a.md".to_string()]);
}

#[test]
fn unusable_answers_yield_nothing_rather_than_erroring() {
    assert!(parse_filenames("").is_empty());
    assert!(parse_filenames("I could not find anything relevant.").is_empty());
    assert!(parse_filenames("[\"unterminated").is_empty());
    assert!(parse_filenames("]backwards[").is_empty());
    assert!(parse_filenames("[not, valid, json]").is_empty());
}

#[test]
fn non_string_and_blank_entries_are_dropped() {
    assert_eq!(
        parse_filenames("[\"a.md\", 42, null, \"  \", \"  b.md  \"]"),
        vec!["a.md".to_string(), "b.md".to_string()]
    );
}

#[test]
fn the_prompt_carries_the_listing_query_and_budget() {
    let prompt = build_prompt(&SelectionRequest {
        query: "how do I run the migration",
        manifest: "- [user] user_role.md (today): who the user is",
        recent_tools: &[],
        limit: 5,
    });

    assert!(prompt.contains("user_role.md"));
    assert!(prompt.contains("how do I run the migration"));
    assert!(prompt.contains("at most 5 filenames"));
    assert!(
        !prompt.contains("previous round"),
        "an empty tool list must not add an empty line to the prompt"
    );
}

#[test]
fn recent_tools_are_named_so_the_model_can_suppress_their_reference_docs() {
    let prompt = build_prompt(&SelectionRequest {
        query: "q",
        manifest: "- a.md",
        recent_tools: &["Grep".to_string(), "Read".to_string()],
        limit: 3,
    });

    assert!(prompt.contains("Tools used in the previous round: Grep, Read"));
}

// -- End-to-end selector behaviour --------------------------------------------

/// Replays a fixed event sequence, fails, or stalls past the deadline.
struct ScriptedProvider {
    events: Vec<LlmEvent>,
    fail_with: Option<String>,
    stall: Option<Duration>,
    seen: Mutex<Vec<LlmRequest>>,
}

impl ScriptedProvider {
    fn replaying(text: &str) -> Arc<Self> {
        Arc::new(Self {
            events: vec![
                LlmEvent::TextDelta(text.to_string()),
                LlmEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: TokenUsage::default(),
                },
            ],
            fail_with: None,
            stall: None,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            events: Vec::new(),
            fail_with: Some(message.to_string()),
            stall: None,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn stalling(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            events: Vec::new(),
            fail_with: None,
            stall: Some(delay),
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
        if let Some(delay) = self.stall {
            tokio::time::sleep(delay).await;
        }
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

fn selector(provider: Arc<ScriptedProvider>, timeout: Duration) -> ProviderMemorySelector {
    ProviderMemorySelector::new(provider, "test-model".to_string(), timeout)
}

fn request<'a>(manifest: &'a str) -> SelectionRequest<'a> {
    SelectionRequest {
        query: "how do I run the migration",
        manifest,
        recent_tools: &[],
        limit: 5,
    }
}

#[tokio::test]
async fn a_streamed_answer_becomes_filenames() {
    let provider = ScriptedProvider::replaying("[\"user_role.md\"]");

    let chosen = selector(Arc::clone(&provider), Duration::from_secs(5))
        .select(request("- user_role.md"))
        .await;

    assert_eq!(chosen, vec!["user_role.md".to_string()]);
    let sent = provider.last_request();
    assert_eq!(sent.model, "test-model");
    assert_eq!(
        sent.max_tokens,
        Some(256),
        "a filename list needs no large output budget"
    );
    assert!(sent.tools.is_empty(), "the selection pass must not offer tools");
}

// Recall decorates a turn and never gates one, so a dead provider has to look
// exactly like "nothing was relevant".
#[tokio::test]
async fn a_provider_error_selects_nothing_instead_of_failing() {
    let chosen = selector(ScriptedProvider::failing("connection refused"), Duration::from_secs(5))
        .select(request("- user_role.md"))
        .await;

    assert!(chosen.is_empty());
}

#[tokio::test]
async fn a_slow_provider_is_abandoned_at_the_deadline() {
    let started = std::time::Instant::now();

    let chosen = selector(
        ScriptedProvider::stalling(Duration::from_secs(30)),
        Duration::from_millis(50),
    )
    .select(request("- user_role.md"))
    .await;

    assert!(chosen.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the turn must not wait out the provider's own timeout"
    );
}

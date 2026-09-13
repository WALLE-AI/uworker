use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agentrs_config::config::{CliArgs, Config};
use agentrs_config::memory::ExtractConfig;
use agentrs_types::message::{ContentBlock, Message, Role};

use super::{MemoryExtractor, render_transcript};
use crate::spawner::AgentSpawner;
use crate::tool_policy::ToolPolicy;

fn test_config(session_dir: &std::path::Path) -> Config {
    let mut config = Config::resolve(&CliArgs {
        provider: Some("anthropic".to_string()),
        api_key: Some("sk-test".to_string()),
        base_url: None,
        model: Some("claude-sonnet-4-20250514".to_string()),
        max_tokens: Some(4096),
        thinking: None,
        thinking_budget: None,
        max_turns: None,
        max_tool_call_malformed_turns: None,
        max_tool_call_failure_turns: None,
        system_prompt: None,
        profile: None,
        auto_approve: true,
        project_dir: None,
    })
    .expect("test config");
    config.session.directory = session_dir.display().to_string();
    config
}

/// Build an extractor over a real memory directory. No model is ever called:
/// every test here exercises a path that stops before the spawn.
fn extractor(enabled: bool) -> (tempfile::TempDir, PathBuf, MemoryExtractor) {
    let temp = tempfile::tempdir().unwrap();
    let memory_dir = temp.path().join("memory");
    std::fs::create_dir_all(&memory_dir).unwrap();
    let config = test_config(&temp.path().join("sessions"));
    let provider = agentrs_providers::create_provider(&config);
    let spawner = Arc::new(AgentSpawner::new(
        provider,
        config,
        temp.path().to_path_buf(),
        ToolPolicy::Unrestricted,
    ));
    let mut extract = ExtractConfig::default();
    extract.enabled = enabled;
    let extractor = MemoryExtractor::new(spawner, memory_dir.clone(), extract);
    (temp, memory_dir, extractor)
}

fn user(text: &str) -> Message {
    Message::new(Role::User, vec![ContentBlock::Text { text: text.into() }])
}

fn assistant(text: &str) -> Message {
    Message::new(Role::Assistant, vec![ContentBlock::Text { text: text.into() }])
}

// AC4-5: a disabled extractor must cost nothing — no scan, no spawn.
#[tokio::test]
async fn a_disabled_extractor_never_starts_a_pass() {
    let (_temp, _dir, extractor) = extractor(false);

    assert!(!extractor.enabled());
    assert!(!extractor.spawn_extraction(&[user("remember that I prefer tabs")]));
}

#[tokio::test]
async fn an_empty_transcript_starts_nothing() {
    let (_temp, _dir, extractor) = extractor(true);

    assert!(!extractor.spawn_extraction(&[]));
    assert!(
        !extractor.spawn_extraction(&[user("   ")]),
        "whitespace is not a conversation"
    );
    assert!(
        !extractor.spawn_extraction(&[Message::new(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "call".into(),
                content: "file contents".into(),
                is_error: false,
            }],
        )]),
        "a turn made only of tool traffic teaches nothing about the user"
    );
}

// AC4-4: draining with nothing in flight must return at once, not wait out the
// timeout.
#[tokio::test]
async fn draining_with_nothing_in_flight_returns_immediately() {
    let (_temp, _dir, extractor) = extractor(true);
    let started = std::time::Instant::now();

    extractor.drain(Duration::from_secs(30)).await;

    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn the_prompt_names_the_directory_and_pre_injects_the_existing_memories() {
    let (_temp, memory_dir, extractor) = extractor(true);
    std::fs::write(
        memory_dir.join("user_role.md"),
        "---\nname: user_role\ndescription: senior Rust engineer\ntype: user\n---\n\nbody",
    )
    .unwrap();

    let prompt = extractor
        .build_prompt(&[user("I always run the integration suite before pushing")])
        .expect("a prompt");

    assert!(prompt.contains(&memory_dir.display().to_string()));
    assert!(
        prompt.contains("user_role.md"),
        "the listing is injected so the pass does not spend a turn on ls"
    );
    assert!(prompt.contains("I always run the integration suite before pushing"));
}

#[test]
fn an_empty_directory_is_described_rather_than_left_blank() {
    let (_temp, _dir, extractor) = extractor(true);

    let prompt = extractor.build_prompt(&[user("hello")]).expect("a prompt");

    assert!(prompt.contains("(no memories stored yet)"));
}

// -- Transcript rendering -----------------------------------------------------

#[test]
fn only_user_and_assistant_text_survives_rendering() {
    let rendered = render_transcript(&[
        user("don't mock the database in these tests"),
        Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Text {
                    text: "Understood.".into(),
                },
                ContentBlock::ToolUse {
                    id: "call".into(),
                    name: "Read".into(),
                    input: serde_json::json!({ "file_path": "/tmp/x" }),
                    extra: None,
                },
            ],
        ),
    ]);

    assert_eq!(
        rendered,
        "user: don't mock the database in these tests\nassistant: Understood.\n"
    );
}

// The correction or confirmation lands at the end of an exchange, so an
// over-long transcript must lose its head, not its tail.
#[test]
fn an_oversized_transcript_keeps_its_ending() {
    let filler = "x".repeat(200);
    let mut messages: Vec<Message> = (0..300).map(|_| user(&filler)).collect();
    messages.push(assistant("the decisive last word"));

    let rendered = render_transcript(&messages);

    assert!(rendered.len() <= super::MAX_TRANSCRIPT_CHARS + 256);
    assert!(rendered.ends_with("assistant: the decisive last word\n"));
    assert!(
        rendered.is_char_boundary(0),
        "truncation must not split a UTF-8 sequence"
    );
}

#[test]
fn multibyte_text_survives_truncation() {
    let filler = "记忆".repeat(100);
    let mut messages: Vec<Message> = (0..300).map(|_| user(&filler)).collect();
    messages.push(assistant("结尾"));

    let rendered = render_transcript(&messages);

    assert!(rendered.ends_with("assistant: 结尾\n"));
}

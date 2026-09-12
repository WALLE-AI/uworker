use std::sync::Arc;

use agentrs_agent::engine::AgentEngine;
use agentrs_agent::output::OutputSink;
use agentrs_agent::output::terminal::TerminalSink;
use agentrs_config::compat::ProviderCompat;
use agentrs_config::config::{Config, ProviderType, SessionConfig, ToolsConfig};
use agentrs_config::hooks::HooksConfig;
use agentrs_mcp::config::McpConfig;
use agentrs_providers::create_provider;
use agentrs_tools::read::ReadTool;
use agentrs_tools::registry::ToolRegistry;

/// Skip the test if ANTHROPIC_API_KEY is not set.
fn anthropic_api_key() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty())
}

fn anthropic_config(api_key: &str) -> Config {
    Config {
        provider: ProviderType::Anthropic,
        provider_label: "anthropic".to_string(),
        api_key: api_key.to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        model: "claude-haiku-4-20250514".to_string(), // cheapest for e2e
        max_tokens: Some(256),
        max_turns: Some(3),
        max_tool_call_malformed_turns: Some(3),
        max_tool_call_failure_turns: Some(3),
        system_prompt: Some("You are a helpful assistant. Be concise.".to_string()),
        thinking: None,
        prompt_caching: false,
        compat: ProviderCompat::anthropic_defaults(),
        tools: ToolsConfig {
            auto_approve: true,
            allow_list: vec![],
            skills: agentrs_config::config::SkillsPermissionConfig::default(),
        },
        session: SessionConfig {
            enabled: false,
            directory: "/tmp".to_string(),
            max_sessions: 1,
        },
        compact: agentrs_config::compact::CompactConfig::default(),
        compact_context_window_source: agentrs_config::config::CompactContextWindowSource::Default,
        plan: agentrs_config::plan::PlanConfig::default(),
        todo: agentrs_config::todo::TodoConfig::default(),
        shell: agentrs_config::shell::ShellConfig::default(),
        file_cache: agentrs_config::file_cache::FileCacheConfig::default(),
        hooks: HooksConfig::default(),
        bedrock: None,
        vertex: None,
        mcp: McpConfig::default(),
        logging: agentrs_config::logging::LoggingConfig::default(),
        tui: agentrs_config::tui::TuiConfig::default(),
        web: agentrs_config::web::WebConfig::default(),
        subagent: Default::default(),
        team: Default::default(),
        memory: Default::default(),
    }
}

/// Smoke test: single-turn text completion returns non-empty text.
#[tokio::test]
async fn test_anthropic_single_turn_completion() {
    let Some(api_key) = anthropic_api_key() else {
        eprintln!("[e2e] ANTHROPIC_API_KEY not set — skipping");
        return;
    };

    let config = anthropic_config(&api_key);
    let provider = create_provider(&config);
    let output: Arc<dyn OutputSink> = Arc::new(TerminalSink::new(true));
    let registry = ToolRegistry::new();

    let mut engine = AgentEngine::new_with_provider(provider, config, registry, output, std::env::temp_dir());
    let result = engine
        .run("Say 'hello world' and nothing else.", "")
        .await
        .expect("engine.run should not fail for a valid request");

    assert!(!result.text.is_empty(), "response text should not be empty");
    assert!(result.turns >= 1, "should complete in at least 1 turn");
    assert!(result.usage.output_tokens > 0, "should have output tokens");

    eprintln!(
        "[e2e] anthropic single-turn: {} tokens in / {} out",
        result.usage.input_tokens, result.usage.output_tokens
    );
}

/// Tool-use smoke test: agent calls Read tool when asked to read a file.
#[tokio::test]
async fn test_anthropic_tool_use() {
    let Some(api_key) = anthropic_api_key() else {
        eprintln!("[e2e] ANTHROPIC_API_KEY not set — skipping");
        return;
    };

    // Write a temp file to read
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    std::fs::write(tmp.path(), "e2e-test-content-42").expect("write tempfile");
    let path = tmp.path().to_string_lossy().to_string();

    let config = anthropic_config(&api_key);
    let provider = create_provider(&config);
    let output: Arc<dyn OutputSink> = Arc::new(TerminalSink::new(true));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadTool::new(None)));

    let mut engine = AgentEngine::new_with_provider(provider, config, registry, output, std::env::temp_dir());
    let prompt = format!(
        "Read the file at path '{}' and tell me what it contains. Be brief.",
        path
    );
    let result = engine.run(&prompt, "").await.expect("engine.run should not fail");

    assert!(!result.text.is_empty(), "response text should not be empty");
    // The model should have called Read and seen our content
    assert!(
        result.text.contains("e2e-test-content-42") || result.turns > 1,
        "model should either echo the content or have used multiple turns (tool call): {}",
        result.text
    );

    eprintln!(
        "[e2e] anthropic tool-use: {} turns, {} tokens out",
        result.turns, result.usage.output_tokens
    );
}

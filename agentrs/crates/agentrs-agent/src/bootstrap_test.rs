use super::*;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::sync::Arc;

    use agentrs_config::config::{CliArgs, McpServerConfig, TransportType};
    use agentrs_config::web::SearchBackendKind;
    use agentrs_protocol::events::ToolCategory;
    use agentrs_tools::Tool;
    use agentrs_types::tool::ToolResult;
    use async_trait::async_trait;
    use serde_json::{Value, json};

    use crate::output::OutputSink;
    use crate::output::null_sink::NullSink;
    use crate::tool_policy::ToolPolicy;

    use super::*;

    struct DeferredTestTool(&'static str);

    #[async_trait]
    impl Tool for DeferredTestTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            "deferred test tool"
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn is_concurrency_safe(&self, _input: &Value) -> bool {
            true
        }

        async fn execute(&self, _input: Value) -> ToolResult {
            ToolResult {
                content: "unused".to_string(),
                is_error: false,
            }
        }

        fn category(&self) -> ToolCategory {
            ToolCategory::Info
        }

        fn is_deferred(&self) -> bool {
            true
        }
    }

    fn test_config() -> Config {
        Config::resolve(&CliArgs {
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
            auto_approve: false,
            project_dir: None,
        })
        .unwrap()
    }

    fn write_skill(project_root: &std::path::Path, name: &str) {
        let skill_dir = project_root.join(".agentrs").join("skills").join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "---\ndescription: test skill\n---\n").unwrap();
    }

    #[tokio::test]
    async fn isolated_skill_dirs_excludes_workspace_skills() {
        let workspace = tempfile::TempDir::new().unwrap();
        let isolated_root = tempfile::TempDir::new().unwrap();
        write_skill(workspace.path(), "workspace-only");
        write_skill(isolated_root.path(), "isolated-only");

        let output: Arc<dyn OutputSink> = Arc::new(NullSink);
        let bootstrap = AgentBootstrap::new(test_config(), workspace.path().to_string_lossy(), output)
            .isolated_skill_dirs(vec![isolated_root.path().to_path_buf()]);

        let names = bootstrap
            .load_skills(workspace.path(), None)
            .await
            .into_iter()
            .map(|skill| skill.name)
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "isolated-only"));
        assert!(!names.iter().any(|name| name == "workspace-only"));
    }

    #[test]
    fn mcp_servers_with_runtime_env_uses_server_env_as_override() {
        let mut config = test_config();
        config.mcp.servers.insert(
            "stdio".to_string(),
            McpServerConfig {
                transport: TransportType::Stdio,
                command: Some("server".to_string()),
                args: None,
                env: Some(HashMap::from([
                    ("OVERRIDE".to_string(), "server".to_string()),
                    ("SERVER_ONLY".to_string(), "1".to_string()),
                ])),
                url: None,
                headers: None,
                deferred: None,
                startup_timeout_ms: None,
            },
        );

        let output: Arc<dyn OutputSink> = Arc::new(NullSink);
        let bootstrap = AgentBootstrap::new(config, "/tmp", output).runtime_env(vec![
            ("OVERRIDE".to_string(), "runtime".to_string()),
            ("RUNTIME_ONLY".to_string(), "1".to_string()),
        ]);

        let servers = bootstrap.mcp_servers_with_runtime_env();
        let env = servers
            .get("stdio")
            .and_then(|server| server.env.as_ref())
            .expect("stdio server env should exist");

        assert_eq!(env.get("OVERRIDE").map(String::as_str), Some("server"));
        assert_eq!(env.get("SERVER_ONLY").map(String::as_str), Some("1"));
        assert_eq!(env.get("RUNTIME_ONLY").map(String::as_str), Some("1"));
    }

    #[tokio::test]
    async fn tool_search_snapshot_excludes_policy_denied_tools() {
        let output: Arc<dyn OutputSink> = Arc::new(NullSink);
        let bootstrap = AgentBootstrap::new(test_config(), "/tmp", output)
            .tool_policy(ToolPolicy::allow_only(["ToolSearch", "AllowedDeferred"]));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DeferredTestTool("AllowedDeferred")));
        registry.register(Box::new(DeferredTestTool("DeniedDeferred")));

        bootstrap.register_tool_search(&mut registry);

        let tool_search = registry.get("ToolSearch").expect("ToolSearch should be registered");
        let allowed = tool_search.execute(json!({"query": "AllowedDeferred"})).await;
        let denied = tool_search.execute(json!({"query": "DeniedDeferred"})).await;

        assert!(allowed.content.contains("AllowedDeferred"));
        assert!(denied.content.starts_with("No deferred tools matching"));
        assert!(!denied.content.contains("\"name\": \"DeniedDeferred\""));
    }

    // --- Todo tool registration ---

    fn registered_todo(config: Config) -> (Vec<String>, bool) {
        let output: Arc<dyn OutputSink> = Arc::new(NullSink);
        let bootstrap = AgentBootstrap::new(config, "/tmp", output);
        let mut registry = ToolRegistry::new();

        let store = bootstrap.register_todo_tool(&mut registry);

        (registry.tool_names(), store.is_some())
    }

    #[test]
    fn todo_write_is_registered_by_default() {
        let (names, has_store) = registered_todo(test_config());
        assert!(names.contains(&"TodoWrite".to_string()), "got {names:?}");
        assert!(has_store, "the engine needs the store to persist and remind");
    }

    #[test]
    fn a_disabled_todo_section_registers_nothing() {
        let mut config = test_config();
        config.todo.enabled = false;

        let (names, has_store) = registered_todo(config);
        assert!(!names.contains(&"TodoWrite".to_string()), "got {names:?}");
        assert!(!has_store, "no store means no checklist to persist or remind about");
    }

    #[tokio::test]
    async fn the_registered_todo_tool_follows_the_parallel_policy() {
        let mut config = test_config();
        config.todo.allow_parallel_in_progress = true;
        let output: Arc<dyn OutputSink> = Arc::new(NullSink);
        let bootstrap = AgentBootstrap::new(config, "/tmp", output);
        let mut registry = ToolRegistry::new();
        bootstrap.register_todo_tool(&mut registry);

        let tool = registry.get("TodoWrite").expect("registered");
        assert!(
            tool.description().contains("several at once"),
            "the description must reflect the configured policy"
        );

        let result = tool
            .execute(json!({
                "todos": [
                    { "content": "A", "status": "in_progress" },
                    { "content": "B", "status": "in_progress" }
                ]
            }))
            .await;
        assert!(!result.is_error, "{}", result.content);
    }

    #[tokio::test]
    async fn the_default_todo_tool_enforces_a_single_active_task() {
        let output: Arc<dyn OutputSink> = Arc::new(NullSink);
        let bootstrap = AgentBootstrap::new(test_config(), "/tmp", output);
        let mut registry = ToolRegistry::new();
        bootstrap.register_todo_tool(&mut registry);

        let tool = registry.get("TodoWrite").expect("registered");
        let result = tool
            .execute(json!({
                "todos": [
                    { "content": "A", "status": "in_progress" },
                    { "content": "B", "status": "in_progress" }
                ]
            }))
            .await;
        assert!(result.is_error, "the default policy allows only one active task");
    }

    // --- Web tool registration (TC-1.6-06, TC-2.3-01 through TC-2.3-05) ---

    fn registered_web_tools(config: Config) -> Vec<String> {
        let output: Arc<dyn OutputSink> = Arc::new(NullSink);
        let provider = create_provider(&config);
        let bootstrap = AgentBootstrap::new(config, "/tmp", output);
        let mut registry = ToolRegistry::new();

        bootstrap.register_web_tools(&mut registry, &provider, std::path::Path::new("/tmp"));

        registry.tool_names()
    }

    #[test]
    fn web_fetch_is_registered_by_default() {
        let names = registered_web_tools(test_config());
        assert!(names.contains(&"WebFetch".to_string()), "got {names:?}");
    }

    #[test]
    fn disabling_web_removes_both_tools_from_the_registry() {
        let mut config = test_config();
        config.web.enabled = false;

        let names = registered_web_tools(config);
        assert!(
            names.is_empty(),
            "the model must not see a tool it cannot use: {names:?}"
        );
    }

    #[test]
    fn web_search_is_absent_when_no_backend_is_configured() {
        let names = registered_web_tools(test_config());
        assert!(
            !names.contains(&"WebSearch".to_string()),
            "search defaults to no backend and must not be advertised: {names:?}"
        );
    }

    #[test]
    fn web_search_is_absent_when_its_api_key_is_missing() {
        let mut config = test_config();
        config.web.search.backend = SearchBackendKind::Brave;
        config.web.search.api_key_env = "AGENTRS_TEST_BOOTSTRAP_MISSING_KEY".to_string();
        unsafe { std::env::remove_var("AGENTRS_TEST_BOOTSTRAP_MISSING_KEY") };

        let names = registered_web_tools(config);
        assert!(
            !names.contains(&"WebSearch".to_string()),
            "a configured-but-unusable backend must not be advertised: {names:?}"
        );
        assert!(names.contains(&"WebFetch".to_string()), "WebFetch is unaffected");
    }

    #[test]
    fn web_search_is_registered_for_a_key_less_backend_with_a_base_url() {
        let mut config = test_config();
        config.web.search.backend = SearchBackendKind::Searxng;
        config.web.search.base_url = "https://searx.example".to_string();

        let names = registered_web_tools(config);
        assert!(names.contains(&"WebSearch".to_string()), "got {names:?}");
    }

    #[test]
    fn an_unregistered_web_search_is_invisible_to_the_model() {
        let output: Arc<dyn OutputSink> = Arc::new(NullSink);
        let config = test_config();
        let provider = create_provider(&config);
        let bootstrap = AgentBootstrap::new(config, "/tmp", output);
        let mut registry = ToolRegistry::new();
        bootstrap.register_web_tools(&mut registry, &provider, std::path::Path::new("/tmp"));

        let advertised: Vec<String> = registry.to_tool_defs().into_iter().map(|def| def.name).collect();
        assert!(!advertised.contains(&"WebSearch".to_string()), "got {advertised:?}");
    }

    // --- TC-3.1-01 / TC-3.1-02: download directory path construction ---

    #[test]
    fn web_download_dir_is_built_from_path_components() {
        let workspace = std::path::Path::new("/tmp/project");
        let dir = super::web_download_dir(workspace);

        assert!(dir.starts_with(workspace), "downloads must stay inside the workspace");
        let tail: Vec<_> = dir
            .strip_prefix(workspace)
            .expect("under workspace")
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            tail,
            vec![".agentrs".to_string(), "webfetch".to_string()],
            "the path must be assembled from components, not a hardcoded separator"
        );
    }

    #[cfg(windows)]
    #[test]
    fn web_download_dir_uses_backslashes_on_windows() {
        let dir = super::web_download_dir(std::path::Path::new("C:\\work"));
        assert!(dir.to_string_lossy().contains("\\.agentrs\\webfetch"), "got {dir:?}");
    }

    #[cfg(unix)]
    #[test]
    fn web_download_dir_uses_slashes_on_unix() {
        let dir = super::web_download_dir(std::path::Path::new("/work"));
        assert_eq!(dir.to_string_lossy(), "/work/.agentrs/webfetch");
    }
}

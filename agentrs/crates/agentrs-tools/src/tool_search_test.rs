use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tool_defs() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "Read".into(),
                description: "Read a file".into(),
                input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
                deferred: false,
            },
            ToolDef {
                name: "SpawnTool".into(),
                description: "Spawn sub-agents".into(),
                input_schema: json!({"type": "object", "properties": {"agents": {"type": "array"}}}),
                deferred: true,
            },
            ToolDef {
                name: "EnterPlanMode".into(),
                description: "Enter plan mode".into(),
                input_schema: json!({"type": "object", "properties": {}}),
                deferred: true,
            },
        ]
    }

    #[tokio::test]
    async fn search_by_exact_name() {
        let tool = ToolSearchTool::new(build_tool_defs());
        let result = tool.execute(json!({"query": "SpawnTool"})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("SpawnTool"));
        assert!(result.content.contains("Spawn sub-agents"));
        assert!(result.content.contains("parameters"));
    }

    #[tokio::test]
    async fn search_case_insensitive() {
        let tool = ToolSearchTool::new(build_tool_defs());
        let result = tool.execute(json!({"query": "spawntool"})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("SpawnTool"));
    }

    #[tokio::test]
    async fn search_by_description_keyword() {
        let tool = ToolSearchTool::new(build_tool_defs());
        let result = tool.execute(json!({"query": "plan"})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("EnterPlanMode"));
    }

    #[tokio::test]
    async fn search_excludes_non_deferred() {
        let tool = ToolSearchTool::new(build_tool_defs());
        let result = tool.execute(json!({"query": "Read"})).await;
        // "Read" is not deferred, should not appear in results
        assert!(!result.content.contains("\"name\": \"Read\"") || result.content.contains("No deferred tools"));
    }

    #[tokio::test]
    async fn search_no_match() {
        let tool = ToolSearchTool::new(build_tool_defs());
        let result = tool.execute(json!({"query": "nonexistent"})).await;
        assert!(!result.is_error);
        assert!(result.content.contains("No deferred tools"));
    }

    #[tokio::test]
    async fn search_empty_query_returns_error() {
        let tool = ToolSearchTool::new(build_tool_defs());
        let result = tool.execute(json!({"query": ""})).await;
        assert!(result.is_error);
    }

    // --- A gated-off tool must explain itself here too ---
    //
    // ToolSearch is the path the model actually takes for deferred tools, so a
    // bare "no match" here is the message a user hits first.

    #[tokio::test]
    async fn searching_for_an_unconfigured_web_search_explains_how_to_enable_it() {
        let tool = ToolSearchTool::new(vec![deferred_def("WebFetch")]);

        let result = tool.execute(serde_json::json!({ "query": "WebSearch" })).await;

        assert!(!result.is_error);
        assert!(
            result.content.contains("No deferred tools matching"),
            "got: {}",
            result.content
        );
        assert!(result.content.contains("[web.search]"), "got: {}", result.content);
        assert!(result.content.contains("brave"), "got: {}", result.content);
    }

    #[tokio::test]
    async fn searching_for_a_genuine_typo_gets_no_configuration_advice() {
        let tool = ToolSearchTool::new(vec![deferred_def("WebFetch")]);

        let result = tool.execute(serde_json::json!({ "query": "Wibble" })).await;

        assert_eq!(result.content, "No deferred tools matching \"Wibble\" found.");
    }

    fn deferred_def(name: &str) -> agentrs_types::tool::ToolDef {
        agentrs_types::tool::ToolDef {
            name: name.to_string(),
            description: "stub".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            deferred: true,
        }
    }
}

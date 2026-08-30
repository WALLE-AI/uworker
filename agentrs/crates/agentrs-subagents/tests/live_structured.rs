//! Explore 与 ToolSearch 严格结构输出的真实模型冒烟。

use std::collections::BTreeMap;

use agentrs_prompts::registry::{EXPLORE, TOOL_SEARCH};
use agentrs_provider::transport::OpenAiCompatProvider;
use agentrs_provider::ProviderPort;
use agentrs_subagents::{
    build_request, collect, parse_structured, parse_tool_search, FunctionalKind, SubagentInput,
};
use agentrs_types::{ContentBlock, Message, Role};

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn live() -> Option<(OpenAiCompatProvider, agentrs_contracts::ids::ModelId)> {
    let base = env("AGENTRS_LIVE_BASE_URL")?;
    let model = env("AGENTRS_LIVE_MODEL")?;
    let provider = OpenAiCompatProvider::new(&base, env("AGENTRS_LIVE_API_KEY")).ok()?;
    Some((provider, model.into()))
}

#[tokio::test]
async fn 真实模型遵守_explore_严格_schema() {
    let Some((provider, model)) = live() else {
        eprintln!("跳过：未设置 live 端点");
        return;
    };
    let input = SubagentInput {
        task: "判断发布是否就绪，只能依据材料。".into(),
        context: vec![Message::new(
            Role::User,
            vec![ContentBlock::text("材料：单元测试通过；跨平台 L1 隔离尚未验证。")],
        )],
        model,
        max_tokens: Some(512),
    };
    let request = build_request(&input, "live-explore".into(), EXPLORE.template);
    assert!(request.tools.is_empty());
    let output = collect(&provider.stream(request).await.expect("真实模型调用失败")).expect("收敛失败");
    let parsed = parse_structured(FunctionalKind::Explore, &output).expect("Explore schema 不合格");
    assert!(!parsed.evidence.is_empty());
    assert!(parsed.confidence <= 100);
}

#[tokio::test]
async fn 真实模型的_tool_search_只选择候选() {
    let Some((provider, model)) = live() else {
        eprintln!("跳过：未设置 live 端点");
        return;
    };
    let candidates = vec!["Read".to_string(), "Grep".to_string(), "Deploy".to_string()];
    let input = SubagentInput {
        task: "需要搜索源代码中所有 timeout 配置；候选工具：Read, Grep, Deploy。".into(),
        context: Vec::new(),
        model,
        max_tokens: Some(256),
    };
    let system = TOOL_SEARCH
        .render(&BTreeMap::from([("limit", "1".to_string())]))
        .expect("提示词渲染");
    let request = build_request(&input, "live-tool-search".into(), &system);
    let output = collect(&provider.stream(request).await.expect("真实模型调用失败")).expect("收敛失败");
    let decision = parse_tool_search(&output, &candidates, 1).expect("ToolSearch schema 不合格");
    assert_eq!(decision.selected, ["Grep"]);
}

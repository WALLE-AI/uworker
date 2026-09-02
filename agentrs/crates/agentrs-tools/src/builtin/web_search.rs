//! `WebSearch`：用配置好的端点检索网页。
//!
//! **没有免密的网页搜索**——参考实现（opencode）用的 Exa / Parallel 都要 key。
//! 所以这个工具只在 [`super::Capabilities::search`] 为真时才进目录：
//! 注册一个必定失败的工具比没有它更糟，模型会反复试、换着法子重写查询，
//! 把一整个 Run 耗在一个根本不通的出口上。

use agentrs_types::ToolDef;
use serde_json::{json, Value};

use super::{arg, count, ToolCtx, ToolOutput};

/// 默认返回多少条。
pub const DEFAULT_RESULTS: u64 = 8;

/// 模型侧声明。
pub fn def() -> ToolDef {
    ToolDef::read_only(
        "WebSearch",
        "用配置好的搜索端点检索网页，返回标题、URL 与摘要。要读某条结果的正文，\
         再对它的 URL 调 WebFetch。",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "检索词，用自然语言或关键词都可以",
                    "minLength": 1
                },
                "num_results": {
                    "type": "integer",
                    "description": "最多返回多少条，默认 8",
                    "minimum": 1,
                    "maximum": 25
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    )
}

/// 执行。
pub async fn run(cx: &ToolCtx<'_>, args: &Value) -> ToolOutput {
    let Some(query) = arg(args, "query") else {
        return ToolOutput::bad_args("query");
    };
    let (Some(http), Some(endpoint)) = (cx.http, cx.search) else {
        // 正常路径上模型根本看不到这个工具——目录里没有它。这是兜底。
        return ToolOutput::failed(
            "WebSearch 未配置。宿主需设置 AGENTRS_SEARCH_URL 与 AGENTRS_SEARCH_KEY",
        );
    };
    let body = json!({
        "query": query,
        "numResults": count(args, "num_results", DEFAULT_RESULTS),
    });
    match http.post_json(&endpoint.url, &endpoint.key, body).await {
        Err(why) => ToolOutput::failed(format!("搜索失败：{why}")),
        Ok(value) => ToolOutput::ok(render(&value)),
    }
}

/// 把搜索端点的 JSON 渲染成一段给模型读的文本。
///
/// 兼容 Exa 风格（`results[]` + `title`/`url`/`text`/`snippet`）。换供应商只需
/// 改这一层字段映射。
pub fn render(body: &Value) -> String {
    let Some(results) = body.get("results").and_then(|v| v.as_array()) else {
        return "搜索端点没有返回 results 字段".to_string();
    };
    if results.is_empty() {
        return "没有搜到结果".to_string();
    }
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let s = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").trim();
            let 摘要 = [s("snippet"), s("text"), s("summary")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or("");
            let 摘要: String = 摘要.chars().take(300).collect();
            format!(
                "{}. {}\n   {}\n   {}",
                i + 1,
                if s("title").is_empty() { "(无标题)" } else { s("title") },
                s("url"),
                摘要.replace('\n', " ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeWorkspace;
    use super::*;
    use agentrs_contracts::sandbox::ExecutionOutcome;

    #[test]
    fn 渲染成模型读得懂的形状() {
        let body = json!({"results":[
            {"title":"标题一","url":"https://a/1","snippet":"摘要一"},
            {"url":"https://b/2","text":"没有标题的那条"}
        ]});
        let out = render(&body);
        assert!(out.contains("1. 标题一"), "{out}");
        assert!(out.contains("https://a/1"), "{out}");
        assert!(out.contains("摘要一"), "{out}");
        // 缺字段不能让整条消失，那会让模型以为只搜到一条。
        assert!(out.contains("2. (无标题)"), "{out}");
        assert!(out.contains("没有标题的那条"), "{out}");
    }

    #[test]
    fn 结果为空时明说() {
        assert_eq!(render(&json!({"results":[]})), "没有搜到结果");
        assert!(render(&json!({"x":1})).contains("没有返回 results"));
    }

    #[tokio::test]
    async fn 未配置端点时明说而不是静默失败() {
        let files = FakeWorkspace::new([]);
        let out = run(&ToolCtx::files_only(&files), &json!({"query": "rust"})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 1 });
        assert!(out.text.unwrap().contains("未配置"));
    }

    #[tokio::test]
    async fn 缺参数是参数问题() {
        let files = FakeWorkspace::new([]);
        let out = run(&ToolCtx::files_only(&files), &json!({})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 2 });
        assert!(out.text.unwrap().contains("query"));
    }
}

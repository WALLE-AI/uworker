//! `compact` 子 Agent 对真实模型的冒烟测试。
//!
//! 默认**跳过**。需要端点时设置：
//!
//! ```bash
//! AGENTRS_LIVE_BASE_URL=http://127.0.0.1:8094/v1 \
//! AGENTRS_LIVE_MODEL=Qwen3.8-27B \
//! NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1 \
//!   cargo test -p agentrs-subagents --test live_compact -- --nocapture
//! ```
//!
//! ## 这里验的是单元测试验不了的那部分
//!
//! 提示词里逐条列了必填项、`validate` 会检查缺项——这些都有单元测试。
//! 但**"一个真实模型照着这个提示词到底能不能写出合格的摘要"**
//! 只能真跑一次才知道。
//!
//! 若它跑不过，问题多半在提示词而不在代码：那正是这条测试要暴露的东西。

use std::sync::Arc;

use agentrs_context::compaction::missing_sections;
use agentrs_provider::transport::OpenAiCompatProvider;
use agentrs_provider::ProviderPort;
use agentrs_subagents::{build_request, collect, compact};
use agentrs_types::{ContentBlock, Message, Role};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// 一段值得压缩的历史：有意图、有工具、有错误、有未决审批。
fn 待压历史() -> Vec<Message> {
    let 用户 = |t: &str| Message::new(Role::User, vec![ContentBlock::text(t)]);
    let 助手 = |t: &str| Message::new(Role::Assistant, vec![ContentBlock::text(t)]);
    vec![
        用户("帮我把 config.rs 里的超时从 30 秒改成 60 秒，并加上重试次数上限。"),
        助手("我先读一下 config.rs。"),
        用户("[工具结果 Read config.rs] pub struct Config { pub timeout_secs: u64, pub retries: u32 }"),
        助手("我把 timeout_secs 的默认值改成 60。"),
        用户("[工具结果 Edit config.rs] 已替换 1 处（未提交，位于 ChangeSet cs-7 内）"),
        助手("接着我想改 src/net/pool.rs 里的连接池上限。"),
        用户("[工具结果 Edit src/net/pool.rs] 失败：未找到待替换的文本"),
        助手("看来 pool.rs 里的写法和我预期不同，我需要先读它。"),
        用户("[工具结果 Read src/net/pool.rs] const MAX_CONNS: usize = 16;"),
        助手("我打算把 MAX_CONNS 改成 32，另外想跑一下测试确认。"),
        用户("[审批请求] 运行 cargo test —— 等待用户确认，尚未批准"),
    ]
}

#[tokio::test]
async fn 真实模型能产出合格的压缩摘要() {
    let (Some(base), Some(model)) = (env("AGENTRS_LIVE_BASE_URL"), env("AGENTRS_LIVE_MODEL")) else {
        eprintln!("跳过：未设置 AGENTRS_LIVE_BASE_URL / AGENTRS_LIVE_MODEL");
        return;
    };

    let provider: Arc<dyn ProviderPort> =
        Arc::new(OpenAiCompatProvider::new(&base, env("AGENTRS_LIVE_API_KEY")).expect("装配失败"));

    let input = compact::input(待压历史(), model.as_str().into(), Some(1024));
    let req = build_request(&input, "live-compact".into(), &compact::system_prompt());

    // 零工具是结构保证，这里顺带再确认一次它没被装配环节加回去。
    assert!(req.tools.is_empty(), "compact 请求不得带工具");

    let events = provider.stream(req).await.expect("模型调用失败");
    let out = collect(&events).expect("未能收敛出结论");

    println!("\n===== 摘要正文 =====\n{}\n====================", out.conclusion);
    println!(
        "用量：输入 {} / 输出 {}",
        out.usage.input_tokens, out.usage.output_tokens
    );

    let 缺 = missing_sections(&out.conclusion);
    println!("缺失要点：{缺:?}");

    // **压缩必须保住具体标识符**：文件名、ChangeSet id、错误原文
    // 是后续工作的依据，摘要成"改了几个文件"就没用了。
    for 必留 in ["config.rs", "pool.rs"] {
        assert!(
            out.conclusion.contains(必留),
            "摘要丢掉了关键标识符 {必留}：\n{}",
            out.conclusion
        );
    }

    // 必填项检查。跑不过多半是提示词的问题——那正是这条测试要暴露的。
    assert!(
        缺.is_empty(),
        "摘要缺少必填要点 {缺:?}。\n提示词需要调整，而不是放宽校验。\n正文：\n{}",
        out.conclusion
    );

    // 走一遍真正的验收路径。
    compact::accept(&out).expect("accept 应当通过");
}

#[tokio::test]
async fn 真实模型的摘要保住了未决审批() {
    // **单独立一条**：这是最容易被摘要吃掉、后果又最严重的一项。
    // 漏了它，恢复后模型会以为那个操作已经批过了，直接接着往下做。
    let (Some(base), Some(model)) = (env("AGENTRS_LIVE_BASE_URL"), env("AGENTRS_LIVE_MODEL")) else {
        eprintln!("跳过：未设置端点");
        return;
    };

    let provider: Arc<dyn ProviderPort> =
        Arc::new(OpenAiCompatProvider::new(&base, env("AGENTRS_LIVE_API_KEY")).expect("装配失败"));
    let input = compact::input(待压历史(), model.as_str().into(), Some(1024));
    let req = build_request(&input, "live-approval".into(), &compact::system_prompt());
    let out = collect(&provider.stream(req).await.expect("调用失败")).expect("收敛失败");

    let 提到审批 = out.conclusion.contains("审批")
        && (out.conclusion.contains("未决")
            || out.conclusion.contains("等待")
            || out.conclusion.contains("尚未")
            || out.conclusion.contains("cargo test"));
    assert!(
        提到审批,
        "摘要没有保住「未决审批」这一项，恢复后模型会以为已经批过了：\n{}",
        out.conclusion
    );
}

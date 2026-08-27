//! `memorySelector` 对真实模型的冒烟测试。默认跳过。
//!
//! ```bash
//! AGENTRS_LIVE_BASE_URL=http://127.0.0.1:19121/starvlm/v1 \
//! AGENTRS_LIVE_MODEL=Qwen3.8-27B \
//! NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1 \
//!   cargo test -p agentrs-memory --test live_selector -- --nocapture
//! ```
//!
//! ## 验的是"模型到底听不听话"
//!
//! `validate` 会拦住越界与超限——那有单元测试。这里验的是**提示词管不管用**：
//! 一个真实模型面对一堆明显不相关的候选，会不会克制住不选。
//!
//! 若它把不相关的也选进来，`validate` 拦不住（那些 id 确实在候选里），
//! 结果就是模型被一段无关的旧事带偏。**这一类只能靠提示词，
//! 所以必须真跑一次看看提示词写得够不够。**

use std::sync::Arc;

use agentrs_contracts::ids::MemoryId;
use agentrs_contracts::ports::MemoryCandidate;
use agentrs_memory::selector::{parse_output, system_prompt, validate, DEFAULT_LIMIT};
use agentrs_provider::transport::OpenAiCompatProvider;
use agentrs_provider::ProviderPort;
use agentrs_subagents::{build_request, collect, SubagentInput};
use agentrs_types::{ContentBlock, Message, Role};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn 候选() -> Vec<MemoryCandidate> {
    [
        ("m1", "用户偏好：所有 Rust 代码用 4 空格缩进，不用 tab。", 1),
        ("m2", "上次给猫看病花了 800 块，兽医叫王医生。", 2),
        (
            "m3",
            "这个项目的数据库连接池上限历史上被调过三次，最后定在 32。",
            3,
        ),
        ("m4", "用户 2019 年去过冰岛，最喜欢的城市是雷克雅未克。", 4),
        (
            "m5",
            "项目里 config.rs 的超时字段叫 timeout_secs，不叫 timeout。",
            5,
        ),
        ("m6", "用户不喜欢吃香菜。", 6),
    ]
    .into_iter()
    .map(|(id, summary, rank)| MemoryCandidate {
        id: MemoryId::new(id),
        summary: summary.into(),
        rank,
    })
    .collect()
}

/// 把候选摆成模型能读的样子。
fn 提问(task: &str, cands: &[MemoryCandidate]) -> Vec<Message> {
    let mut s = format!("当前任务：{task}\n\n候选记忆：\n");
    for c in cands {
        s.push_str(&format!("- id={} ：{}\n", c.id, c.summary));
    }
    vec![Message::new(Role::User, vec![ContentBlock::text(s)])]
}

async fn 跑(task: &str) -> Option<(Vec<MemoryId>, String)> {
    let (Some(base), Some(model)) = (env("AGENTRS_LIVE_BASE_URL"), env("AGENTRS_LIVE_MODEL")) else {
        eprintln!("跳过：未设置端点");
        return None;
    };
    let provider: Arc<dyn ProviderPort> =
        Arc::new(OpenAiCompatProvider::new(&base, env("AGENTRS_LIVE_API_KEY")).expect("装配失败"));

    let input = SubagentInput {
        task: task.into(),
        context: 提问(task, &候选()),
        model: model.as_str().into(),
        max_tokens: Some(256),
    };
    let req = build_request(&input, "live-selector".into(), &system_prompt(DEFAULT_LIMIT));
    assert!(req.tools.is_empty(), "selector 请求不得带工具");

    let out = collect(&provider.stream(req).await.expect("调用失败")).expect("收敛失败");
    println!("\n===== 原始输出 =====\n{}\n===================", out.conclusion);

    let picked = parse_output(&out.conclusion).expect("产出无法解析为 {\"selected\": [...]}");
    println!("解析出：{picked:?}");
    Some((picked, out.conclusion))
}

#[tokio::test]
async fn 真实模型只从候选里选且不超限() {
    let Some((picked, _)) = 跑("修改 config.rs 里的超时配置").await else {
        return;
    };
    // 这一条 validate 能拦住，跑真模型是确认它**根本不会触发**。
    validate(&picked, &候选(), DEFAULT_LIMIT).expect("产出应当合法");
}

#[tokio::test]
async fn 真实模型克制住了不相关的记忆() {
    // **`validate` 拦不住这一类**：那些 id 确实在候选里，只是与任务无关。
    // 唯一的防线是提示词。所以必须真跑一次看提示词够不够。
    let Some((picked, 原文)) = 跑("修改 config.rs 里的超时配置").await else {
        return;
    };
    let ids: Vec<&str> = picked.iter().map(|i| i.as_str()).collect();

    for (无关, 是什么) in [("m2", "看猫花了多少钱"), ("m4", "去过冰岛"), ("m6", "不吃香菜")]
    {
        assert!(
            !ids.contains(&无关),
            "选进了与任务无关的记忆 {无关}（{是什么}）。\n\
             这一类 validate 拦不住，只能靠提示词——需要加强 system_prompt。\n\
             原始输出：\n{原文}"
        );
    }

    // 该选的至少要选到一条：全不选虽然"安全"，但 selector 就白跑了。
    assert!(
        ids.contains(&"m5") || ids.contains(&"m3") || ids.contains(&"m1"),
        "一条相关记忆都没选出来，selector 过于保守：{ids:?}"
    );
}

#[tokio::test]
async fn 任务与全部候选都无关时倾向于少选() {
    // **"不确定就不选"的正面检验。** 这里没有任何一条与任务相关，
    // 理想产出是空数组。
    let Some((picked, 原文)) = 跑("帮我把这台服务器的时区改成 UTC").await else {
        return;
    };
    assert!(
        picked.len() <= 2,
        "与任务毫无关系却选了 {} 条，提示词里的「不确定就不选」没生效：\n{原文}",
        picked.len()
    );
}

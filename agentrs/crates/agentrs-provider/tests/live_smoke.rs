//! 真实 provider 冒烟（迭代计划 W2 · R3 / 任务 T05A）。
//!
//! 这是整个计划**最早的证伪点**：前面所有设计如果和真实 SSE 流、真实 wire format
//! 对不上，在这里就会暴露，而不是拖到 Phase B 集中爆发。
//!
//! 默认跳过。需要真实端点时设置：
//!
//! ```sh
//! AGENTRS_LIVE_BASE_URL=http://localhost:8094/v1 \
//! AGENTRS_LIVE_MODEL=Qwen3.8-27B \
//! cargo test -p agentrs-provider --test live_smoke -- --nocapture
//! ```
//!
//! 本文件是**测试代码**，因此允许读环境变量——被测的 crate 本身不读。

use agentrs_provider::transport::OpenAiCompatProvider;
use agentrs_provider::ProviderPort;
use agentrs_types::{ContentBlock, LlmEvent, LlmRequest, Message, Role, StopReason};

struct Live {
    base_url: String,
    model: String,
}

fn live() -> Option<Live> {
    Some(Live {
        base_url: std::env::var("AGENTRS_LIVE_BASE_URL").ok()?,
        model: std::env::var("AGENTRS_LIVE_MODEL").ok()?,
    })
}

fn 请求(cfg: &Live, prompt: &str, max_tokens: u32) -> LlmRequest {
    LlmRequest {
        request_id: "smoke".into(),
        model: cfg.model.as_str().into(),
        system: "你是一个简洁的助手。".into(),
        messages: vec![Message::new(Role::User, vec![ContentBlock::text(prompt)])],
        tools: vec![],
        max_tokens: Some(max_tokens),
        thinking: None,
        reasoning_effort: None,
        cache_prefix_digest: None,
    }
}

#[tokio::test]
async fn 真实端点跑通文本_final() {
    let Some(cfg) = live() else {
        eprintln!("跳过：未设置 AGENTRS_LIVE_BASE_URL / AGENTRS_LIVE_MODEL");
        return;
    };

    let p = OpenAiCompatProvider::new(&cfg.base_url, std::env::var("AGENTRS_LIVE_API_KEY").ok())
        .expect("构造 provider");
    let events = p
        .stream(请求(&cfg, "只回答两个字：就绪", 32))
        .await
        .expect("流式请求");

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            LlmEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();

    let done = events.iter().find_map(|e| match e {
        LlmEvent::Done { stop_reason, usage } => Some((*stop_reason, *usage)),
        _ => None,
    });

    println!("文本: {text:?}");
    println!("终止: {done:?}");

    assert!(!text.is_empty(), "必须收到文本增量；实际事件: {events:?}");
    let (stop, usage) = done.expect("必须收到 Done 事件");
    assert!(
        matches!(stop, StopReason::EndTurn | StopReason::MaxTokens),
        "停止原因: {stop:?}"
    );
    assert!(
        usage.input_tokens > 0,
        "必须解析出输入用量，否则预算与缓存无从归因"
    );
}

#[tokio::test]
async fn 真实端点的用量可解析且缓存率可计算() {
    let Some(cfg) = live() else {
        eprintln!("跳过：未设置 live 端点");
        return;
    };

    let p = OpenAiCompatProvider::new(&cfg.base_url, std::env::var("AGENTRS_LIVE_API_KEY").ok()).unwrap();
    let events = p.stream(请求(&cfg, "数到三", 48)).await.unwrap();

    let usage = events
        .iter()
        .rev()
        .find_map(|e| match e {
            LlmEvent::Done { usage, .. } => Some(*usage),
            LlmEvent::Usage(u) => Some(*u),
            _ => None,
        })
        .expect("必须能拿到用量");

    println!(
        "input={} output={} cache_read={} hit_rate={:?}",
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_hit_rate()
    );

    assert!(usage.input_tokens > 0);
    assert!(usage.output_tokens > 0);
    // 命中率可能为 0（首次请求），但字段必须存在且可计算——
    // M1 出口要求连续 10 轮 ≥70%，那时这个数字才有约束力。
    assert!(usage.cache_hit_rate().is_some());
}

#[tokio::test]
async fn 相同前缀的第二次请求应观测到缓存命中() {
    let Some(cfg) = live() else {
        eprintln!("跳过：未设置 live 端点");
        return;
    };

    let p = OpenAiCompatProvider::new(&cfg.base_url, std::env::var("AGENTRS_LIVE_API_KEY").ok()).unwrap();

    // 用一段足够长的稳定前缀，短前缀通常低于服务端缓存的最小长度阈值。
    let 长前缀 = "背景资料：".to_string() + &"这是一段用于验证前缀缓存的稳定文本。".repeat(80);

    let mut 命中 = Vec::new();
    for i in 0..2 {
        let mut req = 请求(&cfg, &长前缀, 16);
        req.request_id = format!("cache-{i}").into();
        let events = p.stream(req).await.unwrap();
        let usage = events
            .iter()
            .rev()
            .find_map(|e| match e {
                LlmEvent::Done { usage, .. } => Some(*usage),
                LlmEvent::Usage(u) => Some(*u),
                _ => None,
            })
            .unwrap();
        println!(
            "第 {} 次: input={} cache_read={}",
            i + 1,
            usage.input_tokens,
            usage.cache_read_tokens
        );
        命中.push(usage.cache_read_tokens);
    }

    // 这条不是硬断言——端点是否报告缓存字段不由本 crate 决定。
    // 它的价值在于把"命中率是否可观测"在 W2 就摆到台面上，
    // 而不是等 M1 才发现端点根本不报告（迭代计划 M1 出口已据此拆成功能/收益两条）。
    if 命中[1] == 0 {
        eprintln!(
            "\n未观测到缓存命中。按以下顺序排查——前两步能区分\"没生效\"与\"生效但不报告\"：\n\
             \n\
             1. 服务端计数器（权威）。vLLM: curl -s <host>/metrics | grep prefix_cache\n\
             \x20  · prefix_cache_queries_total == 0 → 引擎侧未启用，与 API 无关\n\
             \x20  · queries > 0 但 hits == 0     → 前缀未对齐（检查 chat template 是否注入了变化内容）\n\
             \x20  · hits > 0                     → 缓存生效，只是 API 不报告，见第 2 步\n\
             2. API 字段。本适配器解析两种布局：\n\
             \x20  · usage.prompt_tokens_details.cached_tokens（OpenAI / 新版 vLLM）\n\
             \x20  · usage.prompt_cache_hit_tokens（DeepSeek 系）\n\
             \x20  两者皆无 → 该端点无法用于 M1 的收益验收，需换端点\n\
             3. 前缀长度。多数实现有最小长度阈值（vLLM 按 KV block 对齐，通常 16 token）\n\
             \n\
             实测参考（2026-08-25，vLLM 0.19.0 / Qwen3.8-27B / 端口 8094）：\n\
             queries_total=0、prompt_tokens_details=null、50K token 前缀 TTFT 冷热一致\n\
             （7448ms vs 7352ms）——三条证据一致指向引擎侧未启用。\n"
        );
    }
}

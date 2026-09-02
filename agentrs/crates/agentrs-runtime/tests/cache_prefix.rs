//! 缓存前缀不变式的端到端验证（架构 §9.1.1）。
//!
//! 对应 **M1 出口标准的「缓存 · 功能验证」那一条**——它刻意不依赖端点：
//!
//! > `cache_prefix_digest` 计算正确、`CacheBreakCause` 归因链路完整、
//! > Surface `Replace` 后的失效点计算准确。
//!
//! 拆成功能/收益两条的理由见迭代计划：我们能保证的是"发出去的前缀是稳定的"，
//! 端点那边命不命中取决于它开没开缓存。这个文件只管前者。
//!
//! ## 核心不变式：**正常追加不得破坏前缀**
//!
//! 每一轮对话都会往 Surface 追加内容。若追加就让前缀摘要变化，
//! 那么缓存**永远不可能命中**——每次请求的前缀都是新的。
//!
//! 这条听起来显然，但很容易被实现掉：把整段历史算进摘要就破了。
//! 稳定段的 S2 只覆盖"除末尾之外"的部分，真正破坏前缀的是 `Replace`（压缩）。

use std::sync::{Arc, Mutex};

use agentrs_contracts::authority::{AuthorityEnvelope, CapabilityView, PermissionMode};
use agentrs_contracts::event::EventPayload;
use agentrs_contracts::ids::Digest;
use agentrs_contracts::ports::RunPersistence;
use agentrs_contracts::spec::{
    ContextBudget, ConversationSnapshot, ExecutionBudget, ModelPolicy, ModelTier, RunSpec, SystemContext,
};
use agentrs_contracts::version::SpecVersion;
use agentrs_runtime::engine::{AdmitAll, EngineDeps, StepDriver, TurnGuards};
use agentrs_runtime::host::RuntimeHost;
use agentrs_runtime::inbox::UserInput;
use agentrs_testkit::{FakeClock, FakePersistence};
use agentrs_types::{ContentBlock, LlmEvent, LlmRequest, StopReason, TokenUsage, ToolDef};

/// 记录每次请求的前缀摘要，并在每次回复后**再投一轮输入**。
///
/// 为什么由驱动来投而不是提前一次性投完：inbox 一次 claim 最多取 8 条，
/// 提前投 4 条会被一次认领、合成**一个** Step——那样只有一次请求，
/// 比较不出"多轮之间前缀稳不稳定"。由驱动逐轮投才能造出真正的多轮。
struct 录制驱动 {
    digests: Mutex<Vec<Option<Digest>>>,
    /// 还要再投几轮。
    剩余: Mutex<usize>,
    handle: Mutex<Option<agentrs_runtime::host::RunHandle>>,
}

impl 录制驱动 {
    fn new(轮数: usize) -> Self {
        Self {
            digests: Mutex::new(Vec::new()),
            // 首轮由测试投，驱动负责其余的。
            剩余: Mutex::new(轮数.saturating_sub(1)),
            handle: Mutex::new(None),
        }
    }
    fn attach(&self, h: agentrs_runtime::host::RunHandle) {
        *self.handle.lock().expect("poisoned") = Some(h);
    }
    fn digests(&self) -> Vec<Option<Digest>> {
        self.digests.lock().expect("poisoned").clone()
    }
}

#[async_trait::async_trait]
impl StepDriver for 录制驱动 {
    async fn call(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, String> {
        self.digests
            .lock()
            .expect("poisoned")
            .push(req.cache_prefix_digest.clone());

        // 在返回之前投下一轮：引擎在本 Step 收尾后会再 claim 一次，
        // 于是同一个 Turn 内继续下一个 Step。
        let 下一轮 = {
            let mut n = self.剩余.lock().expect("poisoned");
            if *n > 0 {
                *n -= 1;
                Some(*n)
            } else {
                None
            }
        };
        if let Some(i) = 下一轮 {
            let h = self.handle.lock().expect("poisoned").clone();
            if let Some(h) = h {
                let _ = h
                    .submit(UserInput::Message(vec![ContentBlock::text(format!(
                        "续第 {i} 轮"
                    ))]))
                    .await;
            }
        }

        Ok(vec![
            LlmEvent::TextDelta("好".into()),
            LlmEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage::default(),
            },
        ])
    }
}

fn 目录() -> Vec<ToolDef> {
    vec![ToolDef::read_only("Read", "读", serde_json::json!({}))]
}

fn 规格(mode: PermissionMode, 提示: &str) -> RunSpec {
    RunSpec {
        run_id: "r-cache".into(),
        parent_run_id: None,
        conversation: ConversationSnapshot::default(),
        system_context: SystemContext {
            sections: vec![提示.to_string()],
            workspace_id: Some("ws".to_string()),
        },
        authority: AuthorityEnvelope {
            id: "e".into(),
            workspaces: vec!["ws".into()],
            tools: vec!["Read".into()],
            providers: vec!["p".into()],
            models: vec!["m".into()],
            max_depth: 1,
        },
        initial_capabilities: CapabilityView {
            tools: vec!["Read".into()],
            providers: vec!["p".into()],
            models: vec!["m".into()],
        },
        permission_mode: mode,
        model_policy: ModelPolicy {
            tiers: [(ModelTier::Default, "m".into())].into_iter().collect(),
            fallback: vec![],
            providers: vec!["p".into()],
            max_retries: 0,
            allow_attachments: false,
        },
        context_budget: ContextBudget {
            max_input_tokens: 10_000,
            reserved_output_tokens: 500,
            compaction_threshold_pct: 80,
        },
        execution_budget: ExecutionBudget::default(),
        change_set_id: None,
        checkpoint: None,
        spec_version: SpecVersion(1),
        config: Default::default(),
    }
}

/// 跑一次 Run，投入 `turns` 轮用户输入。
async fn 跑(
    mode: PermissionMode,
    提示: &str,
    tools: Vec<ToolDef>,
    轮数: usize,
) -> (Vec<Option<Digest>>, usize) {
    let driver = Arc::new(录制驱动::new(轮数));
    let persistence = Arc::new(FakePersistence::new());
    let deps = EngineDeps {
        persistence: persistence.clone() as Arc<dyn RunPersistence>,
        event_sink: None,
        clock: Arc::new(FakeClock::new(0)),
        driver: driver.clone(),
        admission: Arc::new(AdmitAll),
        tools: None,
        context: None,
        components: None,
    };

    let host = RuntimeHost::new();
    // 本文件验证的是**已授权目录**的缓存语义；未授权注册项被过滤由 host 单测覆盖。
    let allowed = tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>();
    let mut spec = 规格(mode, 提示);
    spec.authority.tools = allowed.clone();
    spec.initial_capabilities.tools = allowed;
    let started = host
        .start_with_tools(spec, deps, TurnGuards::default(), tools)
        .await
        .expect("启动失败");

    driver.attach(started.handle.clone());
    started
        .handle
        .submit(UserInput::Message(vec![ContentBlock::text("第 0 轮")]))
        .await
        .expect("提交失败");
    started.driver.await;

    let 断裂 = persistence
        .events()
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::CacheBreakObserved))
        .count();
    (driver.digests(), 断裂)
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn 每次请求都带上前缀摘要() {
    // 没有它，端点侧的缓存归因就没有对照物。
    let (digests, _) = 跑(PermissionMode::Default, "你是助手", 目录(), 1).await;
    assert!(!digests.is_empty());
    assert!(
        digests.iter().all(Option::is_some),
        "有请求没带 cache_prefix_digest"
    );
}

#[tokio::test]
async fn 正常追加不破坏前缀() {
    // **本文件最要紧的一条。** 若追加就让摘要变化，缓存永远不可能命中——
    // 每次请求的前缀都是新的，端点侧再怎么配置也没用。
    let (digests, 断裂) = 跑(PermissionMode::Default, "你是助手", 目录(), 4).await;
    assert!(
        digests.len() >= 2,
        "至少要有两次请求才能比较，实际 {}",
        digests.len()
    );

    let 首个 = digests[0].clone();
    assert!(
        digests.iter().all(|d| *d == 首个),
        "追加改变了前缀摘要：{digests:?}"
    );

    // 第一次请求没有可比对象，按 FirstRequest 记一次断裂；之后不应再有。
    assert_eq!(断裂, 1, "稳态下不该反复断裂");
}

#[tokio::test]
async fn 首次请求记为一次断裂() {
    // FirstRequest 是**如实记录**而不是噪声：冷启动确实没有可命中的前缀。
    // 把它藏起来会让"这次 miss 是怎么回事"少掉一种答案。
    let (_, 断裂) = 跑(PermissionMode::Default, "你是助手", 目录(), 1).await;
    assert_eq!(断裂, 1);
}

#[tokio::test]
async fn 系统提示不同则前缀不同() {
    let (a, _) = 跑(PermissionMode::Default, "你是助手", 目录(), 1).await;
    let (b, _) = 跑(PermissionMode::Default, "你是另一个助手", 目录(), 1).await;
    assert_ne!(a[0], b[0], "系统提示变了，前缀摘要必须跟着变");
}

#[tokio::test]
async fn 工具目录不同则前缀不同() {
    let mut 多一个 = 目录();
    多一个.push(ToolDef::mutating("Write", "写", serde_json::json!({})));

    let (a, _) = 跑(PermissionMode::Default, "你是助手", 目录(), 1).await;
    let (b, _) = 跑(PermissionMode::Default, "你是助手", 多一个, 1).await;
    assert_ne!(a[0], b[0], "工具目录变了，前缀摘要必须跟着变");
}

#[tokio::test]
async fn 权限模式不同则前缀不同() {
    // PermissionMode 同时影响 S0（提示分段）与 S1（目录过滤），
    // 两者都在稳定前缀里——所以模式切换必然是一个缓存断点。
    let (a, _) = 跑(PermissionMode::Default, "你是助手", 目录(), 1).await;
    let (b, _) = 跑(PermissionMode::Plan, "你是助手", 目录(), 1).await;
    assert_ne!(a[0], b[0], "模式变了，前缀摘要必须跟着变");
}

#[tokio::test]
async fn 工具描述改动也会改变前缀() {
    // 只改描述、不改名字——**描述进 S1，同样是稳定前缀的一部分**。
    // 漏掉它会让"改了个 prompt 却莫名 miss"查不出原因。
    let 改了描述 = vec![ToolDef::read_only("Read", "读取一个文件", serde_json::json!({}))];
    let (a, _) = 跑(PermissionMode::Default, "你是助手", 目录(), 1).await;
    let (b, _) = 跑(PermissionMode::Default, "你是助手", 改了描述, 1).await;
    assert_ne!(a[0], b[0]);
}

#[tokio::test]
async fn 同样的配置产生同样的前缀() {
    // 确定性：两次独立的 Run，配置一致则摘要一致。
    // 不成立的话，摘要里混进了随机数或时刻，比对就失去意义。
    let (a, _) = 跑(PermissionMode::Default, "你是助手", 目录(), 1).await;
    let (b, _) = 跑(PermissionMode::Default, "你是助手", 目录(), 1).await;
    assert_eq!(a[0], b[0]);
}

//! Plan Mode 的端到端验证（架构 §4.1.2）。
//!
//! 单元测试已覆盖投影与 guard 各自的语义。这里验证的是**装配起来仍然成立**——
//! 上一轮的教训是"每个组件单独看都对，装配起来才发现少了一条边"。
//!
//! 三条断言：
//!
//! 1. 模型**看不到**写类工具（目录投影生效）；
//! 2. 模型**被告知**自己在只读模式（提示分段生效，且与目录同步）；
//! 3. 模型硬要调写类工具时**调不动**（guard 生效，纵深防御的第二道）。
//!
//! 第 3 条最要紧：它是"历史里残留的旧 `tool_use`"和"模型凭记忆猜工具名"
//! 这两种情况的唯一防线。

use std::sync::{Arc, Mutex};

use agentrs_contracts::authority::{AuthorityEnvelope, CapabilityView, PermissionMode};
use agentrs_contracts::ids::Timestamp;
use agentrs_contracts::policy::DenyCode;
use agentrs_contracts::ports::{PolicyEnforcer, RunPersistence, SandboxExecutor};
use agentrs_contracts::spec::{
    ContextBudget, ConversationSnapshot, ExecutionBudget, ModelPolicy, ModelTier, RunSpec, SystemContext,
};
use agentrs_contracts::version::SpecVersion;
use agentrs_runtime::engine::{AdmitAll, EngineDeps, FixedClock, StepDriver, TurnGuards};
use agentrs_runtime::host::RuntimeHost;
use agentrs_runtime::inbox::UserInput;
use agentrs_runtime::toolround::{input_hash, ProposedCall, ToolRoundDeps};
use agentrs_testkit::{FakePersistence, FakePolicy, FakeSandbox};
use agentrs_types::{ContentBlock, LlmEvent, LlmRequest, StopReason, TokenUsage, ToolDef};

/// 记录收到的请求，并按脚本回放事件。
struct 录制驱动 {
    seen: Mutex<Vec<LlmRequest>>,
    /// 第一次调用时回放的事件；之后一律收尾。
    first: Mutex<Option<Vec<LlmEvent>>>,
}

impl 录制驱动 {
    fn new(first: Vec<LlmEvent>) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            first: Mutex::new(Some(first)),
        }
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.seen.lock().expect("poisoned").clone()
    }
}

#[async_trait::async_trait]
impl StepDriver for 录制驱动 {
    async fn call(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, String> {
        self.seen.lock().expect("poisoned").push(req);
        if let Some(ev) = self.first.lock().expect("poisoned").take() {
            return Ok(ev);
        }
        Ok(vec![
            LlmEvent::TextDelta("好的".into()),
            LlmEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage::default(),
            },
        ])
    }
}

fn 目录() -> Vec<ToolDef> {
    vec![
        ToolDef::read_only("Read", "读取文件", serde_json::json!({"type": "object"})),
        ToolDef::mutating("Write", "写入文件", serde_json::json!({"type": "object"})),
    ]
}

fn 规格(mode: PermissionMode) -> RunSpec {
    RunSpec {
        run_id: "r-plan".into(),
        parent_run_id: None,
        conversation: ConversationSnapshot::default(),
        system_context: SystemContext {
            sections: vec!["你是助手。".to_string()],
            workspace_id: Some("ws".to_string()),
        },
        authority: AuthorityEnvelope {
            id: "e".into(),
            workspaces: vec!["ws".into()],
            tools: vec!["Read".into(), "Write".into()],
            providers: vec!["p".into()],
            models: vec!["m".into()],
            max_depth: 1,
        },
        initial_capabilities: CapabilityView {
            tools: vec!["Read".into(), "Write".into()],
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

/// 模型硬要调 Write —— 模拟"历史里残留的旧 tool_use"或"凭记忆猜工具名"。
fn 提出写调用() -> Vec<LlmEvent> {
    vec![
        LlmEvent::ToolUse {
            id: "c1".into(),
            name: "Write".into(),
            input: serde_json::json!({"path": "a.txt", "content": "x"}),
            extra: None,
        },
        LlmEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: TokenUsage::default(),
        },
    ]
}

/// 让 FakeSandbox 认这次写调用的 grant。
///
/// **必须显式登记**：Sandbox 独立复核 grant 与输入指纹的绑定（宿主义务 H1）。
/// 不登记的话执行数恒为 0，Plan 模式的断言会因为**错误的原因**通过。
fn 登记写调用的_grant(sandbox: &FakeSandbox) {
    let call = ProposedCall {
        call_id: "c1".into(),
        tool_name: "Write".into(),
        arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
    };
    sandbox.issue_grant(
        "grant-1",
        input_hash(&call, "ws", &"cs-r-plan".into()),
        Timestamp(i64::MAX),
    );
}

async fn 跑一次(mode: PermissionMode, first: Vec<LlmEvent>) -> (Arc<录制驱动>, Arc<FakeSandbox>) {
    let driver = Arc::new(录制驱动::new(first));
    let sandbox = Arc::new(FakeSandbox::new());
    登记写调用的_grant(&sandbox);
    let persistence = Arc::new(FakePersistence::new());

    let deps = EngineDeps {
        persistence: persistence.clone() as Arc<dyn RunPersistence>,
        event_sink: None,
        clock: Arc::new(FixedClock(Timestamp(0))),
        driver: driver.clone(),
        admission: Arc::new(AdmitAll),
        tools: Some(Arc::new(ToolRoundDeps::minimal(
            Arc::new(FakePolicy::allow_all()) as Arc<dyn PolicyEnforcer>,
            sandbox.clone() as Arc<dyn SandboxExecutor>,
        ))),
        context: None,
        components: None,
    };

    let host = RuntimeHost::new();
    let started = host
        .start_with_tools(规格(mode), deps, TurnGuards::default(), 目录())
        .await
        .expect("启动失败");

    started
        .handle
        .submit(UserInput::Message(vec![ContentBlock::text("看看代码")]))
        .await
        .expect("提交失败");

    started.driver.await;
    (driver, sandbox)
}

#[tokio::test]
async fn plan_模式下模型看不到写类工具() {
    let (driver, _) = 跑一次(PermissionMode::Plan, 提出写调用()).await;
    let reqs = driver.requests();
    assert!(!reqs.is_empty(), "至少发出过一次请求");

    for r in &reqs {
        let names: Vec<&str> = r.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Read"), "只读工具应当保留");
        assert!(!names.contains(&"Write"), "Plan 模式下 Write 不得进入工具目录");
    }
}

#[tokio::test]
async fn plan_模式下系统提示说明了当前是只读() {
    // 与目录同步：目录里没有写类工具，提示就必须解释为什么。
    // 缺了它，模型会反复尝试它"记得"存在的工具。
    let (driver, _) = 跑一次(PermissionMode::Plan, 提出写调用()).await;
    let sys = &driver.requests()[0].system;
    assert!(sys.contains("只读"), "系统提示未说明只读模式：{sys}");
    // 宿主自己的分段不能被顶掉。
    assert!(sys.contains("你是助手"), "宿主的系统分段被覆盖了：{sys}");
}

#[tokio::test]
async fn plan_模式下硬调写类工具会被挡住() {
    // 纵深防御的第二道。目录过滤只防"被误导"，
    // 历史里残留的旧 tool_use 不经过目录。
    let (_, sandbox) = 跑一次(PermissionMode::Plan, 提出写调用()).await;
    assert_eq!(sandbox.execution_count(), 0, "Plan 模式下写类工具不得触达执行器");
}

#[tokio::test]
async fn plan_模式的拒绝码是_permission_mode() {
    // 拒绝码要能让用户看懂"是模式挡的"，而不是含糊的"不允许"——
    // 前者的下一步动作是退出 Plan 模式，后者无从下手。
    let persistence = Arc::new(FakePersistence::new());
    let driver = Arc::new(录制驱动::new(提出写调用()));
    let sandbox = Arc::new(FakeSandbox::new());
    登记写调用的_grant(&sandbox);

    let deps = EngineDeps {
        persistence: persistence.clone() as Arc<dyn RunPersistence>,
        event_sink: None,
        clock: Arc::new(FixedClock(Timestamp(0))),
        driver,
        admission: Arc::new(AdmitAll),
        tools: Some(Arc::new(ToolRoundDeps::minimal(
            Arc::new(FakePolicy::allow_all()) as Arc<dyn PolicyEnforcer>,
            sandbox.clone() as Arc<dyn SandboxExecutor>,
        ))),
        context: None,
        components: None,
    };

    let host = RuntimeHost::new();
    let started = host
        .start_with_tools(规格(PermissionMode::Plan), deps, TurnGuards::default(), 目录())
        .await
        .expect("启动失败");
    started
        .handle
        .submit(UserInput::Message(vec![ContentBlock::text("写个文件")]))
        .await
        .expect("提交失败");
    started.driver.await;

    let events = persistence.events();
    let 拒绝码: Vec<DenyCode> = events
        .iter()
        .filter_map(|e| match &e.payload {
            agentrs_contracts::event::EventPayload::StepResultRecorded { result } => match &result.outcome {
                agentrs_contracts::StepOutcome::Denied { code, .. } => Some(*code),
                _ => None,
            },
            _ => None,
        })
        .collect();

    assert_eq!(拒绝码, [DenyCode::PermissionMode]);
}

#[tokio::test]
async fn default_模式下写类工具正常可见可用() {
    // 反向对照：上面三条不是因为写类工具在哪都跑不通。
    let (driver, sandbox) = 跑一次(PermissionMode::Default, 提出写调用()).await;

    let reqs = driver.requests();
    let names: Vec<&str> = reqs[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"Write"));
    assert_eq!(sandbox.execution_count(), 1, "Default 模式下应当执行");

    // 也不该混进 Plan 模式的提示。
    assert!(!reqs[0].system.contains("只读"));
}

#[tokio::test]
async fn 终态摘要带回最后一条助手文本() {
    // 只报统计的 Run 摘要等于"跑完了但不说结果"。
    // 文本从 Surface 投影而来，不另存一份——否则"模型可见即已记录"
    // 就多了一条不受该不变式约束的旁路。
    let driver = Arc::new(录制驱动::new(vec![
        LlmEvent::TextDelta("结论：".into()),
        LlmEvent::TextDelta("一切正常".into()),
        LlmEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage::default(),
        },
    ]));
    let sandbox = Arc::new(FakeSandbox::new());
    let deps = EngineDeps {
        persistence: Arc::new(FakePersistence::new()) as Arc<dyn RunPersistence>,
        event_sink: None,
        clock: Arc::new(FixedClock(Timestamp(0))),
        driver,
        admission: Arc::new(AdmitAll),
        tools: Some(Arc::new(ToolRoundDeps::minimal(
            Arc::new(FakePolicy::allow_all()) as Arc<dyn PolicyEnforcer>,
            sandbox as Arc<dyn SandboxExecutor>,
        ))),
        context: None,
        components: None,
    };
    let host = RuntimeHost::new();
    let started = host
        .start_with_tools(规格(PermissionMode::Default), deps, TurnGuards::default(), 目录())
        .await
        .expect("启动失败");
    started
        .handle
        .submit(UserInput::Message(vec![ContentBlock::text("检查")]))
        .await
        .expect("提交失败");

    let summary = started.driver.await;
    // 跨帧的增量必须已经拼成一条完整消息。
    assert_eq!(summary.final_text.as_deref(), Some("结论：一切正常"));
}

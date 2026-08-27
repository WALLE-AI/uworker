//! `agentrs` —— 轻量命令行驱动器。
//!
//! ```text
//! agentrs doctor                              健康检查：版本、端口、adapter
//! agentrs run <prompt> [dev] [--commit]        跑一次 Run
//! agentrs conformance                          跑宿主义务 conformance suite
//!
//! 以下四条**只读**事件日志，不触达 provider、不执行工具，
//! 因此在一个坏掉的 Run 上照样能跑——而那正是最需要它们的时候：
//!
//! ```text
//! agentrs validate      [log]   这份日志本身能不能信
//! agentrs trajectory    [log]   这次 Run 做了什么
//! agentrs cache-report  [log]   钱花在哪、每次 miss 因为什么
//! agentrs replay        [log]   投影是否确定性
//! agentrs resume        [log]   崩溃时正在做什么、要不要先 reconcile
//! agentrs export        [log]   产出一份可对外分享的脱敏 bundle
//! ```
//! ```
//!
//! 默认只装配只读/模拟 adapter；`--adapter dev` 才启用真实的本地执行器，
//! 且那条路径仍走完整的 grant → Sandbox 复核链路。

mod inspect;

use std::sync::Arc;

use agentrs_contracts::authority::{AuthorityEnvelope, CapabilityView, PermissionMode};
use agentrs_contracts::ids::Timestamp;
use agentrs_contracts::ports::{PolicyEnforcer, RunPersistence, SandboxExecutor};
use agentrs_contracts::spec::{
    ContextBudget, ConversationSnapshot, ExecutionBudget, ModelPolicy, RunSpec, SystemContext,
};
use agentrs_contracts::version::SpecVersion;
use agentrs_dev_adapter::{DevPolicy, JsonlPersistence, LocalFileSandbox, NON_PRODUCTION_BANNER};
use agentrs_provider::transport::OpenAiCompatProvider;
use agentrs_provider::ProviderPort;
use agentrs_runtime::engine::{AdmitAll, EngineDeps, FixedClock, StepDriver, TurnGuards};
use agentrs_runtime::host::RuntimeHost;
use agentrs_runtime::inbox::UserInput;
use agentrs_runtime::toolround::ToolRoundDeps;
use agentrs_types::{ContentBlock, LlmEvent, LlmRequest};

/// 把 `ProviderPort` 适配成引擎的 `StepDriver`。
///
/// 引擎不直接依赖 provider crate——那会让状态机测试必须构造完整适配器。
struct ProviderDriver(Arc<dyn ProviderPort>);

#[async_trait::async_trait]
impl StepDriver for ProviderDriver {
    async fn call(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, String> {
        if std::env::var("AGENTRS_TRACE").is_ok() {
            eprintln!(
                "[trace] model={} msgs={} tools={}",
                req.model,
                req.messages.len(),
                req.tools.len()
            );
        }
        self.0.stream(req).await.map_err(|e| {
            // 只输出稳定错误码，不透传响应正文（可能含密钥或用户内容）。
            eprintln!("[provider] {e}");
            e.to_string()
        })
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// 对 dev-adapter 跑一遍宿主义务 conformance suite。
///
/// **第三方 adapter 应当照此实现 `SandboxSubject` 并在自家 CI 里跑同一套检查。**
/// 内核不为第三方 adapter 的正确性背书（架构 §12.2）。
async fn conformance(workspace: &str) -> Result<(), Box<dyn std::error::Error>> {
    use agentrs_contracts::ids::ExecutionId;
    use agentrs_contracts::policy::InputHash;
    use agentrs_contracts::sandbox::{ExecutionRequest, IsolationLevel};
    use agentrs_testkit::conformance::{check_sandbox, SandboxSubject};

    struct Subject(Arc<LocalFileSandbox>);

    impl SandboxSubject for Subject {
        fn executor(&self) -> Arc<dyn SandboxExecutor> {
            self.0.clone()
        }
        fn issue_grant(&self, id: &str, bound: InputHash, expires_at: Timestamp) {
            self.0.issue_grant(id, bound, expires_at);
        }
        fn mutating_request(
            &self,
            execution_id: &str,
            input_hash: InputHash,
            required_isolation: IsolationLevel,
        ) -> ExecutionRequest {
            ExecutionRequest {
                execution_id: ExecutionId::new(execution_id),
                tool_name: "Write".into(),
                arguments: serde_json::json!({
                    "path": format!(".agentrs-conformance/{execution_id}.txt"),
                    "content": "x"
                }),
                change_set_id: "cs-conformance".into(),
                input_hash,
                required_isolation,
            }
        }
        fn side_effect_happened(&self, req: &ExecutionRequest) -> Option<bool> {
            // 副作用落在 overlay 里——内核无提交权，磁盘上查不到。
            let p = req.arguments["path"].as_str()?;
            Some(self.0.read_text("cs-conformance", p).is_some())
        }

        // 读己之写与 ChangeSet 隔离（H7）需要一个可配对的读。
        fn paired_read(
            &self,
            execution_id: &str,
            input_hash: InputHash,
            written: &ExecutionRequest,
        ) -> Option<ExecutionRequest> {
            Some(ExecutionRequest {
                execution_id: ExecutionId::new(execution_id),
                tool_name: "Read".into(),
                arguments: serde_json::json!({"path": written.arguments["path"]}),
                change_set_id: written.change_set_id.clone(),
                input_hash,
                required_isolation: written.required_isolation,
            })
        }
        fn read_content(&self, r: &agentrs_contracts::sandbox::ExecutionResult) -> Option<String> {
            r.output.clone()
        }
        fn in_change_set(&self, req: &ExecutionRequest, change_set: &str) -> Option<ExecutionRequest> {
            let mut r = req.clone();
            r.change_set_id = change_set.into();
            Some(r)
        }
    }

    let sb = Arc::new(LocalFileSandbox::new(workspace)?);
    println!("受检对象：agentrs-dev-adapter（L0BasicContainment）");
    println!("工作区根：{}\n", sb.root().display());

    let mut ok = true;
    let report = check_sandbox(&Subject(sb)).await;
    print!("{}", report.render());
    ok &= report.passed();

    // ---- H3：持久化 ----
    {
        use agentrs_contracts::ids::RunEpoch;
        use agentrs_testkit::conformance::persistence::{check_persistence, PersistenceSubject};

        struct P(Arc<JsonlPersistence>);
        impl PersistenceSubject for P {
            fn persistence(&self) -> Arc<dyn RunPersistence> {
                self.0.clone()
            }
            fn advance_epoch(&self, current: RunEpoch) -> Option<RunEpoch> {
                Some(RunEpoch(current.0 + 1))
            }
        }

        // 写进独立文件，不污染真实的事件流。
        let p = Arc::new(JsonlPersistence::open(".agentrs/conformance-events.jsonl")?);
        println!();
        let r = check_persistence(&P(p)).await;
        print!("{}", r.render());
        ok &= r.passed();
    }

    // ---- H5：策略 ----
    {
        use agentrs_contracts::policy::ToolProposal;
        use agentrs_testkit::conformance::policy::{check_policy, PolicySubject};

        struct Pol(Arc<DevPolicy>);
        impl PolicySubject for Pol {
            fn policy(&self) -> Arc<dyn PolicyEnforcer> {
                self.0.clone()
            }
            fn allowed_proposal(&self, tag: &str) -> ToolProposal {
                ToolProposal {
                    call_id: tag.into(),
                    tool_name: "Write".into(),
                    arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
                    workspace_id: "default".into(),
                    change_set_id: "cs-conformance".into(),
                    input_hash: agentrs_contracts::policy::InputHash(
                        agentrs_contracts::ids::Digest::from_hex(tag),
                    ),
                }
            }
        }

        let sb2 = Arc::new(LocalFileSandbox::new(workspace)?);
        let pol = Arc::new(DevPolicy::new(sb2, ["Write"]));
        println!();
        let r = check_policy(&Pol(pol)).await;
        print!("{}", r.render());
        ok &= r.passed();
    }

    if !ok {
        return Err("conformance 不合格".into());
    }
    Ok(())
}

fn doctor() {
    println!("agentrs {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("契约 SpecVersion  : {:?}", SpecVersion(1));
    println!("隔离级别（dev）   : L0BasicContainment（真实隔离归 SandboxRS）");
    println!();
    println!("环境：");
    for k in ["AGENTRS_BASE_URL", "AGENTRS_MODEL", "AGENTRS_WORKSPACE"] {
        match env(k) {
            Some(v) => println!("  {k:18} = {v}"),
            None => println!("  {k:18} = (未设置)"),
        }
    }
    println!();
    println!("adapter：");
    println!("  default          只读/模拟，不触达任何真实资源");
    println!("  dev              本地 JSONL + 受限文件执行器（L0）");
}

async fn run(prompt: &str, use_dev: bool, commit: bool) -> Result<(), Box<dyn std::error::Error>> {
    let base_url = env("AGENTRS_BASE_URL").ok_or("需要 AGENTRS_BASE_URL")?;
    let model = env("AGENTRS_MODEL").ok_or("需要 AGENTRS_MODEL")?;
    let workspace = env("AGENTRS_WORKSPACE").unwrap_or_else(|| ".".to_string());

    let provider: Arc<dyn ProviderPort> =
        Arc::new(OpenAiCompatProvider::new(&base_url, env("AGENTRS_API_KEY"))?);

    // 内核无提交权：ChangeSet 的提交是**宿主的显式动作**。
    let mut committer: Option<Arc<LocalFileSandbox>> = None;

    // ---- 装配 adapter ----
    let (persistence, tools): (Arc<dyn RunPersistence>, Option<Arc<ToolRoundDeps>>) = if use_dev {
        eprintln!("{NON_PRODUCTION_BANNER}");

        let sandbox = Arc::new(LocalFileSandbox::new(&workspace)?);
        eprintln!("工作区根：{}", sandbox.root().display());
        committer = Some(sandbox.clone());

        // allowlist 是白名单——不在名单内一律拒绝。
        let policy = Arc::new(DevPolicy::new(
            sandbox.clone(),
            ["Read", "Grep", "Write", "Edit", "Delete"],
        ));

        let p: Arc<dyn RunPersistence> = Arc::new(JsonlPersistence::open(".agentrs/events.jsonl")?);
        let t = Arc::new(ToolRoundDeps::minimal(
            policy as Arc<dyn PolicyEnforcer>,
            sandbox as Arc<dyn SandboxExecutor>,
        ));
        (p, Some(t))
    } else {
        // 默认路径：不触达任何真实资源。
        (Arc::new(NullPersistence), None)
    };

    let host = RuntimeHost::new();
    let deps = EngineDeps {
        persistence,
        clock: Arc::new(FixedClock(Timestamp(0))),
        driver: Arc::new(ProviderDriver(provider)),
        admission: Arc::new(AdmitAll),
        tools,
    };

    // 工具目录由宿主提供——内核不持有实现，也不自行发现工具。
    let tools = if use_dev {
        vec![
            agentrs_types::ToolDef::read_only(
                "Read",
                "读取工作区内的一个文本文件。path 为相对工作区根的路径。",
                serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string", "description": "相对路径"}},
                    "required": ["path"]
                }),
            ),
            agentrs_types::ToolDef::read_only(
                "Grep",
                "在工作区内按子串搜索。返回 路径:行号:内容。会看到未提交的改动。",
                serde_json::json!({
                    "type": "object",
                    "properties": {"pattern": {"type": "string", "description": "要搜索的子串"}},
                    "required": ["pattern"]
                }),
            ),
            agentrs_types::ToolDef::mutating(
                "Write",
                "把内容写入工作区内的一个文件（进入未提交的 ChangeSet）。",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "相对路径"},
                        "content": {"type": "string", "description": "文件内容"}
                    },
                    "required": ["path", "content"]
                }),
            ),
            agentrs_types::ToolDef::mutating(
                "Edit",
                "把文件中的 old 全部替换为 new（进入未提交的 ChangeSet）。\
                 后续 Read/Grep 立即能看到替换结果。",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "相对路径"},
                        "old": {"type": "string", "description": "待替换的原文本"},
                        "new": {"type": "string", "description": "替换后的文本"}
                    },
                    "required": ["path", "old", "new"]
                }),
            ),
            agentrs_types::ToolDef::mutating(
                "Delete",
                "删除工作区内的一个文件（进入未提交的 ChangeSet，提交前不动磁盘）。",
                serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string", "description": "相对路径"}},
                    "required": ["path"]
                }),
            ),
        ]
    } else {
        Vec::new()
    };

    let started = host
        .start_with_tools(spec(&model), deps, TurnGuards::default(), tools)
        .await
        .map_err(|e| e.to_string())?;

    started
        .handle
        .submit(UserInput::Message(vec![ContentBlock::text(prompt)]))
        .await
        .map_err(|e| e.to_string())?;

    let summary = started.driver.await;

    println!();
    // **先给答案再给统计。** 只打统计的 CLI 等于跑完不说结果。
    match &summary.final_text {
        Some(t) => println!("{t}\n"),
        None => println!("（本次没有助手文本输出）\n"),
    }
    println!("终止  : {:?}", summary.termination);
    println!(
        "Turn  : {}（其中 0-Step {}）",
        summary.turns, summary.zero_step_turns
    );
    println!("Step  : {}", summary.steps);

    if let Some(sb) = committer {
        let cs = "cs-cli-run";
        let pending = sb.pending_count(cs);
        println!("待提交: {pending} 个文件（未提交时不落盘）");
        if commit && pending > 0 {
            // **这是宿主的动作，不是内核的**——内核只能提议与消费结果。
            let n = sb.commit(cs)?;
            println!("已提交: {n} 个文件");
        } else if pending > 0 {
            println!("提示  : 加 --commit 以落盘");
        }
    }
    Ok(())
}

fn spec(model: &str) -> RunSpec {
    RunSpec {
        run_id: "cli-run".into(),
        parent_run_id: None,
        conversation: ConversationSnapshot::default(),
        system_context: SystemContext {
            sections: vec!["你是一个简洁的助手。需要读写文件时使用 Read / Write 工具。".to_string()],
            workspace_id: Some("default".to_string()),
        },
        authority: AuthorityEnvelope {
            id: "cli".into(),
            workspaces: vec!["default".into()],
            tools: vec!["Read".into(), "Write".into()],
            providers: vec!["openai-compat".into()],
            models: vec![model.into()],
            max_depth: 1,
        },
        initial_capabilities: CapabilityView {
            tools: vec!["Read".into(), "Write".into()],
            providers: vec!["openai-compat".into()],
            models: vec![model.into()],
        },
        permission_mode: PermissionMode::Default,
        model_policy: ModelPolicy {
            tiers: [(agentrs_contracts::spec::ModelTier::Default, model.into())]
                .into_iter()
                .collect(),
            fallback: vec![],
            providers: vec!["openai-compat".into()],
            max_retries: 1,
            allow_attachments: false,
        },
        context_budget: ContextBudget {
            max_input_tokens: 100_000,
            reserved_output_tokens: 4_000,
            compaction_threshold_pct: 80,
        },
        execution_budget: ExecutionBudget::default(),
        checkpoint: None,
        spec_version: SpecVersion(1),
        config: Default::default(),
    }
}

/// 默认 adapter：不落盘、不触达任何真实资源。
struct NullPersistence;

#[async_trait::async_trait]
impl RunPersistence for NullPersistence {
    async fn begin_step(
        &self,
        _e: agentrs_contracts::ids::RunEpoch,
        _i: agentrs_contracts::StepIntent,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        Ok(())
    }
    async fn append_event(
        &self,
        _e: agentrs_contracts::ids::RunEpoch,
        _ev: agentrs_contracts::event::RunEventEnvelope,
    ) -> Result<agentrs_contracts::ids::EventSequence, agentrs_contracts::ports::PersistError> {
        Ok(agentrs_contracts::ids::EventSequence(0))
    }
    async fn finish_step(
        &self,
        _e: agentrs_contracts::ids::RunEpoch,
        _r: agentrs_contracts::StepResult,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        Ok(())
    }
    async fn save_checkpoint(
        &self,
        _e: agentrs_contracts::ids::RunEpoch,
        _c: agentrs_contracts::spec::RunCheckpoint,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("doctor");

    match cmd {
        "doctor" => doctor(),
        "validate" | "trajectory" | "cache-report" | "replay" | "resume" | "export" => {
            let log = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| ".agentrs/events.jsonl".to_string());
            let r = match cmd {
                "validate" => inspect::validate(&log),
                "trajectory" => inspect::trajectory(&log, 20),
                "cache-report" => inspect::cache_report(&log),
                "replay" => inspect::replay(&log),
                "export" => inspect::export(
                    &log,
                    env("AGENTRS_WORKSPACE"),
                    // 盐由调用方给——内核不读随机源（§1.1 边界判据）。
                    &env("AGENTRS_BUNDLE_SALT").unwrap_or_else(|| "default-salt".into()),
                ),
                _ => inspect::resume_plan(&log),
            };
            if let Err(e) = r {
                eprintln!("失败：{e}");
                std::process::exit(1);
            }
        }
        "conformance" => {
            let ws = env("AGENTRS_WORKSPACE").unwrap_or_else(|| ".".to_string());
            if let Err(e) = conformance(&ws).await {
                eprintln!("失败：{e}");
                std::process::exit(1);
            }
        }
        "run" => {
            let use_dev = args.iter().any(|a| a == "dev");
            let prompt = args
                .iter()
                .skip(1)
                .find(|a| !a.starts_with("--") && *a != "dev")
                .cloned()
                .unwrap_or_else(|| "你好".to_string());
            let commit = args.iter().any(|a| a == "--commit");
            if let Err(e) = run(&prompt, use_dev, commit).await {
                eprintln!("失败：{e}");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("未知命令：{other}");
            eprintln!("用法：");
            eprintln!("  agentrs doctor");
            eprintln!("  agentrs run <prompt> [dev] [--commit]");
            eprintln!("  agentrs conformance");
            eprintln!("  agentrs validate|trajectory|cache-report|replay|resume|export [事件日志]");
            std::process::exit(2);
        }
    }
}

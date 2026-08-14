# AgentRS 技术架构设计与实施方案

> 版本：v1.2（补充开源边界、运行轨迹与受控 Component 体系）
>
> 范围：本方案只定义 uworker 的 `agentrs`。它是 Rust 编写、可嵌入的 Agent 推理与工作流内核；保留既有安全边界与 durable workflow，并吸收 DeepSeek Harness/Cordis 的生命周期、依赖拓扑和可撤销注册语义。这里借鉴的是语义，不移植 JavaScript Proxy、任意 YAML 插件或进程内不受信代码。

---

## 1. 定位与边界

AgentRS 的职责是将“用户目标 + 已授权上下文 + 可用能力”推进为可审计的步骤流：模型推理、工具提议、工具结果回灌、上下文管理、子 Agent 和最终结果。它不拥有桌面 UI、数据库、用户身份、策略规则、文件系统写入权限或 OS 进程。

```text
AgentUI                  AgentCore                       AgentRS                    SandboxRS
  用户输入/展示   ->   RunSpec、策略/审批、持久化端口  ->  推理、规划、工具调度  ->  命令隔离、ChangeSet
      ^                        ^                              |                              |
      +---- RunEvent ----------+--------- RunPersistence ------+------ SandboxExecutor --------+
```

依赖方向固定：`agentcore -> agentrs`，`agentcore -> sandboxrs`；AgentRS 只依赖稳定的 `agentrs-contracts` 和 trait port。AgentRS 可以请求工具执行，但不能直接 `spawn`、`fs::write`、访问 SQLite、读取 Keychain 或决定“总是允许”。

| AgentRS 必须负责 | AgentRS 不得负责 |
|---|---|
| provider 无关消息与流、回合循环、模型路由、上下文预算、工具编排、技能加载、子 Agent、摘要与结构化事件 | Electron/HTTP UI、账号和订阅、SQLite/JSONL 实现、工作区授权、审批最终裁决、SandboxGrant 签发、文件提交/删除、进程/网络隔离 |

设计目标：

1. 可嵌入：Core 通过 Rust trait 进程内驱动，不依赖 CLI 或全局配置文件。
2. 可恢复：每个有意义步骤能写 checkpoint，崩溃后从明确边界恢复，不重复未知副作用。
3. 可控：工具仅由 Core 注入的 capability 和 SandboxGrant 执行；AgentRS 不持有越权通道。
4. 高信噪比：主 Agent 上下文只含任务、精选记忆、已批准能力和结构化摘要，不吸收子 Agent 过程推理。
5. 可测：Provider、时间、持久化、工具执行、记忆检索与子 Agent 全部可替换为 fake。
6. 可收敛：Run、Operation、ChildRun 的 live resource 有唯一 owner，取消按 `cancel -> drain -> reverse cleanup` 结束。
7. 可重建：任何模型可见输入均能由不可变 RunSpec、durable event 与内容寻址引用重建。
8. 可解释：一次 Run 的输入来源、模型请求、工具路径、子运行、组件代际和终止原因可以从事实流投影为完整 Trajectory。
9. 可扩展：扩展以声明式 Component 和明确 capability seam 进入系统，默认静态链接或进程外隔离，不向不受信代码开放进程内权限。

## 2. 总体架构

```text
                         +----------------------------+
                         | RuntimeHost                 |
                         | start / resume / cancel     |
                         +-------------+--------------+
                                       |
                         +-------------v--------------+
                         | Composition Kernel          |
                         | scope / owner / generation  |
                         | lifecycle / quiescence      |
                         +-------------+--------------+
                                       | committed view
 +----------------+      +-------------v--------------+      +------------------+
 | ContextManager |----->| AgentEngine                |<-----| ModelRouter      |
 | manifest/build |      | stable turn/step FSM       |      | provider policy  |
 +----------------+      +------+--------------+------+      +--------+---------+
                               |              |                      |
                     +---------v--+    +------v-------+      +-------v--------+
                     | ToolLoop   |    | ChildRunHost |      | ProviderPort    |
                     | fixed pipe |    | derived scope|      | owned stream    |
                     +------+-----+    +--------------+      +----------------+
                            |
             +--------------v--------------------------------+
             | Ports: PolicyEnforcer | SandboxExecutor        |
             | RunPersistence | MemoryRetriever | SkillResolver|
             | Clock | Cancellation | EventSink                |
             +-----------------------------------------------+
                            |
             +--------------v--------------------------------+
             | Event Ledger -> Projection Registry            |
             | Trajectory / Replay / Component Inventory      |
             +-----------------------------------------------+
```

AgentEngine 是唯一拥有可变对话状态的组件，但不是 privileged core。它只消费 operation 开始时冻结的 capability、provider、tool、prompt 和 middleware snapshot；扩展能力不能靠持续修改主循环加入。所有外部 I/O 均通过 port；durable operation 以 intent/result/reconcile 管理，进程内 live resource 以 owner/cleanup 管理，两者不可互相替代。

## 3. Workspace 与 crate 边界

```text
agentrs/
  agentrs-contracts/       公共 ID、DTO、RunEvent、ComponentManifest、projection/port trait
  agentrs-types/           Message、ContentBlock、LlmRequest/Event、ToolDef、Usage
  agentrs-runtime/         RuntimeHost、AgentEngine、composition/inventory/lifecycle、取消和恢复
  agentrs-provider/        Provider trait、流式适配、模型路由、ProviderCompat、重试
  agentrs-context/         token 账本、RequestManifest、请求构建、选择、压缩和来源追踪
  agentrs-tools/           scoped registry、typed pipeline、资源冲突规划、结果规范化
  agentrs-skills/          Skill manifest、按需加载、变量替换、ContextModifier
  agentrs-memory/          MemoryRetriever port adapter、选择协议、引用格式
  agentrs-subagents/       函数式 asTool、profile、摘要 contract、Team 适配端口
  agentrs-prompts/         版本化系统提示、结构化输出 schema、prompt fixtures
  agentrs-observability/   trajectory/projection/replay、trace/metrics、脱敏导出
  agentrs-cli/             轻量命令行驱动器、JSONL host 协议、诊断与测试入口
  agentrs-testkit/         fake ports/capability change、故障调度、回放与属性测试
```

`agentrs-contracts` 不得依赖任何业务 crate。`agentrs-runtime` 依赖 contracts/types/context/tools/provider，但不得依赖 sandboxrs 的实现 crate；SandboxRS 仅通过 `SandboxExecutor` trait 注入。Provider 厂商差异集中在 `ProviderCompat` 和投影器，禁止在 AgentEngine 中出现厂商名分支。P0 将 composition 作为 `agentrs-runtime` 内部模块；只有 tools/provider/subagents 至少三个模块稳定复用同一 API 后，才评估抽出 `agentrs-composition`，且不承诺公开插件 ABI。

### 3.1 AgentRS CLI：轻量驱动器，不是第二个产品

`agentrs-cli` 是薄二进制模块，面向开发者、自动化和宿主集成测试。它负责把命令行参数或 JSONL 输入翻译为 `RunSpec`，装配 RuntimeHost，并把 `RunEvent` 原样输出；它不实现 TUI、账户、会话数据库、插件市场或桌面工作台。

```text
stdin / args -> CliAdapter -> RuntimeHost -> RunEvent -> stdout JSONL / human renderer
                     |              |
                     |              +-> injected Provider/Persistence/Policy/Sandbox ports
                     +-> config/profile validation only
```

CLI 必须复用 RuntimeHost 和 contracts，禁止维护第二份 loop、工具协议或权限模型。默认采用只读/模拟 adapter；要执行真实工具时必须显式指定由 AgentCore/SandboxRS 提供的受控 adapter，且 CLI 不能自行退化为直接执行宿主 Shell 的后门。

建议命令面：

```text
agentrs run "<prompt>" [--model <id>] [--workspace <id>] [--json]
agentrs serve --jsonl                         # stdin command -> stdout RunEvent，供宿主驱动
agentrs resume <run-id> --checkpoint <path>   # 仅在提供 Persistence adapter 时可用
agentrs validate config|provider|skill <path>
agentrs replay <events.jsonl>                 # 回放/诊断，不调用模型或工具
agentrs trajectory <run-id> [--json]          # 投影 Run/Turn/Step/Tool/Child 轨迹
agentrs components [--scope <id>]             # 组件、依赖、代际、生命周期快照
agentrs doctor                                # 版本、capability、provider 与 adapter 健康检查
```

`run` 在无受控执行 adapter 时只允许无工具或只读 fake 工具；`--unsafe` 不作为常规参数提供。开发环境若确需真实 Sandbox，使用明确的 `--sandbox-endpoint` 或宿主签发的短期 capability 文件，并在 JSONL 事件中标识执行器与 grant 来源。

### 3.2 开源与产品边界

建议以 AgentRS Kernel 为开源单元，开放 contracts/types、runtime/composition、trajectory/projection/replay、Component SDK、testkit、CLI 和参考 adapter。开放这些承重契约有利于宿主集成、第三方 Provider/Tool 适配和对恢复/生命周期不变量的外部审查。

AgentCore 的账号、计费、组织策略、凭据、插件市场后台、签名服务、商业路由和私有评测不进入 AgentRS 仓库；SandboxGrant 签发与工作区最终授权仍归 Core，OS 隔离与 ChangeSet 执行仍归 SandboxRS。开源不改变进程权限边界，也不意味着任意插件可以进入 AgentRS 进程。

发布前必须完成依赖许可证、NOTICE、商标和代码来源审计。DeepSeek Harness/Cordis 仅作为设计与测试语义参考；AgentRS 默认采用独立的 Rust 实现，并保留必要归属信息。

## 4. 核心契约

### 4.1 启动与恢复

```rust
pub struct RunSpec {
    pub run_id: RunId,
    pub parent_run_id: Option<RunId>,
    pub conversation: ConversationSnapshot,
    pub system_context: SystemContext,
    pub authority: AuthorityEnvelope,
    pub initial_capabilities: CapabilityViewRef,
    pub model_policy: ModelPolicy,
    pub context_budget: ContextBudget,
    pub execution_budget: ExecutionBudget,
    pub checkpoint: Option<RunCheckpoint>,
}

pub trait RuntimeHost: Send + Sync {
    async fn start(&self, spec: RunSpec) -> Result<RunHandle, AgentError>;
    async fn resume(&self, spec: RunSpec, checkpoint: RunCheckpoint) -> Result<RunHandle, AgentError>;
    async fn cancel(&self, run_id: RunId, reason: CancelReason) -> Result<(), AgentError>;
}
```

`RunSpec` 是不可变快照。`AuthorityEnvelope` 是 Core 签发且 Run 内不可扩大的授权上界；`CapabilityView` 是上界内当前可用的 provider/tool 集合，可以收缩、失效或在安全边界换代。工作区身份不能原地切换；策略、prompt 或能力变化不得静默混入进行中的 operation。

```rust
pub struct ResolvedCapability<T: ?Sized> {
    pub provider_id: ProviderId,
    pub generation: Generation,
    pub value: Arc<T>,
    pub availability: Availability,
}

pub struct OperationView {
    pub authority_id: AuthorityEnvelopeId,
    pub capability_digest: CapabilityViewDigest,
    pub provider_generation: Generation,
    pub tool_catalog_generation: Generation,
    pub middleware_generation: Generation,
    pub prompt_generation: Generation,
}

pub struct ModelRequestManifest {
    pub request_id: RequestId,
    pub operation_view: OperationView,
    pub source_event_range: EventRange,
    pub system_sections: Vec<ContentRef>,
    pub memory_fragments: Vec<MemoryRef>,
    pub skill_fragments: Vec<SkillRef>,
    pub compaction_refs: Vec<SummaryRef>,
    pub tool_catalog_digest: Digest,
    pub token_accounting: TokenAccounting,
}
```

每个 operation 只使用一个 committed `OperationView`。P0 可以只在 Run 启动时生成 view，但身份、代际和 digest 字段必须预留；P1 才实现安全边界上的 generation replacement。

### 4.2 外部端口

```rust
pub trait RunPersistence: Send + Sync {
    async fn begin_step(&self, intent: StepIntent) -> Result<StepId, PersistError>;
    async fn append_event(&self, event: RunEvent) -> Result<EventSequence, PersistError>;
    async fn finish_step(&self, result: StepResult) -> Result<(), PersistError>;
    async fn save_checkpoint(&self, checkpoint: RunCheckpoint) -> Result<(), PersistError>;
}

pub trait PolicyEnforcer: Send + Sync {
    async fn evaluate(&self, proposal: ToolProposal) -> Result<PolicyDecision, PolicyError>;
    async fn await_approval(&self, request: ApprovalRequest) -> Result<ApprovalDecision, PolicyError>;
}

pub trait SandboxExecutor: Send + Sync {
    async fn execute(&self, grant: SandboxGrant, request: ExecutionRequest)
        -> Result<ExecutionResult, SandboxError>;
    async fn cancel(&self, execution_id: ExecutionId) -> Result<(), SandboxError>;
    async fn reconcile(&self, execution_id: ExecutionId) -> Result<ExecutionStatus, SandboxError>;
}

pub trait MemoryRetriever: Send + Sync {
    async fn candidates(&self, query: MemoryQuery) -> Result<Vec<MemoryCandidate>, MemoryError>;
    async fn load(&self, ids: &[MemoryId]) -> Result<Vec<MemoryFragment>, MemoryError>;
}
```

端口的责任必须单一：Persistence 是事实记录，不执行策略；Policy 是授权决策，不运行命令；Sandbox 是限制执行，不规划任务；Memory 是检索，不直接注入提示词。AgentRS 组合这些结果，但不绕过其中任何一个。

每个 capability seam 都要明确 Definition、Provider、Consumer，并回答：注册/撤销方式、owner、generation 变化时的 in-flight 策略、失败传播、durable 数据、ChildRun 收窄规则以及是否存在绕过 Policy/Sandbox 的 alternate caller。核心 port 继续使用强类型 trait 字段；composition registry 只维护 identity、generation、scope、availability 和 owner，不引入字符串 Service Locator 或 `Any` 驱动的公共 API。

| Seam | Definition | Provider | Consumer |
|---|---|---|---|
| LLM | `ProviderPort`、message/stream types | provider adapters | ModelRouter/AgentEngine |
| Tool | `ToolDef`、execution/result contract | Core/Sandbox/MCP registrations | ToolLoop/request assembler |
| Persistence/Policy | event/checkpoint、proposal/decision | AgentCore adapters | RuntimeHost/RecoveryPlanner/ToolLoop |
| Context sources | memory/skill/prompt refs | Core resolvers/scoped contributors | ContextManager |
| Subagent | ChildRunSpec/summary | RuntimeHost/Core worker adapter | ToolLoop/main Agent |

### 4.3 事件模型

`RunEvent` 是 AgentRS 的权威 durable facts 流和唯一对外语义输出。事件 envelope 至少包含 `run_id`、单调 `seq`、`event_id`、时间戳、可见性和 durability；需要建立因果关系的事件还携带 `trace_id/parent_event_id`、`turn_id/step_id/operation_id`、`scope_id/component_id/generation` 与 `request_manifest_id`。未知事件必须可安全忽略。

```text
RunStarted | ContextSelected | ModelRequestPrepared | TurnStarted | TextDelta | ThinkingDelta
ToolProposed | ApprovalRequested | ToolStarted | ToolCompleted
ChangeSetAvailable | ArtifactCreated | SubagentSummary | UsageUpdated
CompactionStarted | CompactionCompleted | Checkpointed
RunCompleted | RunFailed | RunCanceled | RunNeedsUserAction
```

事件分为三类：

| 类别 | 示例 | 规则 |
|---|---|---|
| Durable facts | `StepIntent`、审批、`StepResult`、`ModelRequestPrepared`、终态 | 必须追加持久化，是恢复与回放依据 |
| Durable content refs | assistant committed message、tool result、summary/artifact ref | 保存 hash、版本与来源范围，正文可由 Core 加密存储 |
| Live stream/telemetry | frame timing、queue depth、`TextDelta`、lifecycle notification | 可采样或丢失；UI delta 必须有 committed replacement 或明确 partial record |

`TextDelta` 不单独作为完整恢复依据。任何进入下一模型请求的内容都必须可由 RunSpec、durable events 与内容寻址引用重建。终态之后禁止产生新的模型或工具语义事件；迟到的 live frame 只能丢弃或记录为诊断。

### 4.4 Trajectory 与 Projection Registry

Trajectory 不是日志搜索页，而是从权威 Event Ledger 计算出的版本化读模型。AgentRS 提供事件、纯投影定义、查询/分页、replay bundle 和脱敏导出协议；AgentUI/AgentCore 负责桌面渲染和存储实现。投影不得成为第二份可变事实源。

```rust
pub trait ProjectionDefinition: Send + Sync {
    type State: Serialize + DeserializeOwned;
    type View: Serialize;

    fn key(&self) -> ProjectionKey;
    fn state_version(&self) -> u32;
    fn init(&self) -> Self::State;
    fn apply(&self, state: &mut Self::State, event: &RunEventEnvelope);
    fn view(&self, state: &Self::State) -> Self::View;
}
```

首批投影包括：Run/Turn/Step 树、模型请求与 TTFT/usage、工具 proposal 到 Policy/Approval/Sandbox/StepResult 的路径、ChildRun 父子关系、上下文来源、compaction、终态，以及影响模型可见能力的 component generation 变化。查询必须支持按 seq/time、event/tool/component/error/generation 过滤和向前分页。

系统维持三类轨迹，不能混成一个无限增长日志：

| 轨迹 | 内容 | 保留规则 |
|---|---|---|
| Agent Trajectory | turn/request/tool/approval/compaction/child/terminal | durable，可重放 |
| Composition Trajectory | component 激活、撤回、换代、依赖失效、shutdown settlement | 影响能力或审计的变化 durable，其余 live |
| Operational Telemetry | frame timing、queue depth、内部 retry 等 | 可采样，不参与恢复 |

## 5. Runtime Composition 与结构化生命周期

AgentRS 同时维护两个时间平面：

```text
Live plane:    acquire/register/start -> owner -> cancel/drain/reverse cleanup
Durable plane: StepIntent -> external operation -> StepResult/reconcile
```

Live plane 管理 tool/middleware/prompt registration、provider stream、background task、approval waiter、temporary section 和 ChildRun；durable plane 管理不可逆或状态未知的外部动作。`cleanup/release` 只释放当前 live resource，不宣称历史未发生；`reconcile/compensate` 处理外部 operation，不能由 `Drop` 或 disposer 替代。

### 5.1 Scope、Component 与 Owner

作用域固定为：

```text
ProcessScope -> HostScope -> RunScope -> OperationScope
                            +---------> ChildRunScope -> Child OperationScope
```

每个 ToolDef、middleware、prompt contributor、memory view、listener、stream 和 child handle 都带 `ScopeId` 与唯一 owner。父 owner 停止时先阻止新注册并传播 cancellation，再等待 child/in-flight operation 到达 settlement，最后反序 cleanup 自身资源。

```rust
pub enum LifecycleState {
    Pending,
    Starting { generation: Generation },
    Active { generation: Generation },
    Stopping { generation: Generation },
    Failed { generation: Generation, code: ErrorCode },
    Disposed,
}

#[async_trait]
pub trait AsyncCleanup: Send {
    async fn cleanup(self: Box<Self>) -> Result<(), CleanupError>;
}
```

`ResourceOwner` 必须满足：setup 成功后的 cleanup 在资源可见前登记；部分 setup 失败反序清理；`Stopping` 后拒绝逃逸注册；cleanup failure 汇总但不阻断其余 cleanup；并发多次 shutdown 幂等并共享 settlement 结果。RAII 继续负责局部同步资源，async owner 负责取消、drain、错误收集和有序清理。

### 5.2 Component Manifest 与 Inventory

运行时正式使用 `Component` 表达扩展单元；“插件”只是它的一种交付方式。P0 的 `ComponentManifest` 至少包含：`component_id/version/api_version`、`requires/provides`、config schema、execution mode、trust level、scope/resource policy、migration version 与 telemetry/redaction policy。

`ComponentInventory` 是 Composition Kernel 的只读投影，每次读取权威 registry/owner 状态，不维护第二份生命周期真相。快照至少展示 source/profile、scope/owner、lifecycle、generation、dependencies、registrations、in-flight operation 数、last failure 和 shutdown settlement；P0 只读，不提供绕过宿主授权的 enable/install API。

### 5.3 分级扩展与配置组合

| 级别 | 形态 | 阶段 | 规则 |
|---|---|---:|---|
| Builtin Component | 静态链接 Rust component | P0 | 受信、强类型、随发行版测试 |
| External Component | MCP/WASI/JSON-RPC adapter | P1 | Core/Sandbox 管进程、凭据和资源限制 |
| Trusted in-process extension | 已编译且受信实现 | P2 评估 | 只有明确需求和稳定 API 后开放 |
| Native dynamic plugin | 任意 Rust 动态库 | 不采用 | ABI、崩溃隔离和供应链成本过高 |

P1 可增加 profile/bundle/overlay，但配置必须是经过 schema 校验的声明式数据；禁止 `!!js`、可执行 YAML 和模型生成后直接加载代码。Profile 只选择已安装 catalog 中的 Component，权限仍受 AuthorityEnvelope 限制。

### 5.4 P0 与后续边界

P0 实现 owner、scope、manifest、只读 inventory、固定 component seam 和 quiescent shutdown，不实现通用动态插件 loader。P1 在真实需求出现后增加 committed generation 与事务式换代：解析 manifest/config，验证依赖/权限/API，私下启动 candidate，执行 health/invariant checks，提交后让新 operation 使用新 generation，再 drain/cleanup 旧 generation；任一步失败都保留或恢复旧 generation。P2 才评估受信进程内扩展和插件市场控制面。

## 6. 可恢复 Agent Loop

每轮遵循同一骨架，但将可能失败或产生副作用的边界显式化：

```text
start/resume
  -> create RunOwner + restore checkpoint + validate AuthorityEnvelope
  -> commit OperationView
  -> assemble ModelRequestManifest from durable sources
  -> persist ModelRequestPrepared + TurnStarted
  -> bind provider.stream(request) to OperationOwner
  -> persist/emit model deltas
  -> Final              => save assistant result + checkpoint + RunCompleted
  -> ToolRound          => for each proposed tool:
       persist StepIntent(input_hash)
       Policy.evaluate
       Allow             => Sandbox.execute(grant, request)
       RequireApproval   => persist ApprovalRequested -> await decision -> execute
       Deny              => tool result error, feed back to model
       persist StepResult + artifact/change-set refs
       feed compact result back to conversation
  -> settle OperationOwner + checkpoint -> next turn
```

### 6.1 状态机和恢复规则

| 边界 | 持久记录 | 恢复策略 |
|---|---|---|
| 模型请求前 | `ModelRequestManifest` + `TurnStarted` | manifest 可重建且尚未输出可见文本时才允许重试 |
| 工具执行前 | `StepIntent` + input hash + grant reference | 重启后先向 Sandbox `reconcile`，不盲目重跑 |
| 等待审批 | `ApprovalRequested` | 继续等待、过期或由 Core 取消 |
| 执行完成 | `StepResult` + artifact/change-set ref | 将结果回灌，绝不再次执行 |
| 压缩完成 | summary + source event range | 复用摘要，不重新压缩同一范围 |

重试只允许两类情形：模型在无可见输出前的瞬时失败，或 Sandbox 明确返回“未开始执行”。任何未知副作用、部分写入或外部系统超时都进入 `NeedsUserAction` 或 Core 的 reconcile 流程。

恢复时先重建 durable projection，再重新创建 live composition；不序列化 listener、task、Arc 或 disposer。若 checkpoint 已引用但 durable log 尚未提交对应事实，以 durable log 为准；若已有 UI delta 但无 committed assistant 内容，按已记录的 partial-output policy 结束、续接或请求用户确认，禁止静默重发。

### 6.2 回合防护

引擎内置确定性 guard，不依赖模型自觉：最大回合、最大工具调用、最大并行数、预算、重复工具调用指纹、连续失败、空响应、上下文硬上限和取消令牌。触发后输出结构化 `RunNeedsUserAction` 或 `RunFailed`，不能无限循环。

## 7. Provider 与模型路由

继承 aionrs 的 `LlmProvider::stream(LlmRequest)` 统一流式模型，消息模型保留 provider 私有元数据以保证工具调用和 reasoning 签名可 round-trip。Provider transport、参数字段、工具 wire format、reasoning、token 上限和图像能力统一由 `ProviderCompat` 数据配置表达。

```text
provider defaults -> provider profile -> model profile -> RunSpec override
                         -> validated ProviderCompat -> request projector
```

模型路由不允许由 Agent 任意指定。`ModelPolicy` 由 Core 传入，约束可选模型、场景、成本上限、fallback、数据驻留和是否可传附件。推荐档位：

| 档位 | 用途 | 权限 |
|---|---|---|
| `lite` | memorySelector、风险提示、工具/技能搜索、简单分类 | 零工具 |
| `default` | 主任务、计划、探索、压缩 | 仅继承的 capability |
| `craft` | 高价值复杂任务 | 仅继承的 capability，成本预算更严格 |

Provider 失败编码为稳定错误，不透传响应正文和密钥。流式重试只在未产生可见文本时进行；发生 rate limit 时输出可恢复状态和 `retry_after`，由 Core 决定排队或切换模型。

Provider adapter 必须有 `ProviderId`、`Generation` 和 owner；stream/continuation lease 归 OperationOwner。一次请求不得跨 generation，replacement 只影响下一安全边界。旧 adapter 必须保留到其 stream 完成、取消或 reconcile 结束，不能用 `ArcSwap` 静默替换正在使用的依赖。

## 8. 工具调度、Typed Pipeline 与 SandboxRS 协作

AgentRS 管理“模型知道哪些工具、何时提出调用、哪些可并行、如何把结果回灌”；SandboxRS 管理“命令实际上如何受限执行并变成 ChangeSet”。工具定义至少包含 schema、风险、是否 deferred、`EffectProfile/ResourceAccess`、重试/取消语义和产物策略。

```text
ToolUse from model
  -> validate schema / normalize proposal
  -> resolve scoped ToolDef + committed catalog generation
  -> capability/cancellation guard
  -> persist StepIntent
  -> PolicyEnforcer evaluates -> approval if required
  -> timeout/retry/metrics typed middleware
  -> Core grants SandboxGrant -> SandboxExecutor
  -> normalize/redact/artifact -> persist StepResult
  -> deterministic ToolResult order -> next model turn
```

规则：

1. AgentRS 不将模型原始字符串当 Shell 命令执行，必须先匹配注册的 `ToolDef` 并验证 input schema。
2. 未声明 EffectProfile 的工具按最保守 exclusive 模式调度；`read/write` 只作为 P0 默认分类，不作为并行安全证明。
3. `ChangeSetAvailable` 是一等事件。AgentRS 可以理解 diff 和请求下一步，但不能提交 ChangeSet；提交由 Core/用户流程完成。
4. 工具返回值先做大小限制、脱敏和 Artifact 化，再进入上下文；保留与本任务有关的摘要和引用。
5. deferred 工具先只向模型暴露名称、描述、风险和权限摘要，只有 `ToolSearch` 选中后才加载完整 schema。
6. schema、capability/grant、`StepIntent`、Sandbox enforcement、`StepResult` 是不可绕过的固定阶段；middleware 只能扩展 timeout、受限 retry、metrics 和结果转换，不能承担最终授权。
7. tool/middleware registration 必须带 scope、owner、稳定 order 并返回可撤销 handle；新 operation 只看最新 committed snapshot，旧 operation 按旧 snapshot 完成或明确取消。

资源访问模式至少包括 `SharedRead`、`ExclusiveWrite`、`CommutativeAppend` 与 `RateLimited`。两个调用仅在资源集合不冲突、不共享 exclusive lease、Policy 与预算允许且失败不会撤销另一 grant 时并行；否则按稳定 proposal order 串行。并发执行结果必须全部 settlement，并按原始 call order 写入模型历史。

## 9. 上下文、记忆与压缩

### 9.1 上下文预算

`ContextManager` 持有显式 token 账本，将可用窗口分配给：系统规则、用户输入、记忆、技能、工具 schema、历史消息、近期工具结果、输出预留和压缩缓冲。任何注入源都必须声明 token 成本和优先级，不能无限追加。

每次请求必须先产出 `ModelRequestManifest`，记录 request ID、provider/prompt/tool/capability generation 或 digest、source event range、system/skill/memory/summary content refs、token accounting。未进入 durable fact 或内容寻址存储的纯内存内容不得进入请求 assembler。

优先级从高到低：安全/能力约束、用户当前输入、未决审批/当前任务、最近对话、关键文件/产物引用、精选记忆、技能正文、工具 schema、低优先级历史。

### 9.2 记忆选择

AgentRS 不直接管理 SQLite FTS 或 Markdown 文件；它通过 `MemoryRetriever` 获取候选，然后使用 `memorySelector`（lite、零工具、结构化 JSON 输出）在候选中选择默认最多 5 条。selector 只能降低注入数量，不能扩大可见范围；不确定时少选。检索/selector 失败时退回 Core 给出的确定性排名和最小记忆集。

### 9.3 压缩策略

采用四段而不是单次截断：

1. 工具输出整理：去 ANSI、去重复、JSON/表格压缩、ArtifactRef 替代大输出。
2. Microcompact：清理早期已消费的工具全文，保留引用、ChangeSet、错误和最近结果。
3. Compact：接近阈值时由零工具 Agent 生成结构化摘要，替换早期历史。
4. ContextSummary：恢复、跨 Run 延续或重大切换时生成完整工作摘要。

摘要必须包括原始意图、约束、授权范围、关键文件/产物、已做/待做任务、错误修复、未决审批、当前 ChangeSet 和下一步；记录 `summary_version` 与 source event range，禁止反复压缩同一段历史造成信息漂移。

## 10. 技能与提示词

AgentRS 只解析和执行由 Core 解析后提供的 `SkillManifest` 与内容包。技能可提供指令、参数、模板、参考资料、模型/工具缩减和 fork 运行模式；它不是权限插件。`ContextModifier` 以 derived configuration layer 合并：集合取交集、预算取最小、风险策略取更严格、可见范围只能缩小，不能使用普通“右侧覆盖”扩大父 Run 的 AuthorityEnvelope。

系统提示、子 Agent 提示、压缩提示和 tool result 提示均放入 `agentrs-prompts`，具备版本号、输入 schema、snapshot fixture 和安全测试。禁止将产品文案、数据库路径、UI 控制逻辑嵌入 prompt。

## 11. 子 Agent 架构

### 11.1 函数式 asTool（P1）

`Explore`、`Plan`、`memorySelector`、`ToolSearch`、`compact`、`contextSummary`、`promptHookEvaluator` 是函数式子 Agent：输入 -> 独立受限上下文 -> schema 化结果。它们没有持久身份、默认零工具、不能直接发送消息或创建命令。

主 Agent 只能获得 `SubagentSummary`：任务、结论、证据/ArtifactRef、风险、置信度和建议下一步。过程推理、草稿、token 流与原始上下文不自动回灌，避免上下文膨胀和注意力稀释。

### 11.2 协作式 Team Worker（P2）

AgentRS 只提供 `ChildRunSpec`、AgentProfile、摘要 contract 和取消传播；持久 Worker、TaskList DAG、租约、消息和 artifact 共享属于 Core。ChildRun 通过 `ScopeDerivation` 从父 scope 构造，模型、工具、预算、可见性和递归深度只能收窄；child registration 归 ChildRunOwner，父取消先传播并等待 child terminal，child 结束后其 tool/listener/prompt 不得残留。

## 12. 可观测性与安全要求

每个模型调用、回合、工具提议、子 Agent 和压缩操作携带 trace ID、run ID、turn ID、step/operation ID、component/scope/generation、模型档位、token/cost 和结果码。AgentRS 输出脱敏结构化指标，严禁在普通 telemetry 中记录 prompt 正文、文件内容、命令输出、密钥、绝对路径或 provider 原始响应。

Durable facts、UI stream 与 live lifecycle telemetry 使用不同事件类别和保留策略。Composition 诊断可以展示 component/scope/owner/generation/state，但不能把每个 lifecycle tick 混入恢复日志。模型输入审计以 manifest 的 ref/hash 为准，不要求普通 telemetry 保存敏感正文。

Trajectory 支持一致的 `as_of_seq` 快照、向前分页和窗口化查询；长 Run 不得要求一次加载全部事件。Replay bundle 默认只包含 schema 版本、脱敏事件、content hashes/可选 refs 和 projection versions，敏感正文必须由 Core 按可见性与用户授权另行导出。接收端以 `(run_id, seq)` 去重，不把 telemetry 缺口解释成 durable event 丢失。

安全不变量：

1. 无注册 ToolDef 和 schema 验证，不得产生工具请求。
2. 无 PolicyDecision 和 SandboxGrant，不得发生工具执行。
3. 无 `StepIntent`，不得跨出有副作用的调用边界。
4. 无 `StepResult` 或 Sandbox reconcile 结果，恢复时不得重放工具执行。
5. 子 Agent 的工具、模型和通信能力只能收窄。
6. 模型输出和 Hook 评估只能建议动作，不能替代 Core/Sandbox 的确定性安全检查。
7. AuthorityEnvelope 在 Run 内不可扩大，CapabilityView 的变化只能收缩或在安全边界换代。
8. 每个 registration 和 async task 都有 owner；Run 终态前必须完成 quiescent settlement 或输出明确 shutdown failure。
9. 每个模型请求都有关联的可重建 `ModelRequestManifest`，一次 operation 不混用 generation。

## 13. 测试策略

| 范围 | 必测内容 |
|---|---|
| contracts | schema 兼容、未知事件降级、ID/seq 单调、manifest/digest 稳定性 |
| trajectory | causal envelope、纯投影确定性、stateVersion 失效、分页稳定性、replay/export 脱敏 |
| lifecycle | setup 中途失败反序清理、并发 dispose、cleanup failure/timeout、Stopping 拒绝注册 |
| component | manifest/config 校验、依赖缺失、inventory 权威性、candidate 失败保留旧代 |
| runtime | 状态机属性测试、cancel/drain/cleanup 顺序、checkpoint 恢复、live/durable 重建 |
| provider | frame 解析、metadata round-trip、未输出重试、generation replacement/drain |
| tools | schema/deferred、scoped registration、pipeline 顺序、资源冲突、确定性结果提交 |
| context | token 账本、request 重建、generation digest、压缩 source range、摘要回归 |
| subagents | derived scope、零工具 profile、摘要 schema、父子取消与资源清理 |
| integration | fake Core/Sandbox：审批、ChangeSet、reconcile、崩溃恢复、依赖交错 |
| evaluation | 固定任务集：文件整理、文档生成、失败恢复、拒绝危险动作、长上下文 |

测试必须以 fake port 为主，真实 Provider/MCP/Sandbox 仅作本地 smoke。每次 prompt、ProviderCompat、压缩模板或工具 schema 修改都跑 snapshot/eval 回归。

必须增加 deterministic interleaving scheduler：随机交错 setup/cancel/provider replacement/tool settlement，验证 operation 不跨 generation、旧 provider 在 consumer settlement 前不释放、dispose 后无 stale registration/duplicate callback。对无冲突 operation 做 property test，打乱执行顺序后最终 durable projection 应一致。

## 14. 分阶段实施

### Phase A：Contracts、生命周期、轨迹基础与最小循环（3 周）

- 建立 workspace 和 `agentrs-contracts/types/runtime/testkit`。
- 定义 AuthorityEnvelope/CapabilityView、causal event envelope、RequestManifest、StepIntent/Result、Checkpoint 与 port trait。
- 实现 Run/Operation/Child owner、ComponentManifest、只读 Inventory、async cleanup stack、部分 setup 回滚与 quiescent shutdown。
- 实现 Projection Registry、Run/Turn/Step/Tool 基础轨迹和 CLI `trajectory/components/replay`。
- 实现单 Provider fake 的流式 loop：operation committed view、文本 Final、单工具 ToolRound、取消、最大回合。
- 接入 fake Persistence/Policy/Sandbox，建立确定性时钟和事件回放测试。
- 实现 `agentrs-cli serve --jsonl`、`replay` 与 `doctor`，仅装配 fake/read-only adapter。

验收：测试 Run 能稳定输出事实流并投影完整基础轨迹；manifest 可重建模型输入；Inventory 与 registry 状态一致；退出时 live resource 全部 settlement，崩溃后可从 checkpoint 恢复。

### Phase B：Provider、工具管线与恢复（3-4 周）

- 落地 `LlmProvider`、ProviderCompat、OpenAI/Anthropic 优先适配和模型路由。
- 实现 scoped/owned ToolRegistry、typed middleware、deferred ToolSearch、EffectProfile 资源冲突规划与结果裁剪。
- 与 Core/SandboxRS 接通 Policy -> SandboxGrant -> ExecutionResult/ChangeSetAvailable。
- 实现模型无输出重试、工具 reconcile、重复调用 guard、三段 checkpoint。
- 完成 CLI `run`、`resume`、`validate`；真实执行只能经显式 Core/Sandbox adapter，不增加直连 Shell 路径。
- 为轨迹补齐 provider timing、Policy/Approval、Sandbox、StepResult、generation 和 reconcile 因果链。

验收：AgentRS 不含 OS 命令执行代码；一次 operation 不跨 generation；安全固定阶段不可被 middleware 绕过；崩溃后不重复未知工具副作用。

### Phase C：可重建上下文、技能与专用 Agent（3-4 周）

- 实现 token 账本、工具输出整理、Microcompact、Compact、ContextSummary。
- 接入带 provenance/content ref 的 MemoryRetriever、memorySelector、SkillManifest、单调收窄 ContextModifier。
- 实现 Plan/Explore/Hook evaluator 等函数式子 Agent、derived scope 与摘要 contract。
- 建立 prompt 版本、snapshot、长上下文和中文任务 eval 集。

验收：主 Agent 不接收子 Agent 过程推理；长任务可在预算内维持授权、ChangeSet 和未决事项；记忆选择可解释且可降级。

### Phase D：受控扩展、generation replacement、多 Agent 与生产强化（持续）

- 实现声明式 profile/overlay、candidate generation 事务式启动、safe-boundary replacement 与旧 provider drain。
- 增加 MCP/WASI/JSON-RPC External Component；进程、凭据与资源限制仍由 Core/Sandbox 管理。
- 实现 ChildRunSpec、parent-owned 生命周期、取消传播、预算归集、受限 fork。
- 与 Core TaskList DAG 接通协作式 Team Worker。
- 扩展 Provider、模型 fallback、成本路由、MCP/Skills 兼容矩阵。
- 性能剖析、压力测试、故障注入、可观测性与发布兼容性治理。

AgentUI 可在本阶段基于 AgentRS projection contract 实现虚拟化 Trajectory Table/Timeline、搜索、局部检查器和插件库存页面；这些 UI 不进入 AgentRS crate。

验收：子 Run 无扩权、无上下文泄漏、可追踪成本和摘要；Core/Sandbox 重启、模型失败和审批超时都有确定终态。

## 15. 首批 ADR

1. AgentRS 是可嵌入 Rust 内核，不提供 UI、数据库或直接 OS 工具执行。
2. AgentRS 仅通过 trait port 与 Core、SandboxRS、存储、记忆和策略交互。
3. Provider 差异由 `ProviderCompat` 数据驱动，禁止在核心 loop 硬编码厂商分支。
4. 所有副作用先写 `StepIntent`，恢复时优先 reconcile，禁止盲目重放。
5. `ChangeSetAvailable` 是一等事件；AgentRS 可推理但不可提交用户文件变更。
6. 工具按 schema、scope 和 owner 注册；并发由 ResourceAccess 冲突模型保守判定，deferred 工具按需加载。
7. 主上下文不自动包含子 Agent 推理过程；只接收结构化摘要和引用。
8. memorySelector、compact、contextSummary、hook evaluator 固定零工具且使用受控模型预算。
9. 压缩摘要必须带 source range/version，避免摘要漂移和重复压缩。
10. 每个 AgentRS release 必须通过 contracts、状态机、恢复、上下文和安全 eval 门禁。
11. AgentRS CLI 仅是 RuntimeHost 的薄驱动器，复用事件/端口协议；默认只读或模拟，不能绕过 Core/SandboxRS 执行真实命令。
12. AgentRS 采用 live cleanup 与 durable recovery 双时间平面，两者不可替代。
13. AuthorityEnvelope 与 CapabilityView 分离；ChildRun 只能派生更窄 scope。
14. 每个 operation 固定 committed dependency generation，变化只在安全边界生效。
15. 所有模型可见输入必须由 RunSpec、durable events 和 content refs 重建。
16. 所有 tool/middleware/prompt/listener/task registration 都有 owner，并支持幂等 async shutdown。
17. P0 不提供任意进程内动态插件、动态库 ABI 或不受信 self-modification。
18. Trajectory 是 Event Ledger 的版本化纯投影，不是第二份事实源；AgentRS 提供协议和查询，AgentUI 负责渲染。
19. 扩展单元统一为 Component；P0 静态链接，P1 优先进程外，profile 只能选择已安装且经授权的 catalog。
20. Component Inventory 只读权威 runtime 状态；安装、签名、市场和最终启停授权属于 AgentCore 控制面。
21. AgentRS Kernel 可独立开源；商业账号、计费、组织策略、凭据和市场后台不进入内核边界。

## 16. 完成标准

AgentRS 完成 P0/P1 的判断不是“能调用模型和工具”，而是同时满足：

- 嵌入 Core 后不读写其私有存储，也不依赖全局配置；
- 工具请求经过 schema、Policy 和 Sandbox 三道边界；
- 任意中断后能明确恢复、reconcile 或停在 `NeedsUserAction`，不重复未知副作用；
- 上下文有硬预算，记忆/技能/工具/子 Agent 结果均可选择、裁剪和追溯；
- 事件、checkpoint、prompt、ProviderCompat 和工具 schema 均有可回放测试；
- 不会通过任何直接进程或文件 API 绕过 SandboxRS 的 ChangeSet/回收区保护；
- CLI 的 JSONL 流与嵌入 API 产生同构事件，且没有任何默认可写的本地执行路径。
- Run、Operation 与 ChildRun 取消后无残留 stream/task/listener/registration，cleanup failure 有可诊断 settlement。
- 任意模型请求可由 manifest 指向的 durable facts/content refs 重建，UI delta 丢失不影响 committed transcript 回放。
- capability/provider/tool/prompt/middleware 换代不会让单次 operation 混用 generation，也不会突破 AuthorityEnvelope。
- Run/Turn/Step/Tool/Child/Component 的关键路径可由事件投影、分页查询和脱敏 replay bundle 解释，投影版本变化能安全失效旧缓存。
- Component manifest、依赖、配置、scope、owner 和 generation 可诊断；candidate 失败不会破坏正在服务的 generation。
- 开源仓库不包含产品密钥、商业策略或绕过 AgentCore/SandboxRS 的默认执行路径，并完成发布许可证与来源审计。

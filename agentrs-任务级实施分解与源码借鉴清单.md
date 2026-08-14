# AgentRS 任务级实施分解与源码借鉴清单

> 配套文档：[AgentRS 技术架构设计与实施方案](agentrs-技术架构设计与实施方案.md)
>
> 版本：v1.2（补充开源边界、Trajectory/Projection 与受控 Component 体系）
>
> 目标：将 AgentRS 拆为可排期、可验收、可并行的开发任务；逐项列明参考项目的源码模块与所借鉴的设计思想。本文的“借鉴”表示吸收架构模式，不表示复制实现或继承其安全边界。

---

## 1. 范围、原则与依赖图

AgentRS 是 uworker 的推理与工作流内核。它与 SandboxRS 并列，不能直接执行宿主命令、直接读写用户工作区或提交 ChangeSet；它通过 Core 注入的 trait port 请求执行、记录状态和等待审批。

```text
T01 contracts/types
  +-> T02 ports/testkit
       +-> T03 event/checkpoint -> T03B trajectory/projection -+
       +-> T03A composition/lifecycle --------------------------+-> T04 runtime loop
                                             +-> T05 provider
       +-> T07 scoped tool registry --------+-> T07A generation/middleware -> T08 tool loop
  T03A + T03B -> T07B manifest/inventory
  T07A + T07B -> T07C profile/transaction
  T03 + T08 -> T09 Sandbox collaboration -> T10 recovery/reconcile
  T03 + T04 + T07A -> T11 request manifest/context -> T12/T13 compact
  T03A + T11 -> T15 skills -> T16 specialist subagents
  T03A + T16 -> T20 child runs/team
  T17 CLI；T18 MCP；T19 observability/evals -> T19A trajectory query/replay 横向接入
```

实施原则：

1. 先契约、fake port 和确定性测试，再接真实模型、SandboxRS 或 MCP。
2. 每个副作用前写 `StepIntent`，恢复先 `reconcile`，绝不由 AgentRS 盲重试。
3. 每个任务只新增一个明确责任；不把 UI、SQLite 实现、SandboxRS 或云服务重新塞入 AgentRS。
4. 每项任务的产物必须具备单元测试；真实 Provider、浏览器、MCP 只作本地 smoke。
5. live resource cleanup 与 durable operation recovery 分开实现和验收，任何一方不得代替另一方。
6. 每个 registration 有 scope/owner，每个 operation 固定 dependency generation，每个模型请求有可重建 manifest。
7. Trajectory 由 durable facts 纯投影产生；Component Inventory 读取 runtime 权威状态，两者都不得成为第二份事实源。
8. 扩展默认静态链接或进程外隔离；profile/overlay 只能选择已安装、经 schema 校验且不突破 AuthorityEnvelope 的 Component。

## 2. 参考源码总表

以下是本方案的全部直接参考来源，后续任务以编号引用。WorkBuddy/QoderWork 来源是逆向推测文档，应只借鉴模式，不能把其中未验证的实现细节当作事实复制。

| 编号 | 项目 | 参考源码/文档模块 | 借鉴设计 |
|---|---|---|---|
| R01 | aionrs | `crates/aion-types/src/message.rs`、`llm.rs`、`tool.rs`、`spawner.rs` | provider 无关 Message/ContentBlock/LlmRequest/LlmEvent、ToolDef、Spawner 根类型 |
| R02 | aionrs | `crates/aion-agent/src/bootstrap.rs`、`engine.rs`、`turn.rs`、`context.rs`、`context_usage.rs` | Bootstrap 组装、回合循环、turn guard、请求构建、token 核算 |
| R03 | aionrs | `crates/aion-agent/src/orchestration.rs`、`tool_policy.rs`、`confirm.rs`、`output/sink.rs` | 工具编排、策略过滤、审批等待、OutputSink 输出边界 |
| R04 | aionrs | `crates/aion-agent/src/compact/{micro,auto,emergency,state,prompt}.rs` | 微压缩、LLM 自动摘要、硬上限保护的分层压缩 |
| R05 | aionrs | `crates/aion-agent/src/session.rs`、`spawn_tool.rs`、`spawner.rs`、`skill_tool.rs` | 会话快照、受限 fork、子 Agent、技能调用入口 |
| R06 | aionrs | `crates/aion-providers/src/{provider,composed,stream_runner}.rs`、`crates/aion-config/src/compat.rs` | `LlmProvider` 流式 trait、组合式 transport、无可见输出重试、ProviderCompat 数据驱动 |
| R07 | aionrs | `crates/aion-tools/src/{tool,registry,tool_search,file_cache,read,edit,write}.rs` | Tool trait/registry、deferred ToolSearch、读改缓存和陈旧检测、工具并发标记 |
| R08 | aionrs | `crates/aion-compact/src/{sanitize,fold,json,toon}.rs` | 工具输出清洗、重复折叠、JSON/表格 token 优化 |
| R09 | aionrs | `crates/aion-skills/src/{discovery,loader,frontmatter,permissions,context_modifier,prompt,substitution}.rs` | SKILL.md 发现、frontmatter、权限、上下文收窄、按需加载/参数替换 |
| R10 | aionrs | `crates/aion-memory/src/{index,store,prompt,paths}.rs` | 项目隔离记忆、索引常驻正文按需加载的接口思想 |
| R11 | aionrs | `crates/aion-mcp/src/{manager,tool_proxy,transport}.rs` | MCP tool proxy 与本地工具同注册表、延迟暴露、传输抽象 |
| R12 | aionrs | `crates/aion-protocol/src/{commands,events,approval,reader,writer}.rs`、`aion-cli/src/*` | host JSONL 命令/事件、oneshot 审批桥接、薄 CLI 驱动 |
| R13 | pi | `packages/agent/src/agent-loop.ts`、`types.ts` | EventStream 式回合循环、工具前后 hook、错误作为流事件而非打断会话 |
| R14 | pi | `packages/agent/src/harness/agent-harness.ts`、`harness/types.ts` | Harness/port 化资源、lane 操作边界、watch 快照、结果类型 |
| R15 | pi | `packages/agent/docs/harness.md`、`harness/session/types.ts`、`harness/session/*` | append-only entry、operation intent/effect/settlement、崩溃恢复语义；只借鉴持久步骤思想，不引入 lane/tree 到 P0 |
| R16 | pi | `packages/agent/src/harness/compaction/{compaction,utils}.ts` | 跨压缩追踪读/改文件，避免摘要后丢失文件工作集 |
| R17 | pi | `packages/agent/src/harness/tools/{read,edit,write}.ts` | 可取消工具、结构化 tool details、读/写工具测试方式 |
| R18 | Claude Code | `src/query.ts`、`src/services/query/QueryEngine.ts` | 全链路流式 query、边界事件、取消和终态统一处理 |
| R19 | Claude Code | `src/services/compact/{microCompact,autoCompact,reactiveCompact,contextCollapse}.ts`、`src/commands/compact/compact.ts` | 多级压缩、超长上下文恢复、压缩边界和摘要续接 |
| R20 | Claude Code | `src/memdir/{memdir,memoryScan,findRelevantMemories,memoryTypes}.ts` | 记忆扫描、相关性预取、来源/范围意识；实现仍由 Core Awareness 提供 |
| R21 | Claude Code | `src/entrypoints/sdk/coreSchemas.ts`、`src/commands/plan/plan.tsx` | PermissionMode、Plan 状态作为明确契约，不把安全只藏在提示词里 |
| R22 | Claude Code | `src/tools/AgentTool/*`、`src/tools/ToolSearchTool/*`、`src/tasks.ts` | Task/子 Agent、延迟工具发现、显式任务状态；仅借鉴 AgentRS 的 ChildRun contract |
| R23 | Claude Code | `src/skills/*`、`src/hooks/*`、`src/mcp/*` | 技能、hooks、MCP 扩展面与延迟加载思想 |
| R24 | AionCore | `crates/aionui-ai-agent/src/manager/aionrs/{agent,content,history_sanitize}.rs`、`capability/backend_output_sink.rs` | aionrs 嵌入适配、OutputSink 到统一事件翻译、历史合法性清洗 |
| R25 | AionCore | `crates/aionui-conversation/src/{turn_orchestrator,stream_relay,background_stream}.rs` | 外层单飞回合、流事件持久/转发、Finish 后后台事件处理 |
| R26 | AionCore | `crates/aionui-ai-agent/src/worker_task_manager/*`、`session/*` | 每会话惰性单飞构建、能力变化重建、后端无关 session FSM |
| R27 | AionUi | `docs/contributing/file-structure.md`、`docs/contributing/development.md` | Electron 主/预加载/渲染隔离、桌面拉起本地 Core；仅定义 AgentRS host 约束 |
| R28 | WorkBuddy 逆向分析 | `WorkBuddy实现逆向推测：六层架构、安全控制、SubAgent通信、上下文记忆管理的一些细节.md` | 专用 lite Agent、asTool 与 Team 双模式、TaskList 黑板、上下文隔离、工具按需加载 |
| R29 | QoderWork 逆向分析 | `QoderWork.md` | 记忆 FTS/JIT 检索、结构化会话备份、参数化 skill 模板、Shell snapshot；索引/文件实现归 Core/SandboxRS |
| R30 | Cordis runtime | `opensource/deepseek-harness/deepseek-harness-master/vendor/cordis/src/` | owner-bound disposer、依赖激活/撤回、异步与重入 cleanup；只借鉴语义，不移植 Proxy Context |
| R31 | DeepSeek Harness packages | `opensource/deepseek-harness/deepseek-harness-master/packages/{core,llm,client,schedule}/` | per-agent scope、LLM capability seam、session/context 组合、组件 registration 生命周期 |
| R32 | AgentRS 影响评估 | `DeepSeek-Harness与Cordis对AgentRS设计的影响评估报告.md` | live/durable 双平面、Composition Kernel、generation、RequestManifest、typed middleware、资源冲突模型 |
| R33 | Harness Trajectory/Projection | `opensource/deepseek-harness/deepseek-harness-master/packages/client/ui-trajectory/`、`opensource/deepseek-harness/deepseek-harness-master/packages/session/{session-projection,session-telemetry}/` | 权威日志派生轨迹、纯投影单元、版本失效、分页/虚拟化、ledger/ops 分流和脱敏导出 |
| R34 | Harness Component 控制面 | `opensource/deepseek-harness/deepseek-harness-master/packages/host/plugin-inventory/`、`opensource/deepseek-harness/deepseek-harness-master/packages/boot/app-boot/`、`opensource/deepseek-harness/deepseek-harness-master/packages/preset/agent-presets/` | 只读库存、分层 profile/overlay、candidate 失败回滚、per-agent composition generation |

## 3. 任务清单

### T01. Contracts 与基础类型

- 依赖：无。
- 参考：R01、R12、R14、R18、R24、R32-R34。
- 借鉴：aionrs 的 provider 无关内容块；pi 的公开 harness 类型；Claude Code/AionCore 的稳定流事件边界。
- 实施：创建 contracts/types，定义 ID 包装、消息/模型/工具/Run 类型；增加 causal RunEvent envelope、ComponentManifest/Inventory DTO、ProjectionKey/stateVersion、AuthorityEnvelope/CapabilityViewDigest、ModelRequestManifest/content ref 和 EffectProfile/ResourceAccess。
- 不照搬：不把 AionCore 的 API DTO 或 aionrs 的 Session JSON 作为公共格式；不把 SandboxGrant 的实现放入 types。
- 交付：serde schema、事件版本策略、JSON fixtures、Rust API 文档。
- 验收：未知事件/字段可降级；seq 单调且因果 ID 可关联；provider metadata 可 round-trip；AuthorityEnvelope 与 CapabilityView 分离；manifest/config/projection schema 可稳定 hash 和版本失效。

### T02. Port 接口与 Fake Testkit

- 依赖：T01。
- 参考：R13、R14、R15、R24、R30-R32。
- 借鉴：pi 的 provider/storage/hook 注入，AionCore 的 adapter 层，aionrs OutputSink。
- 实施：定义各 port；registration/resolution 携带 provider identity/generation/availability；在 testkit 实现可编排 fake、deterministic clock、capability 失效/替换、cleanup failure 与 interleaving scheduler。
- 不照搬：不在 AgentRS 实现 SQLite repository 或审批 UI；不使用全局 singleton。
- 交付：每个 port 的错误语义、fake builder、deterministic clock、录制 provider 流。
- 验收：runtime 单测不需要网络、文件系统、真实时钟或 OS 进程；fake 可精确控制 provider 撤回、旧 generation drain 和 cleanup 失败时点。

### T03. RunEvent、Step 与 Checkpoint 协议

- 依赖：T01、T02。
- 参考：R05、R15、R18、R25、R31、R32。
- 借鉴：pi operation intent/effect/settlement、Claude Code transcript 边界、AionCore StreamRelay 的终态收敛。
- 实施：定义 durable fact/content ref/live stream 分类，增加 `ModelRequestPrepared`、partial-output commit policy、capability/catalog generation/digest；checkpoint 只保存 durable projection 所需游标和引用，不序列化 live task/listener/disposer。
- 不照搬：P0 不实现 pi Entry 分支树/lane；持久化格式由 Core 决定，AgentRS 只定义语义。
- 交付：事件状态迁移表、checkpoint codec、迁移版本字段。
- 验收：同一 Step 不能重复 `finish`；终态后不允许新语义事件；UI delta 丢失不影响 committed transcript；checkpoint 可恢复且不会复活旧 live resource。

### T03A. Runtime Composition 与 Owned Lifecycle

- 依赖：T01、T02。
- 参考：R30-R32。
- 借鉴：Cordis effect/fiber 的 owner 与反序 cleanup 语义，Harness per-agent scope 和可撤销 registration；翻译为显式 Rust lifecycle。
- 实施：在 `agentrs-runtime::composition` 定义 scope 层级、ComponentSpec、LifecycleState、ResourceOwner/AsyncCleanup、child ownership、setup transaction 与 quiescent shutdown。
- 不照搬：不实现 JavaScript Proxy、字符串 Service Locator、YAML 任意插件、Rust 动态库 ABI 或不受信进程内代码。
- 交付：composition 内部模块、owner API、状态迁移表、shutdown report、故障注入 fixtures。
- 验收：setup 第 N 步失败反序清理前 N-1 步；先 cancel/drain child 再 cleanup parent；cleanup failure 不阻断其余资源；Stopping 后拒绝注册；并发 dispose 幂等且无 stale registration。

### T03B. Causal Event Envelope 与 Trajectory Projection

- 依赖：T01、T02、T03。
- 参考：R15、R25、R31-R33。
- 借鉴：Harness model-visible-is-logged、Trajectory 的 Session 投影和 Projection Registry 的纯同步 fold；事件事实与 UI 读模型分离。
- 实施：为 RunEvent 增加 trace/parent、turn/step/operation、scope/component/generation、manifest 等关联字段；实现版本化 Projection Registry 和 Run/Turn/Step/Request/Tool/Child/Terminal 基础投影。
- 不照搬：不在 AgentRS 实现 React UI；不把 lifecycle tick、全部 token delta 或 telemetry 当 durable facts；projection 不写回事实流。
- 交付：event envelope schema、ProjectionDefinition/Registry、trajectory snapshot/as_of_seq、分页游标、golden replay fixtures。
- 验收：相同事件前缀始终产生相同 snapshot；stateVersion 变化使旧缓存失效；向前补页不改变已加载记录键/顺序；UI delta 丢失不影响 committed trajectory；未知可忽略事件不破坏投影。

### T04. 最小 RuntimeHost 与 AgentEngine Loop

- 依赖：T01-T03、T03A、T03B。
- 参考：R02、R13、R18、R24、R31、R32。
- 借鉴：aionrs `engine.rs` 的 Final/ToolRound 骨架；pi EventStream；Claude `query()`；AionCore 的统一流输出。
- 实施：实现 `start/resume/cancel`、Run/Operation owner、单一可变 ConversationState 和稳定 FSM；Engine 只消费 committed dependency snapshot，provider stream 绑定 OperationOwner。
- 不照搬：不在 loop 内直接执行工具，不将前端 WebSocket 逻辑混入 runtime。
- 交付：`agentrs-runtime`、状态图、fake provider fixtures。
- 验收：纯文本、工具回合、取消、最大回合和 provider 错误产生确定事件序列；取消后 stream/task 收敛；loop 不持有可绕过 composition 的全局 registry。

### T05. Provider 抽象与 ProviderCompat

- 依赖：T01、T02、T03A、T04。
- 参考：R01、R06、R18。
- 借鉴：`LlmProvider::stream`、ComposedProvider/投影器、ProviderCompat、无可见输出重试。
- 实施：实现带 ProviderId/Generation 的 `ProviderPort::stream` 与优先 adapter；stream/continuation lease 归 OperationOwner；定义 replacement、withdrawal 和 drain policy；compat 继续数据驱动。
- 不照搬：不复制 aionrs 的全局 TOML/profile 读取；配置由 Core 转换成不可变 ModelPolicy。
- 交付：provider fixture、SSE decoder、compat matrix、稳定错误分类。
- 验收：OpenAI-compatible 新端点只加 compat/profile；已输出文本后不自动重试；一次 request 不跨 generation；旧 adapter 在 stream settlement 前不会释放；错误正文不写日志。

### T06. ModelPolicy、模型路由与预算

- 依赖：T01、T04、T05。
- 参考：R06、R21、R28。
- 借鉴：aionrs provider profile，Claude Code 的 side model，WorkBuddy lite/default/craft 分工。
- 实施：定义 `ModelPolicy`、`ModelTier`、token/cost budget、fallback 条件、数据驻留和附件能力；实现只由 Core 授权的路由选择。
- 不照搬：不允许模型自行选择任意 provider 或提升预算；不在 AgentRS 结算账单。
- 交付：router、预算耗尽事件、模型选择审计字段。
- 验收：selector/guard 固定 lite 且零工具；主 Agent 超预算时产生 `RunNeedsUserAction` 或 Core 可处理的 fallback 建议。

### T07. ToolDef、Scoped Registry 与 Deferred ToolSearch

- 依赖：T01、T02、T03A、T04。
- 参考：R07、R11、R22、R28、R30-R32。
- 借鉴：aionrs Tool trait/registry/deferred 工具和 MCP proxy，Claude ToolSearch，WorkBuddy 两步加载。
- 实施：定义含 EffectProfile/ResourceAccess 的不可执行 ToolDef、schema validator、按 ScopeId/owner/generation 查询的 registry、capability filter、deferred stub 与 immutable catalog snapshot。
- 不照搬：不把 Read/Write/Exec 实现放入 AgentRS；这些由 SandboxRS 或 Core adapter 注册。
- 交付：registry/filter/search、工具 schema fixture、prompt 描述生成器。
- 验收：未授权/已撤回工具不进入新 snapshot；dispose owner 后 contribution 消失；deferred schema 按需加载；畸形参数在执行前失败。

### T07A. Capability Generation 与 Typed Middleware

- 依赖：T03A、T05、T07。
- 参考：R30-R32。
- 借鉴：Cordis committed dependency view 与 waterfall 可环绕语义；改为有固定阶段、稳定顺序和 owner 的强类型 pipeline。
- 实施：定义 OperationView 和 provider/tool/prompt/middleware generation；实现 Tool/LLM typed middleware registry、owner-bound handle、immutable chain snapshot 和 candidate replacement 基础接口。
- 不照搬：middleware 不承担 schema/grant/intent/Sandbox/StepResult 等硬安全裁决；P0 不实现通用 loader 或热装代码。
- 交付：snapshot API、phase/order 规则、registration disposer、replacement/interleaving tests。
- 验收：一次 operation 不混用 generation；撤销组件后新 operation 不可见，旧 operation 完成或明确取消；同 order 冲突可诊断；安全固定阶段无法被短路。

### T07B. Component Manifest、Inventory 与运行时不变量

- 依赖：T03A、T03B；T07/T07A 完成后接入 Tool/Middleware registration 诊断。
- 参考：R30-R34。
- 借鉴：Cordis fiber 状态、Harness Plugin Inventory 的权威只读投影、agent preset 的 scope/generation 组合；库存不复制 Loader 状态。
- 实施：定义 ComponentManifest（id/version/api、requires/provides、config schema、execution/trust、scope/resource、migration、redaction）；实现从 Composition Kernel 即时读取的 ComponentInventory 和依赖/所有权运行时不变量。
- 不照搬：Inventory P0 不提供 install/enable/disable 写路径；不把 service/method 是否存在当健康证明；不把 scope 当安全权限边界。
- 交付：manifest schema、inventory snapshot/query、dependency graph、health/invariant report、CLI `components` contract。
- 验收：快照能定位 source/scope/owner/state/generation/dependency/registration/in-flight/last failure；registry 与 inventory 不发生双写漂移；缺失依赖、循环、越界 scope 和 orphan registration fail loud。

### T07C. Profile/Overlay 与 Candidate Generation Transaction（P1）

- 依赖：T07A、T07B；至少两个 provider、MCP 或 per-run component 需求出现后启动。
- 参考：R31、R32、R34。
- 借鉴：Harness profile/bundle/overlay 与失败保留最后可用树；改为 Rust 声明式 catalog 和显式 generation transaction。
- 实施：定义已安装 Component catalog、profile 和按 id overlay；执行 parse -> schema/API/authority/dependency validation -> private start -> health checks -> commit -> old drain/cleanup；接入 MCP/WASI/JSON-RPC External Component。
- 不照搬：不支持 `!!js`、可执行 YAML、任意 Rust dylib、在线下载后直接进程内加载或模型自修改代码。
- 交付：profile/overlay schema、dry-run/diff、transaction coordinator、rollback report、external component adapter contract。
- 验收：candidate 失败保留旧 generation；突发配置变化串行收敛到最后合法版本；旧 operation settlement 前旧代不释放；profile 不能扩大 AuthorityEnvelope；Core/Sandbox 拥有外部进程、凭据和资源限制。

### T08. ToolLoop：并发规划、Policy 与结果回灌

- 依赖：T03、T04、T07、T07A；T07B 提供诊断但不阻塞 P0 ToolLoop。
- 参考：R03、R07、R13、R17、R21、R31、R32。
- 借鉴：aionrs orchestration；pi hooks；Claude fail-closed；Harness/Cordis typed seam 和 resource independence。
- 实施：固定 parse/resolve/guard/intent/policy/approval/middleware/execute/normalize/commit pipeline；按 ResourceAccess、lease、Policy 与预算构建保守冲突图，settle 后按 call order 回灌。
- 不照搬：AgentRS 不实现 `allow always` 存储，也不把 hook 模型判断当作授权。
- 交付：ToolLoop、失败/取消回灌格式、重复调用指纹 guard。
- 验收：冲突资源串行，无冲突调用才并行；并发失败不丢其他 StepResult；结果提交顺序确定；Policy Deny 结构化回灌；每个外部副作用前都有 StepIntent。

### T09. SandboxRS 协作与 ChangeSet 事件

- 依赖：T02、T03、T07A、T08；与 SandboxRS 对齐开发。
- 参考：R03、R15、R24，以及 uworker `sandboxrs` 方案。
- 借鉴：aionrs 审批回调和 pi effect 记录；调整为 uworker 的 SandboxGrant/ChangeSet 模型。
- 实施：消费 Core 下发的 SandboxGrant，引入 `ExecutionRequest/Result`、`ChangeSetAvailable`、ArtifactRef、reconcile；将 sandbox 结果摘要化回灌模型。
- 不照搬：不提交/丢弃 ChangeSet，不创建 OS 进程，不处理回收区。
- 交付：SandboxExecutor adapter contract test、ChangeSet prompt formatter。
- 验收：AgentRS 只能展示/推理 ChangeSet；无 grant 或 input hash 不符时不调用 executor；重启后先 reconcile。

### T10. 恢复、重试与幂等防护

- 依赖：T03、T03A、T04、T08、T09。
- 参考：R06、R15、R18、R25、R30、R32。
- 借鉴：aionrs FailedEmpty/FailedPartial；pi effect sandwich；Claude transcript 边界；AionCore 自动恢复但不重复工具副作用。
- 实施：分别实现 durable RecoveryPlanner 和 live shutdown settlement：模型无可见输出可重试；工具 `not_started` 可重试；`running/unknown` reconcile；恢复时重建 composition，不恢复旧 live handle。
- 不照搬：不宣称 exactly-once；不在 AgentRS 猜测外部效果。
- 交付：RecoveryPlanner、故障注入测试、状态说明事件。
- 验收：模拟崩溃于五个 durable 边界不产生第二次未知工具调用；模拟取消/setup failure/cleanup failure 不残留 live resource；测试证明 cleanup 与 reconcile 不互相替代。

### T11. ContextManager 与 token 账本

- 依赖：T01、T03、T04、T05、T07、T07A。
- 参考：R02、R04、R16、R19、R28、R31、R32。
- 借鉴：aionrs context usage/三层压缩、pi 文件操作追踪、Claude context collapse、WorkBuddy 精准过滤。
- 实施：建立显式预算与 contributor provenance；每次请求生成 `ModelRequestManifest`，记录 source event range、content refs、provider/prompt/tool/capability digest、memory/skill/summary refs 和 token accounting。
- 不照搬：不把 Core Awareness 的 FTS 实现复制到 AgentRS；只处理 fragment、引用和预算。
- 交付：ContextPlan、token estimator port、priority ladder、文件工作集结构。
- 验收：超预算按优先级裁剪；摘要后保留关键事实；任意请求可由 RunSpec + durable events + refs 重建；无 durable 来源的内容不能进入 assembler；generation 变化会改变 manifest digest。

### T12. 工具输出规范化与 Microcompact

- 依赖：T08、T11。
- 参考：R08、R04、R16、R19。
- 借鉴：aion-compact 清洗/折叠/TOON、aionrs microcompact、pi 文件操作跨压缩、Claude snip/micro compact。
- 实施：ANSI 清洗、重复折叠、JSON/表格压缩、output size ceiling、ArtifactRef 替换大输出；清理早期已消费全文但保留执行摘要、路径引用、ChangeSet、错误。
- 不照搬：不清空用户原始输入、审批或未提交变更信息。
- 交付：normalizer、microcompact、golden fixtures。
- 验收：同一工具大输出不会无限增长上下文；压缩后模型仍可回答改了哪些文件、剩余什么风险。

### T13. Compact 与 ContextSummary

- 依赖：T11、T12、T06。
- 参考：R04、R16、R19、R28。
- 借鉴：aionrs micro/auto/emergency，pi readFiles/modifiedFiles，Claude auto/reactive/contextCollapse，WorkBuddy compact/contextSummary 分工。
- 实施：定义触发阈值、Compact（保留近期）和 ContextSummary（恢复/跨 Run）两个 prompt/schema；保存 source event range、summary version、token before/after；硬上限时停止请求并返回可恢复状态。
- 不照搬：不使用不可追溯的“摘要替换全部历史”；不让压缩 Agent 有工具权限。
- 交付：summary schema、trigger policy、snapshot/eval 数据集。
- 验收：同一区间不会二次摘要；Provider 返回 context-too-long 时可触发一次受控恢复，而不是无限重试。

### T14. MemoryRetriever 与 memorySelector 协作

- 依赖：T02、T06、T11。
- 参考：R10、R20、R28、R29。
- 借鉴：aionrs MEMORY.md 索引，Claude relevant-memory prefetch，WorkBuddy lite selector，QoderWork FTS/JIT/importance。
- 实施：只定义候选/加载 port 和带 provenance/visibility/generation 的 fragment ref；实现 lite selector 输入及默认 5 条 JSON 选择；失败降级至确定性排名，选中 ref 进入 request manifest。
- 不照搬：AgentRS 不扫描磁盘、不建 FTS、不自动修改 MEMORY.md；这些由 AgentCore Awareness 完成。
- 交付：selector prompt/schema、candidate fixtures、explainability event。
- 验收：selector 无工具且无法选择无权限 fragment；主上下文只注入被选中的可追溯片段。

### T15. Skills 与 ContextModifier

- 依赖：T01、T03A、T07、T11。
- 参考：R09、R23、R28、R29、R30、R32。
- 借鉴：aionrs SKILL.md discovery/frontmatter/context modifier，Claude skill search，WorkBuddy/QoderWork 参数化模板。
- 实施：消费 Core 解析的 SkillManifest；ContextModifier 作为 derived restriction layer，集合取交集、预算取最小、风险/可见性取更严格值；prompt contribution 带 scope/owner/version/content ref。
- 不照搬：AgentRS 不自行安装第三方技能、不执行 skill shell snippet、不升级 Sandbox 权限。
- 交付：manifest schema、prompt builder、参数替换、skill fixtures。
- 验收：未启用技能正文不进入上下文；skill 无法突破 AuthorityEnvelope 或扩大 CapabilityView；普通 config overwrite 不能绕过单调收窄；模板参数通过 schema 验证。

### T16. 函数式专用子 Agent

- 依赖：T03A、T04、T06、T11、T14、T15。
- 参考：R05、R22、R28、R30-R32。
- 借鉴：aionrs Spawn/Fork 的受限策略，Claude Task/AgentTool，WorkBuddy selector/Explore/Plan/compact/guard 的角色拆分。
- 实施：实现 AgentProfile、asTool runtime 与 derived ChildRunScope；子 operation/registration 归 child owner；首批角色输入/输出 schema 化。
- 不照搬：不回灌子 Agent chain-of-thought；P1 不允许子 Agent 持久通信或自行 spawn 工具。
- 交付：profile manifest、SubagentSummary、prompt fixtures、lite/default router policy。
- 验收：selector/compact/guard 固定零工具；父 Run 只收到结构化结论；子 Agent 不能扩权；父取消后 child 先 terminal，且 child tools/listeners/prompts 无残留。

### T17. AgentRS CLI

- 依赖：T01、T02、T03A、T03B、T04、T07B；T05 后增强真实 Provider。
- 参考：R12、R27。
- 借鉴：aionrs 薄 CLI/JSONL host 协议、AionUi 启动本地 backend 的宿主契约意识。
- 实施：实现 `run`、`serve --jsonl`、`resume`、`validate`、`replay`、`trajectory`、`components`、`doctor`；诊断读取 projection/inventory，不承担 UI、插件安装或动态代码加载。
- 不照搬：不实现 TUI，不读用户全局配置，不提供默认 `--unsafe` 或直连 Shell。
- 交付：clap command spec、JSONL schema、human renderer、protocol fixtures。
- 验收：JSONL 事件与嵌入 API 同构；默认只有 fake/read-only adapter；真实执行必须指向 Core/SandboxRS 受控 adapter。

### T18. MCP Tool Proxy（P1）

- 依赖：T03A、T07、T07A、T07B、T08、T15、T17；T07C 启用后接入 External Component profile。
- 参考：R11、R23。
- 借鉴：aionrs MCP manager/tool proxy/三 transport、Claude MCP 延迟加载。
- 实施：MCP 由 Core 管理真实连接和 OAuth；AgentRS 将 McpToolDef/executor port 映射为 scoped、owned、generation-aware deferred ToolDef，proxy 撤回不越权关闭 Core 所有的连接。
- 不照搬：AgentRS 不 spawn MCP 子进程，不保存 OAuth token，不在 CLI 自动连接任意 server。
- 交付：MCP proxy contract、命名冲突规则、transport-independent fixture。
- 验收：MCP 工具与本地工具走相同 schema/Policy/Sandbox 流程；server 失败作为工具错误回灌。

### T19. 可观测性、回放与评测

- 依赖：T01-T18 及 T03A/T03B/T07A/T07B，按功能增量接入。
- 参考：R02、R14、R18、R25、R28、R30-R34。
- 借鉴：aionrs cache diagnostics/context usage，pi telemetry，Claude stream 事件，AionCore relay，WorkBuddy 成本分级。
- 实施：区分 Agent Trajectory、Composition Trajectory 与 operational telemetry；关联 trace/run/turn/step/operation/component/scope/generation；输出 OTel 指标并构建固定 eval 集。Trajectory 领域 projection/query 由 T03B/T19A 负责。
- 不照搬：不记录 prompt 正文、文件内容、绝对路径、命令输出、密钥或 provider 原始 body。
- 交付：OTel-compatible port、脱敏字段规范、replay CLI、eval report。
- 验收：失败 Run 可重放状态机和模型输入来源；live tick 缺失不影响恢复；指标可定位 provider/tool/lifecycle/compaction 问题而不泄露用户内容。

### T19A. Trajectory Query、Replay Bundle 与脱敏导出

- 依赖：T03B、T07B、T11、T16、T19。
- 参考：R18、R25、R31-R34。
- 借鉴：Harness Trajectory 的尾部窗口、向前分页、时间线、局部检查器与虚拟行稳定性；Session Telemetry 的 ledger/ops 分流与外发副本脱敏。
- 实施：补全 request timing/usage、Policy/Approval/Sandbox、compaction、ChildRun 和 Component generation projection；实现 filter/search、稳定 cursor、replay bundle、redaction/export policy 与 AgentUI contract。
- 不照搬：AgentRS 不实现 React 页面、不上传 telemetry、不把 content ref 自动解密进导出；投影不参与模型输入或安全裁决。
- 交付：TrajectoryQuery API、snapshot/page schema、search index contract、replay bundle version、redaction fixtures、AgentUI view DTO。
- 验收：百万级事件通过分页/窗口查询而非全量加载；相同 as_of_seq 返回一致切面；按 event/tool/component/error/generation 可过滤；导出默认无敏感正文/绝对路径/密钥；bundle 可离线重建相同 projection。

### T20. ChildRun 与 Team 协作适配（P2）

- 依赖：T03、T03A、T03B、T06、T07A、T16、T19、T19A；Core TaskList 先就绪。
- 参考：R05、R15、R22、R28、R26、R30-R32。
- 借鉴：aionrs 子工具收窄策略，pi lane/operation，Claude AgentTool/coordinator，WorkBuddy TaskList 黑板，AionCore team/worker 管理。
- 实施：定义 ChildRunSpec、ScopeDerivation、parent-owned lifecycle、父子 cancel/drain、预算归集、摘要 contract、最大深度；对接 Core TaskList/notification/artifact port。
- 不照搬：AgentRS 不实现 TaskList 持久化、租约、邮箱或 worker 调度；不引入无限递归 swarm。
- 交付：child-run API、profile inheritance、cancel propagation tests。
- 验收：子 Run 能力是 AuthorityEnvelope 内父 view 的交集；主上下文只收摘要；父取消先传播并等待 terminal；child 结束无 registration/task 残留；Core 失联/超时有确定事件。

## 4. 推荐排期与并行方式

| 周期 | 主线任务 | 可并行任务 | 里程碑 |
|---|---|---|---|
| 第 1-3 周 | T01、T02、T03、T03A、T03B、T07B、T04 | T17 的 `serve/replay/trajectory/components` 骨架 | M0：owner 可收敛、基础轨迹可投影、Inventory 可诊断 |
| 第 4-6 周 | T05、T06、T07、T07A、T08、T09、T10 | T11 manifest/token、T19 指标 | M1：固定 generation 的模型/工具闭环与完整工具因果链 |
| 第 7-9 周 | T11、T12、T13、T14、T15、T16、T19A | T19 lifecycle/OTel | M2：模型输入可重建，Trajectory 可查询/回放/脱敏导出 |
| 第 10 周起 | T07C、T18、T19 | T20（待 Core Team API） | M3：声明式 profile、进程外插件、事务换代与受限多 Agent |

人员建议：一名 Rust runtime 开发负责 T01-T04/T03A/T03B/T10；一名 provider/component 开发负责 T05-T09/T07A-T07C/T18；一名 agent quality/observability 开发负责 T11-T16/T19/T19A。T03A/T03B/T07B 必须先于 M0 收口，T07A 必须先于完整 ToolLoop 收口；T07C、T18、T20 由 AgentRS 与 AgentCore/SandboxRS 团队共同评审。

P0 交付静态链接 Component、owner/scope、manifest、只读 Inventory、基础 Trajectory/Projection 和 generation 字段，不以“支持动态热插拔”为里程碑。P1 在至少两个 provider、MCP 或 per-run component 出现后启用声明式 profile、committed generation、candidate transaction 和进程外插件。通用进程内插件 ABI 不在当前排期。

## 5. 任务完成总门禁

任一任务合入前必须满足：

1. 相关 trait、事件、prompt/schema 有 fixture 和兼容性测试。
2. 新 I/O 都经 port 注入；不允许在 AgentRS 引入宿主文件、进程、数据库或 Electron 依赖。
3. 有副作用的路径能证明 `StepIntent -> Policy -> Sandbox -> StepResult` 完整存在。
4. 新上下文来源有 token 预算、可见范围和裁剪策略。
5. 新子 Agent 有固定 profile、工具集、模型档位、输入/输出 schema 和取消语义。
6. 日志、错误和 telemetry 不包含用户敏感正文或密钥。
7. 新 registration/task/stream 有唯一 ScopeId/owner，具备 cancel、drain、幂等 cleanup 测试；cleanup 不得冒充外部副作用回滚。
8. 新 operation 明确 committed dependency view，测试中不得跨 provider/tool/prompt/middleware generation。
9. 任何新模型可见内容都有 durable event 或 content ref 来源，并进入 ModelRequestManifest。
10. 新 middleware 不得绕过 schema、capability/grant、StepIntent、Sandbox enforcement 或 StepResult commit。
11. 并发能力必须声明 ResourceAccess 并有冲突/预算/稳定提交顺序测试；未声明时默认串行。
12. 新 durable event 必须说明 Trajectory projection、兼容性和 retention；新 projection 是确定性纯 fold，带 stateVersion，不能回写事实流。
13. Inventory 只能投影 Composition Kernel 权威状态；manifest/config 必须 schema/API/authority 校验，缺失依赖和不变量失败必须可诊断。
14. replay/export 默认脱敏且不自动解引用敏感 content；AgentRS 不负责上传，AgentUI 不直接读取 runtime 内部对象。
15. 外部 Component 的进程、凭据、网络和资源限制归 Core/Sandbox；不得用 profile、overlay 或 middleware 扩大 AuthorityEnvelope。
16. 开源发布前通过许可证/NOTICE/来源、secret、默认执行路径和公共 API 文档门禁。

## 6. 与总方案的职责对齐

| 能力 | AgentRS | AgentCore | SandboxRS |
|---|---|---|---|
| 推理、工具调用提议、上下文压缩 | 实现 | 注入配置/持久化 | 不参与 |
| Capability composition/lifecycle | 管 scope、owner、generation 与 committed view | 提供 AuthorityEnvelope/受信 component 配置 | 不参与 |
| Trajectory/Projection | 定义事实语义、纯投影、查询/replay/export contract | 持久化、可见性授权、向 UI 提供数据 | 不参与 |
| Component catalog/profile | 校验 manifest、组合已授权 Component、只读 Inventory | 安装、签名、市场、最终启停和商业策略 | 管进程外执行限制 |
| 模型凭据、账号模型策略 | 按 ModelPolicy 调用 | 管理/签发 | 不参与 |
| 审批与策略 | 等待/消费决策 | 最终裁决/记录 | 强制执行 grant 边界 |
| 命令执行、文件变更 | 请求/消费结果 | 签发 grant、提交 ChangeSet | 隔离执行、生成 ChangeSet/undo |
| FTS/项目记忆文件 | 选择候选/注入 fragment | 索引、权限、保留 | 不参与 |
| Team TaskList/租约/邮箱 | 子 Run 摘要 contract | 持久化和编排 | 不参与 |
| UI/CLI | 轻量 CLI 协议 | 桌面后端 API | 不参与 |

结论：AgentRS 保持“可嵌入、可恢复、可控、可解释”的开源推理内核。aionrs/pi 提供 Agent 主体与 durable recovery；DeepSeek Harness/Cordis 补足 live composition、Trajectory、Projection、Inventory、代际和 capability seam。开源范围覆盖内核、契约、Component SDK、trajectory/replay 和 testkit；账号、计费、凭据、签名市场与组织策略留在 AgentCore。所有改变用户数据或执行外部代码的责任仍停留在 Core/SandboxRS，AgentRS 不装载任意进程内插件。

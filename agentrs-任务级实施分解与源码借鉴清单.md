# AgentRS 任务级实施分解与源码借鉴清单

> 配套文档：[AgentRS 技术架构设计与实施方案](agentrs-技术架构设计与实施方案.md)
>
> 版本：v1.7（对齐架构 v1.7：回填 W1–W10 实现反馈；标注 M0 已完成任务）
>
> **M0 已完成**（252 个测试，真实 Qwen3.8-27B 端到端跑通）：T01、T02、T02A–C、T03、T03A、
> T04、T04A、T04B、T05、T05A、T10、T17、T17A、T22、T22A，以及 T00 的 A 类移植首批。
> 实现暴露的六个契约缺口与五处设计修正见架构方案 §18。
>
> 目标：将 AgentRS 拆为可排期、可验收、可并行的开发任务；逐项列明参考项目的源码模块与所借鉴的设计思想。本文的“借鉴”表示吸收架构模式，不表示复制实现或继承其安全边界。
>
> **阶段坐标统一**：本文只用 **Phase A/A′/B/C/D 与 M0–M3** 表达交付时间。`P0/P1/P2` 仅表达能力成熟度，映射固定为 P0 = Phase A+B、P1 = Phase C + Phase D 前段、P2 = Phase D 之后。此前「标 P1 却排在 Phase C」一类混用已消除。

---

## 1. 范围、原则与依赖图

AgentRS 是 uworker 的推理与工作流内核。它与 SandboxRS 并列，不能直接执行宿主命令、直接读写用户工作区或提交 ChangeSet；它通过 Core 注入的 trait port 请求执行、记录状态和等待审批。

```text
T01 contracts/types
  +-> T02 ports/testkit  (含 T02A ContentStore、T02B Persistence 幂等/epoch、T02C Hook)
       +-> T03 event/checkpoint --------------------------------+
       +-> T03A composition/lifecycle (owner/scope, 无 generation)+-> T04 runtime loop
                                                                 +-> T04A steering
                                                                 +-> T04B approval 挂起/恢复
                                             +-> T05 provider (T05A 早期真实接入)
                                                  +-> T05B HistoryLegalization
                                                  +-> T05C cache prefix / 断点 / 归因
       +-> T07 scoped tool registry ------> T08 tool loop (concurrency_safe 布尔)
  T03 + T08 -> T09 Sandbox collaboration (T09A ChangeSet overlay) -> T10 recovery/reconcile
  T03 + T04 + T05C -> T11 request manifest/context -> T12/T13 compact
  T03A + T11 -> T15 skills -> T16 specialist subagents
  T03A + T16 -> T20 child runs/team
  T03B trajectory/projection（自 M0 后移至 M1）
  T07A generation/middleware、T07B manifest/inventory、T07C profile/transaction（统一后移至 Phase D，需求触发）
  T06A PermissionMode/Plan Mode；T17 CLI；T18 MCP；T19 observability/evals -> T19A query/replay
  T21 host conformance suite 横向接入
```

实施原则：

1. 先契约、fake port 和确定性测试，再接真实模型、SandboxRS 或 MCP。**唯一例外**：真实 provider 的只读冒烟路径必须在第 2 周接通（T05A），不能等到 Phase B。
2. 每个副作用前写 `StepIntent`，恢复先 `reconcile`，绝不由 AgentRS 盲重试。
3. 每个任务只新增一个明确责任；不把 UI、SQLite 实现、SandboxRS 或云服务重新塞入 AgentRS。
4. 每项任务的产物必须具备单元测试；真实 Provider、浏览器、MCP 只作本地 smoke。
5. live resource cleanup 与 durable operation recovery 分开实现和验收，任何一方不得代替另一方。
6. 每个 registration 有 scope/owner，每个 operation 固定 `capability_digest`（P1 才升级为 generation），每个模型请求有可重建 manifest。
7. Trajectory 由 durable facts 纯投影产生；Component Inventory 读取 runtime 权威状态，两者都不得成为第二份事实源。
8. 扩展默认静态链接或进程外隔离；profile/overlay 只能选择已安装、经 schema 校验且不突破 AuthorityEnvelope 的 Component。
9. **不为尚未采用的能力预付复杂度**：generation 系统、Component Inventory、ResourceAccess 冲突图均在真实触发条件出现后才启动（见各任务的「启动条件」）。
10. 所有模型可见内容的物理来源只有两处：durable event 与 `ContentStore`。新增第三处即为设计缺陷。
11. 缓存前缀不变式与安全不变式同级：任何改写 S0–S3 的机制必须说明它在哪个 compaction 边界批量执行。
12. 安全承诺分「内核不变量」与「宿主义务」，后者由 T21 的 conformance suite 验证，不写成内核保证。
13. **结构取 Harness，内容取 aionrs**：新增机制先问"这是骨架问题还是内容问题"——骨架看 R38–R42，内容看 R01–R10。冲突按架构方案 §1.5.3 裁决表处理。
14. durable log 永远 append-only。任何"要改历史"的需求都必须表达为一个 `SurfaceOp::Replace` 节点；直接改写既有事件的实现一律拒收。
15. 任何可插拔扩展点只能收紧不能放宽：新增 guard 只有 Deny/Abstain，新增 hook 只有 Proceed/Advise/Block，不得引入 Allow 变体。

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
| R26 | AionCore | `crates/aionui-ai-agent/src/{registry,active_lease,agent_runtime,task_manager,session_agent,session_context}.rs`、`crates/aionui-conversation/src/{turn_orchestrator,turn_continuation_policy,message_cursor,startup_recovery}.rs` | 每会话惰性单飞构建、能力变化重建、后端无关 session FSM、运行中输入的续接策略 |
| R27 | AionUi | `docs/contributing/file-structure.md`、`docs/contributing/development.md` | Electron 主/预加载/渲染隔离、桌面拉起本地 Core；仅定义 AgentRS host 约束 |
| R28 | WorkBuddy 逆向分析 | `WorkBuddy实现逆向推测：六层架构、安全控制、SubAgent通信、上下文记忆管理的一些细节.md` | 专用 lite Agent、asTool 与 Team 双模式、TaskList 黑板、上下文隔离、工具按需加载 |
| R29 | QoderWork 逆向分析 | `QoderWork.md` | 记忆 FTS/JIT 检索、结构化会话备份、参数化 skill 模板、Shell snapshot；索引/文件实现归 Core/SandboxRS |
| R30 | Cordis runtime | `opensource/deepseek-harness/deepseek-harness-master/vendor/cordis/src/` | owner-bound disposer、依赖激活/撤回、异步与重入 cleanup；只借鉴语义，不移植 Proxy Context |
| R31 | DeepSeek Harness packages | `opensource/deepseek-harness/deepseek-harness-master/packages/{core,llm,client,schedule}/` | per-agent scope、LLM capability seam、session/context 组合、组件 registration 生命周期 |
| R32 | AgentRS 影响评估 | `DeepSeek-Harness与Cordis对AgentRS设计的影响评估报告.md` | live/durable 双平面、Composition Kernel、generation、RequestManifest、typed middleware、资源冲突模型 |
| R33 | Harness Trajectory/Projection | `opensource/deepseek-harness/deepseek-harness-master/packages/client/ui-trajectory/`、`opensource/deepseek-harness/deepseek-harness-master/packages/session/{session-projection,session-telemetry}/` | 权威日志派生轨迹、纯投影单元、版本失效、分页/虚拟化、ledger/ops 分流和脱敏导出 |
| R34 | Harness Component 控制面 | `opensource/deepseek-harness/deepseek-harness-master/packages/host/plugin-inventory/`、`opensource/deepseek-harness/deepseek-harness-master/packages/boot/app-boot/`、`opensource/deepseek-harness/deepseek-harness-master/packages/preset/agent-presets/` | 只读库存、分层 profile/overlay、candidate 失败回滚、per-agent composition generation |
| R35 | aionrs 缓存治理 | `crates/aion-agent/src/cache_diagnostics.rs`、`crates/aion-providers/src/projector.rs`（`cache_control: ephemeral` 断点放置）、`crates/aion-providers/src/openai.rs`（`prompt_cache_hit_tokens` 解析） | 前缀缓存断点放置、cache break 归因（system/tools/TTL）、命中率指标 |
| R36 | 历史合法化 | `crates/aion-providers/src/tool_call_sanitize.rs`、`crates/aion-agent/src/engine.rs::abort_current_turn`、AionCore `crates/aionui-ai-agent/src/manager/aionrs/history_sanitize.rs` | 残缺 tool_use 补合成 tool_result、工具调用 id 清洗、跨 provider 历史合法性修复 |
| R37 | 读改一致性 | `crates/aion-tools/src/file_cache.rs`、`crates/aion-tools/src/{read,edit,write}.rs` | 读改缓存与陈旧检测；在 ChangeSet overlay 模型下升级为读己之写的正确性要求 |

| R38 | Harness Surface / 会话核心 | `packages/core/session/src/{surface,index,types}.ts`、`packages/core/agent-loop/src/`、`docs/architecture.md#session-log` | append-only log + ModelSurface 投影、`surfaceOp: append\|replace{range}`、`deriveMessages` 唯一投影规则、`sourceEventSeqs`、model-visible-is-logged |
| R39 | Harness Turn/Step 与 inbox | `docs/architecture.md#turn-flow`、`docs/agent-lifecycle.md`、`packages/core/agent-loop/src/` | turn/step 定义、inbox + claim、authoritative pre-step、0-Step Turn、压缩双触发与 surface replacement generation |
| R40 | Harness 工具管线与并发 | `docs/tool-execution-pipeline.md`、`packages/core/tools/src/index.ts:1276`、`packages/core/agent-loop/src/tool-calls.ts` | monotonic guard（deny/abstain，顺序受保护）、`executionMode` fail-closed 布尔、exclusive-as-barrier、bounded rolling pool、启动前重分类 |
| R41 | Harness scope 与不变式 | `packages/core/scope/README.md`、`packages/runtime-diagnostics/invariants/README.md` | 注册上下文同时决定可见性与所有权；**scope 不是安全边界**；运行时不变式注册表把文档规定变成可执行断言 |
| R42 | Harness capability seam | `docs/capability-seams.md`、`docs/architecture.md#capability-seams` | Definition + Provider + Consumer 三角，缺一不成 seam；一次 provider 替换改变整个产品 |

| R43 | AionCore Team 实现 | `crates/aionui-team/src/{mailbox,task_board,visibility,member_runtime,crash_detection,work_source}.rs`、`scheduler/{wake,dedup,agent_lifecycle,crash_recovery}.rs`、`mcp/{server,tools}.rs` | 27k 行的真实团队实现：slot 成员模型、FIFO 游标邮箱、`blocked_by` 任务板 DAG、`WakePayload` 唤醒注入、wake lock 去重、崩溃检测与恢复、团队工具以 MCP 暴露。**借鉴其踩坑结论与责任边界，编排本体不进内核** |
| R44 | Harness Subagent seam | `packages/subagent/{subagent,tool-subagent,tool-subagent-control,tool-subagent-report}`、`docs/subsystems/subagent.md` | 多 provider 共存的 capability seam；start 前校验静态 capability 描述符，不支持即 fail loud；one-shot 与 continuable 两种子运行；child→parent 的 report 返回通道 |

> R26 勘误（v1.3）：v1.2 引用的 `crates/aionui-ai-agent/src/worker_task_manager/*` 与 `session/*` 在 AionCore 仓库中不存在（全仓无 `*worker_task*` 匹配，`src/` 下只有 `session_agent.rs`/`session_context.rs`/`task_manager.rs`）。上表已替换为实际承担该职责的模块。其余 R01–R34 的路径经逐项核对存在。

## 3. 任务清单

### T00. 模块移植与许可证合规（贯穿 Phase A′–B）

- 阶段：A 类移植自 **Phase A′ 第 2 周**开始（与真实 provider 冒烟同步），B 类随对应功能任务进行；合规清单持续维护。
- 依赖：T01 定型 contracts 后开始搬需要改类型的部分；纯函数模块可立即开始。
- 参考：R01–R10（aionrs 全体）。
- 借鉴：不是"借鉴"，是**按 §3.3 清单直接复制源码**。Q11 已决：走重写路线，但经验证且不冲突的模块直接搬。
- 实施：
  - **A 类（约 6800 行，仅改路径与 import）**：`aion-providers` 的五个厂商适配 / framing / parser / transport / stream / retry / projector / tool_call_sanitize；`aion-config/compat.rs`（剥掉 TOML 读取）；`aion-compact` 全部纯函数；`aion-types` 内容块模型（加 `ContentRef` 变体）；`context_usage.rs`；`turn.rs`（改为发 durable 事件）；`cache_diagnostics.rs`。
  - **B 类（约 2900 行，改接缝）**：skills 解析类（发现/加载改由 Core 提供 manifest）；`compact/*`（产物改为 `SurfaceOp::Replace`）；protocol JSONL 框架（事件集换成 `RunEvent`）；`tool_search/registry`（加 ScopeId/owner，ToolDef 去执行能力）；`file_cache`（加 `change_set_id` 维度）；`plan/*`（升级为 `PermissionMode`）。
  - **合规**：每个移植文件头部按 §3.2 模板标注来源/commit/修改摘要；逐条维护 `THIRD-PARTY-NOTICES.md`；保留 `LICENSE-APACHE`；清理任何 `aion` 商标性标识。
- 不照搬：C 类约 12900 行明确拒绝——`aion-tui`、`engine/session/orchestration/bootstrap/confirm`、`aion-config` 全局加载、skills 的 shell/executor/discovery、工具执行体、`aion-process`。**C 类不得以"临时用一下"名义进入仓库**，尤其 `aion-process`——它一进来"内核无执行权"就破了。
- 交付：移植后的模块 + **其原测试**、`THIRD-PARTY-NOTICES.md`、文件头合规检查脚本（CI 门禁）。
- 验收：
  - A 类模块携带原测试且全绿；
  - 真实 provider 上用移植的 framing/parser 跑通 SSE 流（Phase A′ 即验证，不等到 Phase B）；
  - `turn.rs` 与 `compact/*` 移植后**立即**适配四平面模型，不留"以后再改"；
  - CI 能检出缺少来源标注的移植文件；
  - 仓库无 `aion` 商标性命名。
- **纪律**：移植是一次性的，**不建立与 aionrs 上游的 rebase 关系**——那会把重写路线偷偷变成演进路线。

### T01. Contracts 与基础类型

- 依赖：无。
- 参考：R01、R12、R14、R18、R24、R32-R34。
- 借鉴：aionrs 的 provider 无关内容块；pi 的公开 harness 类型；Claude Code/AionCore 的稳定流事件边界。
- 实施：创建 contracts/types，定义 ID 包装、消息/模型/工具/Run 类型；增加 causal RunEvent envelope、ProjectionKey/stateVersion、AuthorityEnvelope/CapabilityViewDigest、`PermissionMode`、`RunEpoch`、`SpecVersion`、`ContentRef/ContentScope/ContentStat`、`PolicyDecision/SandboxGrant/InputHash`、`LegalizationOp`、`cache_prefix_digest/cache_breakpoints`、ModelRequestManifest 和 EffectProfile/ResourceAccess（**仅声明字段**）。ComponentManifest/Inventory DTO 后移至 Phase D。
- 不照搬：不把 AionCore 的 API DTO 或 aionrs 的 Session JSON 作为公共格式；不把 SandboxGrant 的实现放入 types（只定义 opaque 载体与绑定字段）。
- 交付：serde schema、事件版本策略与兼容窗口、`SpecVersion` 迁移函数骨架、JSON fixtures、Rust API 文档。
- 验收：未知事件/字段可降级；seq 单调且因果 ID 可关联；`event_id` 确定性可作幂等键；live 与 durable 分属两个序空间且类型上不可混用；provider metadata 可 round-trip；AuthorityEnvelope 与 CapabilityView 分离；manifest/projection schema 可稳定 hash 和版本失效；低于 `min_supported` 的 checkpoint 被拒绝而非静默降级。

### T02. Port 接口与 Fake Testkit

- 依赖：T01。
- 参考：R13、R14、R15、R24、R30-R32。
- 借鉴：pi 的 provider/storage/hook 注入，AionCore 的 adapter 层，aionrs OutputSink。
- 实施：定义各 port；registration/resolution 携带 provider identity/generation/availability；在 testkit 实现可编排 fake、deterministic clock、capability 失效/替换、cleanup failure 与 interleaving scheduler。
- 不照搬：不在 AgentRS 实现 SQLite repository 或审批 UI；不使用全局 singleton。
- 交付：每个 port 的错误语义、fake builder、deterministic clock、录制 provider 流。
- 验收：runtime 单测不需要网络、文件系统、真实时钟或 OS 进程；fake 可精确控制 provider 撤回、能力失效和 cleanup 失败时点。

### T02A. ContentStore 端口与 ContentRef 生命周期

- 依赖：T01。
- 参考：R15（content-addressed operation payload）、R33（脱敏导出与 content hash）、R20。
- 借鉴：Harness 把「模型可见即已记录」落到内容寻址存储的做法；AgentRS 只定义端口与 liveness，不实现存储。
- 实施：定义 `ContentStore { put/get/get_range/stat/retain/release }`、`ContentScope`（run/workspace/global）、`ContentMeta`、`RetentionOwner`；实现 ref 派生规则（hash+长度+编码+scope 决定 ref）；实现三条解引用失败降级路径（`NotFound` 降级占位、`Forbidden` 直接剔除且不暴露存在性、传输错误按可重试）；降级必须写 `ContentRefUnresolved` 并进入 manifest。
- 不照搬：不在 AgentRS 实现存储、加密、GC 或去重后端；不感知密钥。
- 交付：port trait、ref 派生与稳定性测试、retain/release 生命周期规则、fake in-memory store（可注入延迟/丢失/拒绝）、悬空引用检测器。
- 验收：checkpoint 保存前其引用的全部 ref 均已 retain；注入内容丢失后上下文装配不崩溃且降级可解释；`Forbidden` 场景下模型侧无法感知该 fragment 曾存在；相同字节在同一 scope 产生相同 ref。
- **理由**：`ContentRef/ArtifactRef/SummaryRef/MemoryRef` 遍布 manifest 与压缩产物，而 v1.2 的端口清单中没有任何一个能 put/get 内容——「任意模型请求可重建」这一完成标准此前没有机制支撑。

### T02B. Persistence 幂等、Epoch 围栏与写序

- 依赖：T01、T02。
- 参考：R15、R25、R31。
- 借鉴：pi 的 operation intent/settlement 记录语义；补上其在多 writer 与重试下缺失的围栏。
- 实施：为 `begin_step/append_event/finish_step/save_checkpoint` 全部加 `RunEpoch` 参数；定义 `append_event` 的 `event_id` 幂等语义（重投递返回首次 seq，不写第二条）；定义 `CheckpointAhead` 错误（checkpoint 引用的 seq 尚未持久化时拒绝）；定义 `Fenced` 错误与内核收敛路径。
- 不照搬：不实现具体存储；不宣称 exactly-once。
- 交付：错误分类、幂等/围栏契约测试、fake persistence 的乱序/重投递/双 writer 注入器。
- 验收：同一 `event_id` 重投递 100 次只产生一条记录且 seq 不变；旧 epoch writer 被拒绝后停止全部 durable 写入并以 `RunFailed{Fenced}` 收敛；checkpoint 不会先于其引用的事件可见。
- **理由**：三段式调用之间此前无原子性、无幂等键、无围栏，「恢复以 durable log 为准」在双 writer 下不成立。

### T02C. HookEvaluator 端口

- 依赖：T01、T02。
- 参考：R13（工具前后 hook）、R23（Claude hooks）、R09（`aion-skills/src/hooks.rs`）。
- 借鉴：pi 的 tool 前后 hook 与 Claude 的 hooks 体系；收敛为「只能建议或否决，不能授权」的单一端口。
- 实施：定义 `HookPoint{PreToolUse,PostToolUse,PreCompact,TurnEnd,RunStop}`、`HookOutcome{Proceed,Advise,Block}`、超时与失败默认值（失败按 `Proceed` 并记 telemetry，避免宿主 hook 故障锁死 Run）；`Block` 走与 `Deny` 相同的结构化回灌路径。
- 不照搬：AgentRS 不执行 shell hook、不解析 hook 配置、不允许 hook 返回 grant 或放宽 Policy。
- 交付：port trait、`HookOutcomeRecorded` 事件、超时/失败策略、fake hook runner。
- 验收：`Block` 的工具不产生 `StepIntent`；`Advise` 只追加文本不改变授权；hook 超时不阻塞 turn 收敛；不存在任何 hook 能扩大能力的路径。

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
- 阶段：Phase A（M0）。**范围已收缩**：不含 generation、ComponentManifest、Inventory、candidate transaction，这些统一后移至 Phase D 并由需求触发。
- 实施：在 `agentrs-runtime::composition` 定义 scope 层级、LifecycleState、ResourceOwner/AsyncCleanup、child ownership、setup transaction 与 quiescent shutdown。依赖视图以单一 `capability_digest` 表达，不变式只有一条：一次 operation 内 digest 不变。
- 不照搬：不实现 JavaScript Proxy、字符串 Service Locator、YAML 任意插件、Rust 动态库 ABI 或不受信进程内代码；**P0 不实现 generation 元组**——P0 明确不采用动态插件，五元组 generation 在 P0 全程为常量却要渗透进每个 port 签名与事件 envelope。
- 交付：composition 内部模块、owner API、状态迁移表、shutdown report、故障注入 fixtures。
- 验收：setup 第 N 步失败反序清理前 N-1 步；先 cancel/drain child 再 cleanup parent；cleanup failure 不阻断其余资源；Stopping 后拒绝注册；并发 dispose 幂等且无 stale registration。

### T03B. Causal Event Envelope 与 Trajectory Projection

- 阶段：**自 M0 后移至 Phase B（M1）**。M0 只保留 envelope 的关联字段定义（随 T01 交付），Projection Registry 与全部投影在 M1 实现。理由见第 4 节排期说明。
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

### T04A. Steering：运行中注入用户输入

- 依赖：T01、T03、T04。
- 参考：R25（`turn_orchestrator`/`background_stream`）、R26（`message_cursor`/`turn_continuation_policy`）、R18。
- 借鉴：AionCore 为补足 aionrs「单次驱动、无注入通道」而在外层重建的续接编排——把它下沉到内核，避免 AgentCore 再造一遍。
- 实施：`RunHandle::submit(UserInput) -> Result<InputAccepted>`；steering 队列 + 两类安全边界排空（turn 边界、tool round 之间）；排空写 `UserInputSubmitted` durable 事件并计入 token 账本；队列上限与 `SteeringQueueFull`；终态后返回 `RunAlreadyTerminal`。
- 不照搬：**不打断进行中的 provider 流**——打断是 `cancel` 的职责，两者是不同动作，由宿主组合为一个 UI 按钮。
- 交付：submit API、队列语义、边界排空测试、cancel+submit 组合顺序测试。
- 验收：注入内容只在安全边界进入对话；注入后 `cache_prefix_digest` 变化且归因为 `SteeringInjected`；终态后 submit 被拒绝；队列满时不无界增长；cancel 与 submit 并发时行为确定且无竞态。

### T04B. 审批的有界阻塞、挂起与恢复

- 依赖：T02、T03、T04；与 T08 联合验收。
- 参考：R03（`confirm.rs` 审批等待）、R12（oneshot 审批桥接）、R21。
- 借鉴：aionrs 的审批回调与 protocol 桥接；补上其缺失的 deadline 与挂起语义。
- 实施：`await_approval(request, deadline)`，deadline = min(approval_timeout, 剩余 ExecutionBudget)，默认 60s；`ApprovalOutcome::Pending{resume_token}` 时写 `ApprovalTimedOut` + checkpoint，释放**全部** live resource（provider stream / child / lease），以 `RunNeedsUserAction{approval}` 收敛；`RunCheckpoint` 表达「StepIntent 已写、尚未执行」中间态；`resume` 凭 token 换取最终决策；过期 grant 强制重新 `evaluate`；同一 turn 多个待审批 proposal 合并为一次挂起。
- 不照搬：不允许无限期阻塞；不在 AgentRS 存储 `allow always`。
- 交付：审批状态机、挂起 checkpoint codec、resume 路径、超时注入测试。
- 验收：挂起后该 Run 在进程内常驻内存 ≈ 0；`resume` 能继续同一 `StepIntent` 且不重复执行；grant 过期时走重新裁决而非直接执行；N 个待审批工具只产生一次挂起往返。
- **理由**：v1.2 同时存在「阻塞 await」与「NeedsUserAction 终态」两条未收敛的路径，二者的资源含义相差一个数量级。

### T05. Provider 抽象与 ProviderCompat

- 依赖：T01、T02、T03A、T04。
- 参考：R01、R06、R18。
- 借鉴：`LlmProvider::stream`、ComposedProvider/投影器、ProviderCompat、无可见输出重试。
- 实施：实现带 ProviderId/Generation 的 `ProviderPort::stream` 与优先 adapter；stream/continuation lease 归 OperationOwner；定义 replacement、withdrawal 和 drain policy；compat 继续数据驱动。
- 不照搬：不复制 aionrs 的全局 TOML/profile 读取；配置由 Core 转换成不可变 ModelPolicy。
- 交付：provider fixture、SSE decoder、compat matrix、稳定错误分类、`ModelFallbackApplied` 归因字段。
- 验收：OpenAI-compatible 新端点只加 compat/profile；已输出文本后不自动重试；一次 request 内 `capability_digest` 不变；旧 adapter 在 stream settlement 前不会释放；错误正文不写日志；`ModelPolicy.fallback` 内的切换在进程内完成，越权切换才上报 Core。

### T05A. 真实 Provider 早期冒烟（Phase A′，第 2 周起）

- 依赖：T01、T02、T04 骨架。
- 参考：R06、R35。
- 借鉴：aionrs 的 SSE 分帧、projector 与 usage 解析。
- 实施：第 2 周即接通一个真实 provider 的只读路径（真实 SSE 流 + 无工具文本 Final）；用真实响应校准 `ProviderCompat`、reasoning 签名 round-trip 与 `cache_read_tokens`/`prompt_cache_hit_tokens` 解析。
- 不照搬：此路径不引入工具执行，不绕过后续的 Policy/Sandbox 设计。
- 交付：真实 provider 冒烟测试（可跳过的 CI 门）、录制流 fixtures。
- 验收：真实 provider 上跑通文本 Final 并观测到非零 cache 命中。
- **理由**：若前三周全部基于 fake，SSE 分帧、工具 wire format、签名 round-trip、缓存断点放置四件事都未被验证，返工风险会集中爆发在 Phase B。

### T05B. HistoryLegalization 固定阶段

- 依赖：T01、T05。
- 参考：R36、R06、R24。
- 借鉴：aionrs `tool_call_sanitize.rs` + `abort_current_turn()` 补合成 tool_result，AionCore `history_sanitize.rs`；把这三处分散逻辑收敛为一个固定阶段。
- 实施：在 request assembler 与 provider projector 之间插入固定阶段；实现 `LegalizationOp`（合成 tool_result、丢弃失效 reasoning 签名、丢弃不支持 block、合并相邻消息、重写 tool_call id）；op 列表进 `ModelRequestManifest.legalization_ops` 并发 `HistoryLegalized` 事件（只含 op，不含正文）。
- 不照搬：不写回事件流、不修改 `ConversationSnapshot`、不引入新的用户或工具内容。
- 交付：legalization pipeline、op schema、跨 provider fixture、golden 序列。
- 验收：相同历史 + 相同 `ProviderCompat` 必然产生相同 op 序列（replay 可重现）；取消导致的残缺 `tool_use` 被补齐且合成结果在 trajectory 上与真实结果可区分；跨 provider fallback 与跨 compaction 后不因失效签名报 400。
- **理由**：durable 事实流与「provider 当下能接受的输入」之间存在不可消除的阻抗；v1.2 只在 R24 提了一句，无任何任务承担。

### T05C. 缓存前缀布局、断点与归因

- 依赖：T01、T05、T05B、T11。
- 参考：R35。
- 借鉴：aionrs `cache_diagnostics.rs` 的 break 归因与 `projector.rs` 的 `cache_control: ephemeral` 断点放置。
- 实施：实现 S0–S5 分段布局与「S0–S3 只能追加」不变式；计算 `cache_prefix_digest` 与 `cache_breakpoints` 并写入 manifest；按 provider 能力放置断点；实现 `CacheBreakCause` 归因与 `CacheBreakObserved` 事件；把「改写前段」的操作统一收拢到 compaction 边界（约束 T12/T13 的节奏）。
- 不照搬：不为提高命中率而牺牲授权正确性——`PermissionMode` 与工具目录必须同步变化，宁可 miss。
- 交付：分段布局器、断点策略、digest 计算、归因指标、命中率回归测试。
- 验收（拆两条，见架构 §14 Phase B）：
  - **功能**（不依赖端点，必达）：`cache_prefix_digest` 计算正确；每次 miss 可归因到具体 `CacheBreakCause`；Surface `Replace` 后失效点计算准确；`prompt_tokens_details.cached_tokens` 与顶层 `prompt_cache_hit_tokens` 两种布局均能解析；compaction 边界之外不发生 S0–S3 重写；deferred schema 只追加不重排。
  - **收益**（需支持端点）：连续 10 轮真实对话 input token 命中率 ≥ 70%。端点不报告缓存字段时本条不可验证——排查先看服务端计数器（vLLM 为 `/metrics` 的 `prefix_cache_queries_total`）。
- **理由**：v1.2 全文 0 次提及前缀缓存，而 Microcompact、deferred ToolSearch、ContextModifier、memorySelector、模式切换五项机制会各自砸掉缓存且无人协调，实际命中率会趋近 0。

### T06. ModelPolicy、模型路由与预算

- 依赖：T01、T04、T05。
- 参考：R06、R21、R28。
- 借鉴：aionrs provider profile，Claude Code 的 side model，WorkBuddy lite/default/craft 分工。
- 实施：定义 `ModelPolicy`、`ModelTier`、token/cost budget、fallback 条件、数据驻留和附件能力；实现只由 Core 授权的路由选择。
- 不照搬：不允许模型自行选择任意 provider 或提升预算；不在 AgentRS 结算账单。
- 交付：router、预算耗尽事件、模型选择审计字段。
- 验收：selector/guard 固定 lite 且零工具；主 Agent 超预算时产生 `RunNeedsUserAction` 或 Core 可处理的 fallback 建议；`ModelPolicy.fallback` 授权范围内的切换不需要 checkpoint 往返。

### T06A. PermissionMode 与 Plan Mode

- 依赖：T01、T04、T07。
- 参考：R21（`coreSchemas.ts` 的 PermissionMode、`plan.tsx`）、R05、aionrs `crates/aion-agent/src/plan/{state,tools,prompt,file}.rs` 与 `engine.rs::tool_definitions_for_turn`。
- 借鉴：aionrs 把 plan 激活同时作用于工具集过滤与系统提示的做法；Claude Code 把权限模式作为显式契约而非提示词约定。
- 实施：定义 `PermissionMode{Plan, Default, Accepted{scopes}}` 为 Run 级状态；实现「工具目录投影 + 系统提示分段」的同步变化；模式切换只在 turn 边界并写 `PermissionModeChanged`；模型只能通过工具**提议**切换、经 Policy 裁决生效；`Plan -> Default` 需显式审批，`Default -> Plan` 无条件允许；ChildRun 继承且只能更严格。
- 不照搬：不把权限模式藏在提示词里；不允许模型自行切换。
- 交付：模式状态机、目录投影规则、切换审批路径、模式与缓存断点的联动。
- 验收：Plan 模式下写类工具不进入 catalog 且系统提示同步变化（模型不会提出注定被拒的调用）；模式切换必然产生缓存断点且归因为 `PermissionModeChanged`；子 Run 无法放宽父 Run 的模式。
- **理由**：v1.2 把 Plan 降级为「返回 schema 的函数式子 Agent」，丢失了「整个 Run 处于受限权限态」这一语义，相对 aionrs 是功能回退。

### T07. ToolDef、Scoped Registry 与 Deferred ToolSearch

- 依赖：T01、T02、T03A、T04。
- 参考：R07、R11、R22、R28、R30-R32。
- 借鉴：aionrs Tool trait/registry/deferred 工具和 MCP proxy，Claude ToolSearch，WorkBuddy 两步加载。
- 实施：定义不可执行 ToolDef（含 `concurrency_safe(input) -> bool` 判定，以及**仅作声明**的 EffectProfile/ResourceAccess 字段）、schema validator、按 ScopeId/owner 查询的 registry、capability filter、`PermissionMode` 过滤、deferred stub 与 immutable catalog snapshot；deferred schema 只能追加到目录尾部，不得重排（缓存前缀约束）。
- 不照搬：不把 Read/Write/Exec 实现放入 AgentRS；这些由 SandboxRS 或 Core adapter 注册。
- 交付：registry/filter/search、工具 schema fixture、prompt 描述生成器。
- 验收：未授权/已撤回工具不进入新 snapshot；dispose owner 后 contribution 消失；deferred schema 按需加载；畸形参数在执行前失败。

### T07A. Capability Generation 与 Typed Middleware

- 阶段：**typed middleware 部分留在 Phase B；generation 部分整体后移至 Phase D**，启动条件为「至少两个 provider、MCP 或 per-run component 的真实换代需求已出现」。在此之前依赖视图只有单一 `capability_digest`（T03A 交付）。
- 依赖：T03A、T05、T07。
- 参考：R30-R32。
- 借鉴：Cordis committed dependency view 与 waterfall 可环绕语义；改为有固定阶段、稳定顺序和 owner 的强类型 pipeline。
- 实施：定义 OperationView 和 provider/tool/prompt/middleware generation；实现 Tool/LLM typed middleware registry、owner-bound handle、immutable chain snapshot 和 candidate replacement 基础接口。
- 不照搬：middleware 不承担 schema/grant/intent/Sandbox/StepResult 等硬安全裁决；P0 不实现通用 loader 或热装代码。
- 交付：snapshot API、phase/order 规则、registration disposer、replacement/interleaving tests。
- 验收：一次 operation 不混用 generation；撤销组件后新 operation 不可见，旧 operation 完成或明确取消；同 order 冲突可诊断；安全固定阶段无法被短路。

### T07B. Component Manifest、Inventory 与运行时不变量

- 阶段：**自 M0 后移至 Phase D**，与 T07A 的 generation 部分同批启动。此前 M0 排期把本任务放在第 1-3 周，却在正文要求「T07/T07A 完成后接入」（T07A 排在第 4-6 周）——该矛盾已消除。
- 依赖：T03A、T03B、T07、T07A。
- 参考：R30-R34。
- 借鉴：Cordis fiber 状态、Harness Plugin Inventory 的权威只读投影、agent preset 的 scope/generation 组合；库存不复制 Loader 状态。
- 实施：定义 ComponentManifest（id/version/api、requires/provides、config schema、execution/trust、scope/resource、migration、redaction）；实现从 Composition Kernel 即时读取的 ComponentInventory 和依赖/所有权运行时不变量。
- 不照搬：Inventory P0 不提供 install/enable/disable 写路径；不把 service/method 是否存在当健康证明；不把 scope 当安全权限边界。
- 交付：manifest schema、inventory snapshot/query、dependency graph、health/invariant report、CLI `components` contract。
- 验收：快照能定位 source/scope/owner/state/generation/dependency/registration/in-flight/last failure；registry 与 inventory 不发生双写漂移；缺失依赖、循环、越界 scope 和 orphan registration fail loud。

### T07C. Profile/Overlay 与 Candidate Generation Transaction

- 阶段：Phase D。依赖：T07A、T07B；至少两个 provider、MCP 或 per-run component 需求出现后启动。
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
- 实施：固定 parse/resolve/mode-filter/guard/**Hook(PreToolUse)**/intent/policy/approval/middleware/execute/**Hook(PostToolUse)**/normalize/commit pipeline；**P0 并发判定用 `concurrency_safe(input)` 布尔 + 稳定 proposal order 串行**，`EffectProfile`/`ResourceAccess` 仅记录进 ToolDef 与 trajectory，不驱动调度器；grant 一次性消费本地记账；`input_hash` 覆盖 `tool_name + 规范化参数 + workspace_id + change_set_id`。
- 不照搬：AgentRS 不实现 `allow always` 存储，也不把 hook 模型判断当作授权；**P0 不实现资源冲突图与 lease**——最有并行价值的 Read/Grep/Glob 本就只读，而 Bash/Exec 的资源集合在参数层不可判定只能永远 exclusive，冲突图的净效果与一个布尔相同（参见 aionrs `orchestration.rs` 的批次划分）。升级触发条件：出现「两个写不同文件的 Edit 应当并行」且实测收益可观测。
- 交付：ToolLoop、失败/取消回灌格式、重复调用指纹 guard、grant 消费记账。
- 验收：非 `concurrency_safe` 的调用严格按 proposal order 串行；并发失败不丢其他 StepResult；结果提交顺序确定；Policy Deny 与 Hook Block 都走结构化回灌；每个外部副作用前都有 StepIntent；同一 grant 二次消费被判为内部错误而非重试。

### T09. SandboxRS 协作与 ChangeSet 事件

- 依赖：T02、T03、T07A、T08；与 SandboxRS 对齐开发。
- 参考：R03、R15、R24，以及 uworker `sandboxrs` 方案。
- 借鉴：aionrs 审批回调和 pi effect 记录；调整为 uworker 的 SandboxGrant/ChangeSet 模型。
- 实施：消费 Core 下发的 SandboxGrant，引入 `ExecutionRequest/Result`、`ChangeSetAvailable`、ArtifactRef、reconcile；将 sandbox 结果摘要化回灌模型。
- 不照搬：不提交/丢弃 ChangeSet，不创建 OS 进程，不处理回收区。
- 交付：SandboxExecutor adapter contract test、ChangeSet prompt formatter。
- 验收：AgentRS 只能展示/推理 ChangeSet；无 grant 或 input hash 不符时不调用 executor；grant 过期时重新裁决；重启后先 reconcile。

### T09A. ChangeSet Overlay 与读己之写

- 依赖：T09、T11；与 SandboxRS 联合定义。
- 参考：R37（`file_cache.rs` 读改缓存与陈旧检测）、R16（跨压缩追踪读/改文件）、R17。
- 借鉴：aionrs 的文件缓存陈旧检测与 pi 的 readFiles/modifiedFiles 跨压缩追踪；在 ChangeSet 模型下把它从缓存优化提升为正确性要求。
- 实施：`ExecutionRequest` 强制携带 `change_set_id`（缺失即错误，不默认走工作区）；约定 SandboxRS 对同一 Run 的所有执行呈现一致 overlay（工作区 + 未提交变更）；文件工作集条目升级为 `(path, change_set_id, content_ref, observed_at_seq)` 并在压缩后保留；外部提交/丢弃 ChangeSet 时在下一个安全边界把受影响条目标记 `Stale` 并显式告知模型。
- 不照搬：AgentRS 不实现 overlay 文件系统、不提交/丢弃 ChangeSet、不处理回收区。
- 交付：overlay 契约测试（作为 host obligation H7）、工作集结构、Stale 通知格式。
- 验收：Edit 后 Read/Grep/构建读到新内容；同一命令在不同 `change_set_id` 上产生不同 `input_hash`（reconcile 不混淆）；压缩后模型仍知道改过哪些文件及其版本；ChangeSet 被外部丢弃后模型不会静默沿用旧内容。
- **理由**：v1.2 规定「变更先入 ChangeSet、由用户提交」，但从未定义未提交变更的可见性——「Edit 后 Read 同一文件」是必然发生的序列，读不到新内容会让模型陷入循环。

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
- 实施：建立显式预算与 contributor provenance；每次请求生成 `ModelRequestManifest`，记录 source event range、content refs、`capability_digest`、`cache_prefix_digest`/断点、`legalization_ops`、memory/skill/summary refs 和 token accounting；正文一律经 `ContentStore` ref 化，manifest 自身 < 64 KB；图像/附件按 `ProviderCompat.image_input()` 折算并占独立配额。
- 不照搬：不把 Core Awareness 的 FTS 实现复制到 AgentRS；只处理 fragment、引用和预算；不在 manifest 内联正文。
- 交付：ContextPlan、token estimator port、priority ladder、文件工作集结构、多模态配额。
- 验收：超预算按优先级裁剪；摘要后保留关键事实；任意请求可由 RunSpec + durable events + ContentStore refs 重建；无 durable 来源或未 retain 的内容不能进入 assembler；`capability_digest` 变化会改变 manifest；装配 P95 < 30 ms @ 200 条消息。

### T12. 工具输出规范化与 Microcompact

- 依赖：T08、T11。
- 参考：R08、R04、R16、R19。
- 借鉴：aion-compact 清洗/折叠/TOON、aionrs microcompact、pi 文件操作跨压缩、Claude snip/micro compact。
- 实施：ANSI 清洗、重复折叠、JSON/表格压缩、output size ceiling、`ContentStore.put` 后以 ArtifactRef 替换大输出（此步发生在结果首次进入上下文时，属 S5 段，不破坏前缀）；Microcompact 清理早期已消费全文但保留执行摘要、路径引用、ChangeSet、错误——**因其改写 S2/S3，必须攒到 compaction 边界批量执行**，不得每轮零敲碎打。
- 不照搬：不清空用户原始输入、审批或未提交变更信息；不在 compaction 边界之外重写前缀段。
- 交付：normalizer、批量 microcompact、边界触发策略、golden fixtures。
- 验收：同一工具大输出不会无限增长上下文；压缩后模型仍可回答改了哪些文件、剩余什么风险；边界之外不产生 `CacheBreakCause::HistoryRewritten`。

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

### T16. 函数式专用子 Agent（Phase C）

- 依赖：T03A、T04、T06、T11、T14、T15。
- 参考：R05、R22、R28、R30-R32。
- 借鉴：aionrs Spawn/Fork 的受限策略，Claude Task/AgentTool，WorkBuddy selector/Explore/Plan/compact/guard 的角色拆分。
- 实施：实现 AgentProfile、asTool runtime 与 derived ChildRunScope；子 operation/registration 归 child owner；首批角色输入/输出 schema 化。
- 不照搬：不回灌子 Agent chain-of-thought；P1 不允许子 Agent 持久通信或自行 spawn 工具。
- 交付：profile manifest、SubagentSummary、prompt fixtures、lite/default router policy。
- 验收：selector/compact/guard 固定零工具；父 Run 只收到结构化结论；子 Agent 不能扩权；父取消后 child 先 terminal，且 child tools/listeners/prompts 无残留。

### T17. AgentRS CLI

- 阶段：Phase A 交付 `run`/`serve`/`doctor`/`resume`；Phase B 补 `validate`/`replay`/`trajectory`/`conformance`/`cache-report`；`components` 留到 Phase D。
- 依赖：T01、T02、T04；`trajectory` 依赖 T03B，`conformance` 依赖 T21，`components` 依赖 T07B。
- 参考：R12、R27。
- 借鉴：aionrs 薄 CLI/JSONL host 协议、AionUi 启动本地 backend 的宿主契约意识。
- 实施：按上述阶段实现命令面；诊断读取 projection，不承担 UI、插件安装或动态代码加载。
- 不照搬：不实现 TUI，不读用户全局配置，不提供默认 `--unsafe` 或直连 Shell。
- 交付：clap command spec、JSONL schema、human renderer、protocol fixtures。
- 验收：JSONL 事件与嵌入 API 同构；默认只有 fake/read-only adapter；真实执行必须显式指向受控 adapter。

### T17A. 参考 dev adapter（dogfooding 路径）

- 阶段：Phase B（M1 末必须可用）。
- 依赖：T02A、T02B、T02C、T09、T09A、T21。
- 参考：R12、R27、R07。
- 借鉴：aionrs CLI 自带可运行工具集从而能被自己开发者验证的实践；在保持"默认只读"边界的前提下，把它做成显式、独立、非默认的开发工具。
- 实施：独立 crate `agentrs-dev-adapter`，提供最小但真实的四件套——本地 JSONL Persistence（幂等索引 + epoch 文件锁）、本地 CAS ContentStore（retain 计数）、终端交互式 Policy（无 `allow always` 持久化）、受限 Sandbox（SandboxRS 就绪前用子进程 + allowlist + overlay 目录）；启动打印非生产横幅；必须通过 `agentrs conformance` 全部用例。
- 不照搬：不进入 `agentrs-cli` 默认依赖，不隐式加载（必须 `--adapter dev`）；不放宽任何边界——grant 仍由其 Policy 签发、hash 仍被校验、CLI 仍不直连宿主 Shell。
- 交付：adapter crate、非生产横幅、conformance 通过报告、dogfooding 使用文档。
- 验收：M1 末团队能用 AgentRS 完成 AgentRS 自身的日常修改；该 adapter 持续通过 H1–H7；关闭 `--adapter dev` 后 CLI 行为回到默认只读。
- **理由**：若严格止步于"真实执行必须由 Core 提供 adapter"，Phase A/B 期间内核无法被自己的开发者真实使用，可用性缺陷要等 AgentCore 就绪才暴露。这是一个可预见且可消除的反馈延迟。

### T18. MCP Tool Proxy（Phase D）

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

### T20. ChildRun 派生与父子生命周期（函数式子 Agent）

- 阶段：Phase C（随 T16 函数式子 Agent）。**范围已收窄**：本任务只管 parent-owned 的 ChildRun，协作式成员见 T24 系列。
- 依赖：T03、T03A、T16。
- 参考：R05（aionrs Spawn/Fork 收窄策略）、R22（Claude AgentTool）、R38（Harness subagent seam）。
- 借鉴：Harness 把 subagent 做成**多 provider 共存的 capability seam**（`ctx.subagents` 注册多个后端：进程内 spawn、fork、ACP、外部产品），并在 `start` 前用静态 capability 描述符校验请求——**不支持的能力 fail loud，绝不接受后静默忽略**。
- 实施：定义 `ChildRunSpec`、`ScopeDerivation`、parent-owned lifecycle、父子 cancel/drain、预算归集、`SubagentSummary` contract、最大深度；child 共享父 `RunEpoch`。
- 不照搬：不引入无限递归 swarm；不回灌 child 的过程推理。
- 交付：child-run API、profile inheritance、cancel propagation tests、capability 校验的 fail-loud 路径。
- 验收：子 Run 能力是 AuthorityEnvelope 内父 view 的交集；主上下文只收摘要；父取消先传播并等待 terminal；child 结束无 registration/task 残留；请求不支持的 capability 时报错而非静默降级。

### T24. ExternalFact：跨 Run 内容投递

- 阶段：**Phase B**。它改变 inbox/claim 与 Surface 的契约，属于结构层，不能拖到 Phase D 再回头改地基。
- 依赖：T03、T04A（steering）、T22（Surface）、T02A（ContentStore）。
- 参考：R26（AionCore `mailbox.rs` 的 FIFO 游标与 unread 语义、`scheduler/wake.rs` 的 WakePayload）、R38、R39。
- 借鉴：AionCore 用 `Mailbox` + `WakePayload{agent, tasks, unread}` 唤醒成员；本任务取其**投递语义**，但把"注入上下文"的动作收敛到 AgentRS 已有的 inbox/claim，避免出现第二套 inbox。
- 实施：定义 `ExternalFact{fact_id, origin, content, causality}`、`FactOrigin{TeamMessage, BoardSnapshot, HostNotice}`、`ExternalCausality{source_run_id, source_seq}`；扩展 `UserInput` 支持 External 变体；claim 后写 `ExternalFactReceived` durable 事件并 `SurfaceOp::Append`；`fact_id` 幂等去重；trajectory 增加跨 Run 因果串联。
- 不照搬：**不实现邮箱存储、投递重试、已读游标、路由与可见性策略**——那些归 Core。AgentRS 只负责"从 inbox 到 Surface"这一段。
- 交付：ExternalFact 类型、投递路径、幂等测试、跨 Run 因果投影、"只带内容不带能力"的负向测试。
- 验收：跨 Run 内容全部经此通路进入 Surface，不存在旁路；重复投递同一 `fact_id` 只入一次；投递不打断进行中的 Step；收到消息的成员 Run 可独立 replay；**构造一条试图通过消息扩权的用例必须失败**；Trajectory 能回答"这个成员为什么这么做"。

### T25. BoardSnapshotRef：共享可变状态固化

- 阶段：Phase C。
- 依赖：T02A、T11、T24。
- 参考：R26（AionCore `task_board.rs` 的 `blocked_by` DAG 与 `TaskUpdate`）、R28（WorkBuddy TaskList 黑板）。
- 借鉴：AionCore 把全量任务列表放进 WakePayload 一次性注入；本任务保留"快照注入"的做法，但**要求快照不可变且内容寻址**，以满足 replay 确定性。
- 实施：定义 `BoardSnapshotRef{board_id, board_version, content: ContentRef, taken_at}`；快照经 `ContentStore.put` 后进入 `ModelRequestManifest`，与记忆/技能 fragment 同等对待；由成员 checkpoint `retain`；prompt 模板必须明示"截至 board_version=N 的快照，可能已陈旧"。
- 不照搬：**不实现任务板存储、DAG 校验、环检测、租约**——归 Core。
- 验收：同一 log 前缀 replay 得到同一份板快照；成员 Run 不存在"查当前状态"的代码路径；Team 归档后 release 不影响已 retain 的快照；模型能正确表达"我看到的板可能不是最新的"。

### T26. 团队审批归属与预算池

- 阶段：Phase C。
- 依赖：T04B（审批挂起）、T06（预算）、T24。
- 参考：R26（AionCore team capability/visibility）、R28。
- 借鉴：无可借鉴——两个参考实现都未明确禁止 agent 间授权，这是本方案自有的安全要求。
- 实施：`ApprovalRequest` 增加 `originating_member` 与 `team_id` 供 UI 归组；**内核侧拒绝任何来源标记为 agent 的 `ApprovalDecision`**；`ExecutionBudget` 支持"从团队池借出额度"的表达，耗尽时产生确定终态并如实上报消耗。
- 不照搬：不实现团队池的存储与再分配（归 Core）。
- 验收：构造"成员 B 的审批被成员 A 批准"的用例必须被拒绝；N 个成员的总消耗不超过团队池；额度耗尽产生 `RunNeedsUserAction` 而非静默超支或截断。

### T27. MemberRunSpec 派生与成员终态契约

- 阶段：Phase D（需 Core 编排面就绪）。
- 依赖：T20、T24、T25、T26；Q9 已确认。
- 参考：R26（`member_runtime.rs` 的 slot 模型、`crash_detection`/`crash_recovery`、`scheduler/agent_lifecycle.rs`）。
- 借鉴：AionCore 的 slot 概念（成员可改名、可清上下文、可关停、崩溃可检测）；但**生命周期编排归 Core**，AgentRS 只提供派生规则与确定终态。
- 实施：定义 `MemberRunSpec` 派生规则——authority ⊆ 创建者 CapabilityView、`permission_mode` 只能更严、`depth+1` 超限拒绝；成员是**平级 Run，有自己的 `RunEpoch`**（不共享创建者的）；崩溃时保证发出确定终态供 Core 回收租约；"清空成员上下文"实现为覆盖全部 range 的 `SurfaceOp::Replace`，保留 append-origin transcript。
- 不照搬：**不实现 Team 实体、成员调度、崩溃巡检、死锁检测、可见性策略**——AionCore 的 `aionui-team` 为 27,166 行，绝大部分是这些编排职责，不进内核。
- 验收：成员权限严格是创建者的子集；递归创建超深度被拒绝而非静默截断；成员崩溃必然产生可被 Core 消费的终态；清空上下文后模型历史为空但人类 transcript 完整；成员 Run 被 `Fenced` 不牵连其他成员。

### T22. ModelSurface：append-only log 与模型历史投影

- 阶段：Phase A（M0）。这是 v1.4 引入的**结构层地基**，先于 T11 上下文装配。
- 依赖：T01、T03。
- 参考：R38、R33。
- 借鉴：Harness `core/session` 的 Surface 层——log 保持 append-only，模型历史是其上的有序投影，压缩以 replacement 节点遮蔽 range 而非改写历史。
- 实施：定义进入 Surface 的三类事件（`UserMessage`/`AssistantMessage`/`ToolResult`）与 `SurfaceOp{Append, Replace{range, generation}}`；实现纯函数 `derive_messages(log_prefix) -> Vec<Message>` 作为**唯一**模型历史投影规则；`AssistantMessage` 携带 `source_event_seqs`（含显式空列表）；空 content 的 assistant 事件保留但不入派生历史；区分 append-origin（人类 transcript 源）与 replacement 副本（model-only）。
- 不照搬：不引入 Harness 的 Cordis 事件总线；不把 chunk 事件当 durable 恢复依据。
- 交付：Surface 类型、`derive_messages` 纯函数、append/replace fixtures、人类 transcript 与模型历史的双投影测试。
- 验收：同一 log 前缀始终派生同一组消息；压缩后人类 transcript 不丢失用户已看到的对话；live delta 全部丢失时 committed transcript 仍可重建；内核内外不存在第二份模型历史构造逻辑。

### T22A. Model-visible-is-logged 运行时不变式

- 阶段：Phase A（M0）。
- 依赖：T22、T11。
- 参考：R41、R38。
- 借鉴：Harness 的 `ctx.invariants` 注册表——把"模型可见即已记录"从文档规定变成可执行断言。
- 实施：在每次组装模型请求时断言"请求中每条消息都能在 log 前缀的 Surface 投影中找到对应节点"；debug 构建 panic，release 构建产生 `RunFailed{InvariantViolated}`；提供按模块注册、可配置开关的不变式注册表。
- 不照搬：不实现 Harness 的包级 allowlist/blocklist 正则配置，Rust 侧用 feature flag + 配置项即可。
- 交付：不变式注册表、断言实现、故意违规的负向测试。
- 验收：人为构造一条"绕过 log 直接进请求"的路径必然被捕获；不变式关闭时无性能影响。

### T23. Fork：对话分叉

- 阶段：Phase B。
- 依赖：T02A、T03、T22。
- 参考：R38（`ctx.sessions.fork`）、R05（aionrs `forked_from/root_id`）。
- 借鉴：两个参考实现都有的分叉能力——aionrs 记谱系但加载时不跟随，Harness 提供带 boundary 的 fork。
- 实施：定义 `ForkSpec{source_run_id, boundary, new_run_id}`；从源 Run 的 durable 前缀派生新 Run 的 `ConversationSnapshot`；boundary 必须落在 durable seq 上；`forked_from/root_id` 只作记录、加载时不跟随；被引用的 `ContentRef` 由新 Run 重新 `retain`；不继承 inbox、未决审批、in-flight 工具与 grant；`AuthorityEnvelope` 由 Core 重新签发且不得更宽。
- 不照搬：不实现 pi 的 lane/分支树；不允许 fork 复活源 Run 的 live 状态。
- 交付：ForkSpec、派生逻辑、谱系字段、retain 转移测试。
- 验收：源 Run 归档后新 Run 仍可完整重建；fork 不继承任何 live handle；fork 不能扩大权限；从 boundary 分叉的新 Run 其 Surface 与源 Run 该前缀一致。

### T21. Host Conformance Suite（宿主义务验证）

- 依赖：T02、T02A、T02B、T02C、T09、T09A；随各端口增量补齐。
- 参考：R14（harness port 化资源与结果类型）、R15。
- 借鉴：pi 的 harness port 契约测试思路；扩展为**面向第三方宿主的自测套件**。
- 实施：在 `agentrs-testkit` 提供可被任意宿主 adapter 复用的契约套件，逐条验证 H1–H7：

| 用例 | 验证 |
|---|---|
| H1 | 篡改 `input_hash` 或使用过期 grant 时 `SandboxExecutor` 必须拒绝 |
| H2 | 隔离真实生效；`reconcile` 对 `not_started/running/unknown` 如实报告 |
| H3 | `event_id` 幂等、`seq` 单调、epoch 围栏、checkpoint 写序 |
| H4 | `retain` 有效期内内容不被回收 |
| H5 | `Allow` 裁决被记录且可审计 |
| H6 | 任何 `HookOutcome` 都无法放宽授权 |
| H7 | 同一 Run 的所有执行看到一致的 ChangeSet overlay |

- 不照搬：套件不测试宿主的业务逻辑、UI 或商业策略，只测承重义务。
- 交付：conformance crate、可执行报告、失败诊断信息、集成文档。
- 验收：故意实现的「坏 adapter」（不校验 hash、非幂等、提前 GC、overlay 不一致）能被逐条捕获；套件随开源仓库发布。
- **理由**：内核无法阻止恶意或错误的 adapter，v1.2 把 H1–H7 这类宿主义务写成了内核保证。开源之后，没有这套件「AgentRS 是安全的」在第三方宿主上不成立。

## 4. 推荐排期与并行方式

> 排期修订说明（v1.3）：v1.2 的 10 周 / 3 人排期存在两处不成立之处。其一，第 1-3 周要求同时交付契约体系、fake testkit（含 interleaving scheduler）、事件/checkpoint 协议、完整 composition kernel、版本化 Projection Registry + 7 类投影、Component manifest/Inventory 与 RuntimeHost 主循环——作为参照，aionrs 语义远更简单的 `engine.rs` 单文件即 1690 行。其二，人员分配把 M0 的七个任务中的六个交给同一人（"Rust runtime 开发负责 T01-T04/T03A/T03B/T10"），所谓并行实际不存在。下表按「M0 只求端到端可恢复闭环」重排，并把 Projection、Component、generation 系统后移。

| 周期 | 主线任务 | 可并行任务 | 里程碑 |
|---|---|---|---|
| 第 1-4 周 | T01、T02、T02A、T02B、T02C、T03、**T22 Surface**、**T22A 不变式**、T03A（收缩版）、T04、T04A、T04B | **T00 A 类移植（第 2 周起）**、T05A 真实 provider 冒烟、T17 `serve/doctor` 骨架 | **M0**：端到端可恢复闭环——fake provider 下跑通文本/单工具/steering/审批挂起恢复，五个 durable 边界崩溃可恢复，取消无残留，模型历史由 Surface 唯一投影 |
| 第 5-8 周 | **T00 B 类移植**、T05、T05B、T05C、T06、T06A、T07、T08、T09、T09A、T10、**T23 Fork**、**T24 ExternalFact** | T03B 投影、T11 manifest/token、T17A dev adapter、T19 指标、T21 首版 | **M1**：真实模型/工具闭环 + 缓存命中达标 + 读己之写正确 + 工具因果链完整 + **可 dogfooding** |
| 第 9-12 周 | T11、T12、T13、T14、T15、T16、T19A、**T20 ChildRun**、**T25 BoardSnapshot**、**T26 团队审批/预算** | T19 lifecycle/OTel、T21 补齐 | **M2**：模型输入可重建，Trajectory 可查询/回放/脱敏导出 |
| 第 13 周起 | T07A、T07B、T07C、T18、T19 | **T27 MemberRunSpec**（待 Core 编排面） | **M3**：generation 系统、Component Inventory、声明式 profile、进程外插件与受限多 Agent |

人员建议（三人，避免单点串行）：

| 角色 | 负责 | 说明 |
|---|---|---|
| Runtime A | T01、T03、T03A、T04、T04A、T04B、T10 | 主循环与生命周期 |
| Runtime B | T02、T02A、T02B、T02C、T21、T05A、T09、T09A | 端口、testkit、宿主契约、Sandbox 协作 |
| Provider/Context | T05、T05B、T05C、T06、T06A、T07、T08、T11-T16 | provider、缓存、上下文与子 Agent |

M0 的关键路径由 Runtime A 与 Runtime B 分担，端口/testkit 与主循环可真正并行。T03B、T19/T19A 由第三人在 M1/M2 承接。T07A-T07C、T18、T20 由 AgentRS 与 AgentCore/SandboxRS 团队共同评审。

**里程碑取舍原则**：M0 不以「组件可诊断」为目标，只以「崩溃后能确定性恢复」为目标。generation 系统、ComponentManifest/Inventory、ResourceAccess 冲突图三项统一延后至真实触发条件出现（≥2 provider / MCP / per-run component / 实测并行收益），不为尚未采用的能力预付复杂度。通用进程内插件 ABI 不在当前排期。

### 4.1 跨团队确认的排期挂钩

架构方案 §17 列出的 Q1–Q10 是本方案有意留空的接缝，它们直接卡任务：

| 确认项 | 卡住的任务 | 最晚时点 | 未确认时的兜底 |
|---|---|---|---|
| Q1 SandboxGrant 载荷/签名 | T09 | Phase A 末 | 按 opaque 载体 + hash 绑定实现，fake 覆盖 |
| Q2 ChangeSet overlay 形态 | T09A | Phase A 末 | 按 overlay 语义实现契约测试，真实形态待接 |
| Q3 ContentStore 后端/GC | T02A | Phase A 中 | fake in-memory + retain 计数 |
| Q4 审批令牌保管/唤醒 | T04B | Phase A 末 | token 视为 opaque，Core 侧存储 |
| Q5 事件存储形态/保留期 | T02B、T19A | Phase A 中 | JSONL 假设 + 分页契约 |
| Q6 steering 的 UI 语义 | T04A | Phase B 中 | 默认"排队追加"，打断由 cancel 组合 |
| Q7 Hook 配置来源/执行形态 | T02C | Phase B 中 | 端口就位，fake runner |
| Q8 缓存成本目标与验证端点 | T05C 的收益验收 | Phase B 初 | 功能验收照常推进；收益验收待确定可用端点 |
| Q9 Team TaskList API | T20 | Phase D 前 | 不启动 |
| Q10 开源切分与 CI | T21 发布 | Phase C 前 | 单仓假设 |

规则：确认前按最保守假设实现并用 fake 覆盖；确认后若假设不成立，改动必须限制在 adapter 层，不得反向污染内核契约。到最晚时点仍未确认的，转为「按假设交付 + 记技术债」并在 ADR 写明推翻代价。

## 5. 任务完成总门禁

任一任务合入前必须满足：

1. 相关 trait、事件、prompt/schema 有 fixture 和兼容性测试。
2. 新 I/O 都经 port 注入；不允许在 AgentRS 引入宿主文件、进程、数据库或 Electron 依赖。
3. 有副作用的路径能证明 `StepIntent -> Policy -> Sandbox -> StepResult` 完整存在。
4. 新上下文来源有 token 预算、可见范围和裁剪策略。
5. 新子 Agent 有固定 profile、工具集、模型档位、输入/输出 schema 和取消语义。
6. 日志、错误和 telemetry 不包含用户敏感正文或密钥。
7. 新 registration/task/stream 有唯一 ScopeId/owner，具备 cancel、drain、幂等 cleanup 测试；cleanup 不得冒充外部副作用回滚。
8. 新 operation 明确 committed dependency view；测试中 `capability_digest` 在一次 operation 内不变（Phase D 起追加 generation 维度）。
9. 任何新模型可见内容都有 durable event 或**已 retain 的 `ContentRef`**来源，并进入 ModelRequestManifest；不得引入第三种物理来源。
9A. 新 durable 写入路径必须携带 `RunEpoch`，事件必须有确定性 `event_id` 作幂等键。
9B. 任何改写缓存前缀段（S0–S3）的新机制必须说明它在哪个 compaction 边界批量执行，并补 `CacheBreakCause` 归因。
9C. 任何新的 provider 侧不兼容（签名、block 类型、id 格式）必须收敛为一个 `LegalizationOp`，不得散落在 assembler 或 projector 中。
9D. 新增有副作用的执行路径必须携带 `change_set_id` 并进入 `input_hash`。
10. 新 middleware 不得绕过 schema、capability/grant、StepIntent、Sandbox enforcement 或 StepResult commit。
11. 并发能力必须给出 `concurrency_safe` 判定并有稳定提交顺序测试；未声明时默认串行。`ResourceAccess` 作为声明字段一并填写，但在 Phase D 之前不驱动调度器。
12. 新 durable event 必须说明 Trajectory projection、兼容性和 retention；新 projection 是确定性纯 fold，带 stateVersion，不能回写事实流。
13. Inventory 只能投影 Composition Kernel 权威状态；manifest/config 必须 schema/API/authority 校验，缺失依赖和不变量失败必须可诊断。
14. replay/export 默认脱敏且不自动解引用敏感 content；AgentRS 不负责上传，AgentUI 不直接读取 runtime 内部对象。
15. 外部 Component 的进程、凭据、网络和资源限制归 Core/Sandbox；不得用 profile、overlay 或 middleware 扩大 AuthorityEnvelope。
16. 开源发布前通过许可证/NOTICE/来源、secret、默认执行路径和公共 API 文档门禁。
17. 安全相关的新约束必须明确归入「内核不变量」或「宿主义务」；后者必须同时在 T21 conformance suite 增加对应用例，不得写成内核保证。
18. 涉及性能的任务必须对照架构方案「性能与容量预算」表给出实测数字，定性描述不算通过。
19. 任何新的模型可见内容必须先成为一个 Surface 事件，再由 `derive_messages` 投影；不得存在第二份模型历史构造逻辑，T22A 的运行时不变式必须能捕获违规。
20. 任何"改写历史"的实现必须是追加 `Replace` 节点；人类 transcript 必须仍能从 append-origin 事件重建。
21. **任何从 aionrs 移植的文件必须带来源标注**（源路径 + commit + 修改摘要）并登记进 `THIRD-PARTY-NOTICES.md`，CI 门禁检查；A 类移植必须同时搬入原测试。
22. C 类模块（§3.3.1）不得进入仓库，包括以"临时/调试用"为由。
23. 任何跨 Run 进入模型的内容必须经 `ExternalFact` 通路，并有"只带内容不带能力"的负向测试；不得新增旁路注入。
24. 任何共享可变状态进入上下文前必须固化为不可变快照 ref；成员 Run 不允许出现运行时查询外部状态的代码路径。
25. 新增的多 Agent 能力必须说明它是 ChildRun（parent-owned）还是 MemberRun（平级），并给出对应的 epoch、取消与审批归属。
26. **新增 durable 事件必须携带恢复所需的载荷**；无字段变体无法支撑 `RecoveryPlanner`（架构 §18.1 缺口 4）。
27. **任何进入模型请求的内容都必须先进 Surface**，请求只从 `derive_messages` 投影；直接装配会让运行时不变式失去对象（§18.2 修正 5）。
28. **工具结果必须回灌**（小输出走 `output`，大输出走 `artifacts`）；不回灌会让模型凭空编造后续内容——这是端到端才暴露的一类缺陷（§18.1 缺口 6）。
29. 每个 Phase 的出口标准保留一条"能干成一件事"的真实闭环。W1–W10 的经验是：235 个单测全绿时仍有四个装配级缺陷存活，**只有真实跑起来才发现**（§18.3）。

## 6. 与总方案的职责对齐

> 本表是**已决判例**，不是判定规则。新能力的归属请先用架构方案 §1.1 的四个正交测试判定；只有判定结果需要记录时才在此增行。

| 能力 | AgentRS | AgentCore | SandboxRS |
|---|---|---|---|
| 推理、工具调用提议、上下文压缩 | 实现 | 注入配置/持久化 | 不参与 |
| Token 计数 | 内置保守估算器（防超窗） | `TokenCounter` port 提供精确计数 | 不参与 |
| Prompt 与工具描述 | 行为契约与安全规则；描述质量 lint 与 eval | 身份/语气/品牌经 `SystemContext`；工具描述文本 | 注册执行类工具描述 |
| 模型档位 | 档位语义（零工具、预算约束） | 档位→模型 id 映射，经 `ModelPolicy` | 不参与 |
| 错误 | 稳定错误码 | 用户文案与 i18n | 不参与 |
| ModelSurface 与模型历史投影 | 定义 Surface 语义与唯一 `derive_messages` | 持久化 log、提供 UI transcript | 不参与 |
| 对话分叉 | 定义 ForkSpec 与派生规则 | 谱系存储、UI 分支管理 | 不参与 |
| 跨 Run 消息 | 定义 `ExternalFact` 与 inbox→Surface 通路 | Team 实体、邮箱存储、路由、可见性、投递重试 | 不参与 |
| 任务板 | 定义 `BoardSnapshotRef` 固化契约 | 存储、DAG 校验、环检测、租约、死锁升级 | 不参与 |
| 成员生命周期 | `MemberRunSpec` 派生规则、确定终态 | 创建/调度/唤醒/崩溃巡检/回收 | 不参与 |
| 团队审批与预算 | 拒绝 agent 来源的裁决、如实上报消耗 | 池化存储与再分配、最终裁决 | 不参与 |
| 内容寻址存储 | 定义 `ContentStore` 契约与 ref liveness | 实现存储、加密、GC | 不参与 |
| 事件幂等/围栏/写序 | 定义语义与 epoch 协议 | 实现并保证（H3） | 不参与 |
| 缓存前缀布局与断点 | 实现分段与断点放置、归因指标 | 不参与 | 不参与 |
| 历史合法化 | 实现固定阶段并留痕 | 不参与 | 不参与 |
| 运行中输入注入（steering） | 提供 `submit` 与安全边界 | 决定 UI 语义（打断 vs 追加） | 不参与 |
| 审批挂起与恢复 | 有界等待、挂起、凭 token 恢复 | 最终裁决、令牌保管、唤醒 | 不参与 |
| PermissionMode / Plan Mode | 作为 Run 级状态约束目录与提示 | 授权模式切换 | 按 grant 强制 |
| ChangeSet overlay 一致性 | 消费并绑定工作集版本 | 提交/丢弃 | 提供一致 overlay（H7） |
| Hook | 定义端口，只消费建议/否决 | 实现 hook 运行器与配置 | 不参与 |
| Capability composition/lifecycle | 管 scope、owner 与 committed view（generation 为 Phase D） | 提供 AuthorityEnvelope/受信 component 配置 | 不参与 |
| Trajectory/Projection | 定义事实语义、纯投影、查询/replay/export contract | 持久化、可见性授权、向 UI 提供数据 | 不参与 |
| Component catalog/profile | 校验 manifest、组合已授权 Component、只读 Inventory | 安装、签名、市场、最终启停和商业策略 | 管进程外执行限制 |
| 模型凭据、账号模型策略 | 按 ModelPolicy 调用 | 管理/签发 | 不参与 |
| 审批与策略 | 等待/消费决策 | 最终裁决/记录 | 强制执行 grant 边界 |
| 命令执行、文件变更 | 请求/消费结果 | 签发 grant、提交 ChangeSet | 隔离执行、生成 ChangeSet/undo |
| FTS/项目记忆文件 | 选择候选/注入 fragment | 索引、权限、保留 | 不参与 |
| UI/CLI | 轻量 CLI 协议 | 桌面后端 API | 不参与 |

结论：AgentRS 保持“可嵌入、可恢复、可控、可解释”的开源推理内核。aionrs/pi 提供 Agent 主体与 durable recovery；DeepSeek Harness/Cordis 补足 live composition、Trajectory、Projection、Inventory、代际和 capability seam。开源范围覆盖内核、契约、Component SDK、trajectory/replay、testkit 与 **host conformance suite**；账号、计费、凭据、签名市场与组织策略留在 AgentCore。所有改变用户数据或执行外部代码的责任仍停留在 Core/SandboxRS，AgentRS 不装载任意进程内插件。

v1.3 的取舍可以概括为一句话：**把复杂度从「为未采用的能力预付」转移到「承重契约与真实故障路径」**——补齐 ContentStore、Grant 签发、事件幂等与围栏、steering、审批挂起五处此前无机制支撑的空洞，确立缓存前缀与历史合法化两条被完全忽略但必然触发的不变式，同时把 generation 系统、Component Inventory 与资源冲突图推迟到真实触发条件出现。

# agentrs 子智能体调度、通信与协作 —— 深度分析与优化指引

> 本文是一份**独立、自足**的工程指引文档：读完它即可在不重新调研的前提下动手改造 agentrs 的子智能体子系统。
> 前四章是事实基线与对标结论（回答"现在是什么、别人怎么做、差在哪"），第五章起是可直接执行的改造方案（回答"改成什么、怎么改、怎么验收"）。

- 文档版本：v1.0
- 编写日期：2026-09-07
- 代码基线：`agentrs` @ `6dbf5116e4`（main，工作区干净）
- 对标基线：`opensource/claude-code-main`、`opensource/opencode`
- 结论依据：静态阅读 + 新增端到端集成测试 + 探针实测 + 全量 `cargo test --workspace`

---

## 目录

1. [现状基线](#一现状基线)
2. [实测证据](#二实测证据)
3. [横向对标：claude-code 与 opencode](#三横向对标claude-code-与-opencode)
4. [差距清单](#四差距清单)
5. [目标形态与设计原则](#五目标形态与设计原则)
6. [分阶段改造指引](#六分阶段改造指引)
7. [接口契约草案](#七接口契约草案)
8. [测试策略与回归基线](#八测试策略与回归基线)
9. [取舍记录](#九取舍记录)

---

## 一、现状基线

### 1.1 一句话概括

agentrs 的子智能体是一个**单层、一次性、无回传通道**的 fan-out 模型：父进程内 `tokio::spawn` 出若干独立 `AgentEngine`，各自跑完后把纯文本结果一次性交回父的 `tool_result`。子智能体**没有身份、不落盘、不可寻址、不可续跑、不可取消**。

### 1.2 两条调度入口，共用一个 `AgentSpawner`

| 入口 | 代码位置 | 并发度 | 单体预算 | 覆盖项 |
|---|---|---|---|---|
| `Spawn` 工具（模型可见） | `crates/agentrs-agent/src/spawn_tool.rs:77` | 单次最多 5 个，`tokio::spawn` 并发 | 200 turns / 4096 output tokens | 无 |
| fork 模式 skill | `crates/agentrs-skills/src/executor.rs:78` | 单个 | 10 turns / 16384 tokens | `ForkOverrides`：model / effort / allowed_tools |

`Spawner` trait 定义在 `crates/agentrs-types/src/spawner.rs`，使 `agentrs-skills` 无需反向依赖 `agentrs-agent`，也让测试可注入 mock —— **分层方向正确，改造时应予保留**。

### 1.3 子智能体的运行时形态

`AgentSpawner::spawn_one`（`spawner.rs:68`）为每个子构造独立 `AgentEngine`：

| 维度 | 当前取值 | 影响 |
|---|---|---|
| 输出 | `NullSink`，流式输出静默丢弃 | 用户看不到子的任何过程，无进度、无 token 计数 |
| 会话 | `session.enabled = false` | 子不落盘 ⇒ 无法续跑、无法回看、无法恢复 |
| 审批 | `tools.auto_approve = true` | 子的写操作一律免审批 |
| 工具 | `build_tool_registry`（`spawner.rs:229`）**硬编码 6 个**：Read/Write/Edit/ExecCommand/Grep/Glob | 子永远拿不到 MCP / ViewImage / ToolSearch，即使父有且策略允许 |
| 策略 | `effective_child_tool_policy`（`spawner.rs:204`）对父策略取交集 | fork 的 `allowed_tools` 只能收窄不能扩权 —— **设计正确，有单测** |
| 提示词 | 继承父的 `config.system_prompt` 原文 | 提示词宣传 Spawn/Skill/Web，实际工具集没有 |

由此产生两条**结构性**（而非可配置）的保证：无递归（子无 `Spawn`）、无外联（子无 `Skill`/`WebFetch`/`WebSearch`/MCP）。

### 1.4 调度时序

- `Spawn::is_concurrency_safe() = false`（`spawn_tool.rs:69`）⇒ 在 `orchestration::partition` 中自成串行批次，父不会同时跑两组 fan-out
- `is_deferred() = true` ⇒ schema 以名字桩下发，模型需先 `ToolSearch` 才拿到完整参数表
- `spawn_parallel`（`spawner.rs:116`）并发启动、**按提交顺序 `await`** ⇒ 结果顺序稳定，与完成顺序无关
- `JoinError` 单独降级为一条 `is_error` 结果，不击穿整批

### 1.5 通信

**下行**：仅 `SubAgentConfig.prompt` 字符串，外加隐式继承的 `Config`（含 system prompt）、`ToolPolicy`、`cwd`、`runtime_env`。无结构化输入、无返回值 schema 约束。

**上行**：一次性纯文本。`spawn_tool.rs:104` 渲染为

```
## <name> [OK|ERROR]
<text>
[turns: N | tokens: I in / O out]
```

以 `\n\n---\n\n` 拼接，随后在 `orchestration::execute_single` 经 `compact_output` → `max_result_size`(50 KB) ∧ `tool_output_max_bytes` 做**头尾保留式截断**。全批仅当每个子都失败时才判 `is_error`。

**缺失**：无兄弟间通信、无运行中消息投递、无部分结果流式回传、无结构化输出约束。

### 1.6 协作

| 模式 | 存储 | 共享性 | 机制 |
|---|---|---|---|
| `TodoMode::List` | 内存 `TodoStore` | **隔离**：每个子建自己的 store | 整表替换 |
| `TodoMode::Graph` | `<workspace>/.agentrs/tasks/tasks.json` | **共享**：父子指向同一文件 | 读-改-写全程持文件独占锁 |

graph 模式是唯一的真协作面，且实现质量高：任务有稳定 `id`、`owner` 字段、`blocked_by` 双向边与 BFS 环检测（`task/store.rs:283`），`next_id` 持久化避免 id 复用导致依赖悬挂。**这是现有设计里最有价值的一块资产，改造时应作为地基而非重写对象。**

另一个事实上的共享介质是**文件系统**：所有子共享同一 `cwd`，均持 Write/Edit 且免审批，除任务图外无任何冲突防护。

---

## 二、实测证据

### 2.1 新增端到端测试

`crates/agentrs-agent/tests/subagent_collaboration_test.rs`，用按 prompt 分派的 `ScriptedProvider` 驱动完整子引擎（现有 `spawner_test.rs` 只在注册表层做隔离验证）。5 个用例全部通过：

| 用例 | 验证点 | 结果 |
|---|---|---|
| `a_graph_mode_child_claims_a_task_the_parent_planned` | 父建任务 → 子 `TaskUpdate` 认领（`owner=child`, `in_progress`）→ 父端 store 可见 | ✅ |
| `a_list_mode_child_leaves_no_plan_behind_in_the_workspace` | list 模式子不在 workspace 落下 `tasks.json` | ✅ |
| `an_unrestricted_child_still_cannot_spawn_or_reach_the_network` | 父策略 `Unrestricted` 时子仍拿不到 Spawn/Skill/WebFetch/WebSearch | ✅ |
| `a_failing_sibling_does_not_discard_the_other_results` | 3 子 1 失败：其余结果保留、顺序与请求一致、整批不判 error | ✅ |
| `a_batch_where_every_child_failed_is_reported_as_an_error` | 全失败才判 error | ✅ |

### 2.2 探针实测（定位问题 P0-2 / P1-3，未提交）

- 传入**已 cancel** 的 `CancellationToken` 调用 `SpawnTool::execute_with_follow_up` → 子照常跑完并返回 `## slow [OK] finished anyway`
  ⇒ **取消完全不传播**
- 父 `system_prompt` 设为 `"PARENT PROMPT: you may use Spawn, Skill and WebSearch."` → 子收到的 `request.system` 为该原文，而 `request.tools` 仅 `[Edit, ExecCommand, Glob, Grep, Read, TodoWrite, Write]`
  ⇒ **提示词与工具集不一致**

### 2.3 全量回归

`cargo test --workspace --no-fail-fast`：

- 仅 6 个失败，全部为直连 `api.openai.com` 的真实 LLM 验收用例（`compact_test::autocompact_triggers_llm_summary`、`compaction::case_9/10/11`、`openai::test_openai_single_turn_completion`、`openai::test_openai_tool_use`），受环境代理阻断，与子智能体无关
- 另有 18 个 provider / mcp 用例在设置 `http_proxy` 时误报（本地 mock server 被代理拦截），加 `NO_PROXY=127.0.0.1,localhost` 后全绿
- `cargo fmt --all`、`cargo clippy --workspace --all-targets` 无告警

---

## 三、横向对标：claude-code 与 opencode

### 3.1 子智能体的"本体"是什么 —— 根本分歧

| | agentrs | claude-code | opencode |
|---|---|---|---|
| 子的载体 | 进程内临时 `AgentEngine`，**无身份、无持久化** | 注册进 `LocalAgentTask` 表的**有 `agentId` 的任务实体** | **真实子会话** `Session{parentID}`，落 SQLite |
| 生命周期 | 调用内出生、返回即死 | 完成后仍在表中，可 `SendMessage` 续跑其 transcript | 完成后子会话仍在，`task_id` = 子 sessionID，可续 |
| 用户可见性 | 完全不可见（`NullSink`） | 面板 / 进度 / token 计数 / `TaskOutput` 可查 | UI 中即一条可展开子会话，可回看可 revert |

**这一条决定了后面所有能力**：能不能续跑、能不能中途发消息、结果能不能重放，全部由此派生。agentrs 的子是"匿名一次性函数调用"，claude-code 是"可寻址的常驻任务"，opencode 是"可持久化的子会话"。

### 3.2 调度

| 维度 | agentrs | claude-code | opencode |
|---|---|---|---|
| 并发触发 | `Spawn` 单调用最多 5 个；工具自身 `is_concurrency_safe=false` | `AgentTool.isConcurrencySafe()=true` ⇒ 同一条 assistant 消息里多个 `Agent` 调用天然并发 | 同一条消息多个 task 调用并发（`task.txt` 第 1 条明确鼓励） |
| 全局闸门 | 无 | `CLAUDE_CODE_MAX_TOOL_USE_CONCURRENCY`（默认 10，`toolOrchestration.ts:10`）；`StreamingToolExecutor.canExecuteTool` 遇非并发安全工具即断批 | `BackgroundJob` 注册表 |
| 深度控制 | **结构性禁止**（子无 `Spawn`），恒为 1 | 不禁止；由 agent 定义的 `tools` 白名单决定；fork 路径靠历史中嗅探 `<fork-boilerplate>` 防递归 | **显式可配** `cfg.subagent_depth ?? 1`，沿 `parentID` 链实际计数后拒绝；并默认 deny 子的 `task` 权限 |
| 执行形态 | 仅同步阻塞 | sync（共享父 abortController）/ async（独立 controller + 任务表）/ **fork**（继承父精确工具池与已渲染 system prompt 字节） | foreground / background，且**可互转**：`promote()` 前台转后台、`extend()` 向运行中后台任务追加上下文 |
| 隔离 | 全部共享父 `cwd` | `isolation: 'worktree' \| 'remote'`，未改动的 worktree 自动清理 | 有 `Worktree` 服务，但属会话级设施 |

> **claude-code 的 fork 缓存对齐**值得单独记一笔：`buildForkedMessages` 给所有 `tool_use` 填**同一个占位 result**，只让最后一个 directive 文本块不同，使多个 fork 子的 API 请求前缀 byte-identical 从而共享 prompt cache；`useExactTools` 与复用父已渲染的 system prompt 字节也是同一目的（注释明确写了"重新调 getSystemPrompt 会 GrowthBook 冷热分叉、打爆缓存"）。
> agentrs 当前"继承父提示词原文"看似相似，方向却相反：既没拿到缓存收益（工具表已不同，前缀本就不一致），又制造了提示词与工具集的矛盾。

### 3.3 通信

**下行**

| | 内容 |
|---|---|
| agentrs | 仅 `prompt` 字符串 |
| claude-code | `prompt` + `description` + `subagent_type`/`model`/`mode`/`isolation`/`name`/`team_name`；子按 agent 定义**重建** system prompt，可预载 skills，可挂**自己的 MCP server**（additive），触发 `SubagentStart` hook 注入上下文 |
| opencode | `prompt`（含 `@file` 等 part 模板，`resolvePromptParts`）+ `subagent_type` + `task_id` + `background`；子会话带自己的 agent 定义（prompt/model/variant/permission/steps） |

**上行**

- **agentrs**：Markdown 拼接字符串，一次性
- **claude-code**：`<task-notification>` XML 作为 **user-role 消息**注入父会话，含 `<task-id>` `<status>` `<summary>` `<result>` `<usage>{total_tokens, tool_uses, duration_ms}`；coordinator 提示词专门教模型"它看起来像用户消息但不是"。对 `Explore`/`Plan` 这类一次性 agent 省略 agentId/SendMessage/usage 尾巴 —— 注释写着 "~135 chars × 34M Explore runs/week"，是按量算过账的
- **opencode**：`<task id state><task_result>` XML；后台完成时以 `synthetic: true` 的 user part 注入父会话

**运行中通信 —— agentrs 完全缺失的一整层**

- **claude-code** `SendMessage`：`to` 可为 teammate 名 / `agentId` / `"*"` 广播 / `uds:<socket>`（同机跨会话）/ `bridge:<session-id>`（跨机）。投递走 `queuePendingMessage` → 子在下一工具轮 `drainPendingMessages`，且子只 drain 寄给自己的通知（`query.ts:1575`）。另有结构化协议消息 `shutdown_request/response`、`plan_approval_request/response`；tmux teammate 走文件 mailbox。提示词里那句 **"Your plain text output is NOT visible to other agents"** 是这套模型的宪法
- **opencode**：无 agent-to-agent 通道；最接近的是 `background.extend()`（父→运行中子，单向）
- **agentrs**：零

### 3.4 协作

| 共享面 | agentrs | claude-code | opencode |
|---|---|---|---|
| 任务/计划 | graph 模式共享 `tasks.json`（文件锁 + owner + DAG 环检测）；list 模式隔离 | `TaskCreate/Get/List/Update` 共享任务表；`TodoWrite` 私有 | todo 按 `sessionID` 存，子**默认被 deny todowrite** |
| 跨运行记忆 | 无 | **agent-memory**：按 agent type 的持久记忆目录，`user`/`project`/`local` 三 scope | 无 |
| 共享工作区 | 同 cwd，无协调 | coordinator 的 **scratchpad 目录**（免审批读写，承载跨 worker 持久知识）；worktree 隔离 | 会话级 worktree |
| 编队 | 无 | `TeamCreate/TeamDelete` + team lead + tmux 面板 + 广播 | 无 |

> **agentrs 的相对优势**：graph 任务图的一致性保证（OS 文件独占锁贯穿读改写、`next_id` 持久化、双向边 + 环路径渲染）实际**强于 claude-code 的 TaskCreate 系列**。问题不在这块本身，而在于"没有别的东西围绕它"——子既不能主动汇报，也不能被中途指挥。

### 3.5 生命周期与取消

- **agentrs**：❌ 无任何取消路径
- **claude-code**：sync 子共享父 `abortController`；async 子独立 controller + `killAsyncAgent` + `TaskStop` 工具 + `killAllRunningAgentTasks`；有 `SubagentStop` hook
- **opencode**：`ctx.abort` 监听 → `ops.cancel(childSessionID)`；`Effect.acquireUseRelease` 在 `Exit.hasInterrupts` 时同时 cancel 会话与 background job；父会话删除时 `remove()` **递归删子会话并取消其后台作业**

### 3.6 成本与可观测

- **agentrs**：`usage` 只拼进文本给模型看，不进 `total_usage` 会话累计用量（上下文口径无需计入，见 P1-4 澄清）
- **claude-code**：`ProgressTracker` 实时统计 token / 工具调用 / 活动描述，面板可见，`<usage>` 回传父，`costHook` 全局归集
- **opencode**：子会话消息与 token 走标准会话存储，天然计入

---

## 四、差距清单

按严重度排序。每条给出证据、代码位置与影响面。

### P0-1　fork 模式 skill 在生产路径上不可用

**证据**：`bootstrap.rs:355` 使用 `SkillTool::new(...)`（`spawner = None`、`session_id = None`），而同一函数 `bootstrap.rs:361-368` 已构造 `AgentSpawner` 交给 `SpawnTool`。全仓库对 `SkillTool::with_spawner` 的调用**只存在于测试**。

**影响**：任何 `execution_context: fork` 的 skill 在真实 CLI 中必然返回 `"Skill '<name>' requires fork execution context, but no AgentSpawner is available."`；同时 `${AGENTRS_SESSION_ID}` 替换在所有 skill 中恒为空。

**根因**：现有 skill 单测全部自建 `SkillTool`，因此没有任何测试覆盖 bootstrap 装配路径。

### P0-2　父的取消信号不向下传播

**证据**：`SpawnTool` 未覆写 `execute_cancellable`，走 `Tool` 默认实现丢弃 `CancellationToken`；`spawn_parallel` 丢弃 `JoinHandle` 无法 abort。已实测确认。

**影响**：用户 Ctrl-C 后最多 5 个子各自可再跑满 200 轮，既烧钱又可能继续写文件。

### P1-3　子继承父的 system prompt 原文

**证据**：`spawn_one` 仅在 `sub_config.system_prompt` 存在时覆盖，否则沿用父完整提示词（其中列举 Spawn / Skill / 技能清单 / 网络工具）。已实测确认。

**量化**：同一份 shell 环境下，父提示词（不受限策略 + 10 个 skill）**2496 B**，按子实际工具集重建后**833 B**，差额 **1663 B ≈ 420 token**（无 skill 时差额仍有 330 B）。该测量未含 memory 段与 AGENTS.md 段，是保守下界。按 5 子 × 8 轮估算，**每次 Spawn 批次浪费约 17k input token**。

**影响**：诱发幻觉工具调用、浪费轮次与 token。

### P1-4　子的 token 消耗不进父账本

**证据**：`SubAgentResult.usage` 仅拼进文本（`spawn_tool.rs:110`），未汇入 `total_usage`（会话累计用量口径）。

**影响**：5 个子的真实成本对父完全不可见，会话累计用量少算，预算判断失真。

**口径澄清**：子的 token **不应**计入父的 `context_state.context_usage`——它不占父的上下文窗口。父的上下文只因 `<subagent>` 返回文本增长，而该部分**已由 `record_tool_context_estimate` 正确计入**。因此自动压缩的触发依据当前是正确的，需要补的只有成本口径。误把子用量灌进上下文口径会导致压缩被过早触发。

### P1-5　缺少全局并发与预算上限

**证据**：`MAX_SUB_AGENTS = 5`、`DEFAULT_SUB_AGENT_MAX_TURNS = 200`、`DEFAULT_SUB_AGENT_MAX_TOKENS = 4096` 均为 `spawn_tool.rs` 内硬编码常量，且只约束**单次调用**。

**影响**：模型可连续发起多轮 Spawn，无上限；不可按部署环境调参。

### P2-6　子共享 cwd，无写冲突防护

所有子共享父 `cwd`，均持 Write/Edit 且 `auto_approve = true`，除 graph 任务图外无协调机制，无 worktree 隔离。

### P2-7　`SpawnTool::describe` 读错字段

`spawn_tool.rs:128` 读 `input["task"]`，schema 中字段名为 `tasks`（数组）⇒ UI 永远显示 `"Spawn: sub-agent"`。

### P2-8　子工具集硬编码而非从父注册表派生

`build_tool_registry` 写死 6 个工具，子永远拿不到 MCP 工具、`ViewImage`、`ToolSearch`——即使父有、策略也允许。这是"默认安全"的选择，但它是**隐式**的：新增工具时不会有人想起来同步这份列表。

---

## 五、目标形态与设计原则

### 5.1 定位判断

三者定位不同，**不应照抄 claude-code 的复杂度**：

- claude-code = 多 agent 协作平台（团队、邮箱、跨机 peer、持久记忆、tmux 面板）
- opencode = 会话树（子 = 可持久化子会话，工程上最简洁自洽）
- agentrs 当前 = 并行 map 函数

**agentrs 应当收敛到 opencode 的形态**：把子从"匿名函数"升级为"可寻址、可持久化的子会话"，而不是引入团队/邮箱/tmux 那一层。理由：

1. opencode 的模型用一个既有概念（session + parentID）就覆盖了续跑、取消级联、UI 可见、token 入账四件事，改造面最小
2. agentrs 已有 `SessionManager`，`session.enabled = false` 是**主动关掉**的，打开它即可获得地基
3. agent-to-agent 通信的前提是子有身份且长期存活；在没有第 1 步之前谈 `SendMessage` 是空中楼阁

### 5.2 设计原则（改造期间的判据）

1. **显式优于隐式**：深度、并发、预算都应是 `Config` 项，而非"因为子的工具表里没有 Spawn 所以不会递归"这种副作用式保证
2. **收窄不可逆**：子策略永远是父策略的子集，任何新增路径不得破坏 `effective_child_tool_policy` 的交集语义
3. **可寻址先于可通信**：先给子 id 与持久化，再谈消息投递
4. **成本必须可见**：任何新增的子执行路径都要把 `TokenUsage` 汇回父账本
5. **不破坏 graph 任务图**：它是现有最强资产，新机制围绕它建，不替换它

### 5.3 分层收益评估（选型依据）

把"对齐 claude-code"拆成三层评估，各层收益与成本差一个数量级。完整评估见姊妹文档 `agentrs-子智能体对齐claude-code-收益评估.md`，此处只保留结论与它对本方案阶段划分的约束。

| 层 | 内容 | 工作量 | 收益确定性 | 结论 | 对应阶段 |
|---|---|---|---|---|---|
| **L1 机制层** | 取消传播、子提示词重建、usage 入账、预算配置化、fork skill 修复 | 5–8 人日 | **确定且可量化** | ✅ 立即做 | 阶段 0–3 |
| **L2 交互层** | 子可寻址、结构化回传、续跑、进度可见、worktree 隔离 | 3–4 周 | 高 | ✅ 做 | 阶段 4–5 |
| **L3 平台层** | SendMessage 总线、Team/tmux、跨机 peer、agent-memory、fork 缓存对齐 | 2 月+ | **接近零** | ❌ 不做 | 见"明确不做的事" |

**三条关键判断**：

1. **L1 不是"对齐 claude-code"，是修 bug。** 取消无效、420 token/请求的提示词浪费、用量不入账——这些不存在设计权衡，claude-code 只是恰好都做对了。因此阶段 0–3 不需要任何选型讨论。
2. **L2 实际是"opencode 的骨架 + claude-code 的表皮"。** 子会话持久化（`parent_id`、`task_id` 续跑、删除级联）取自 opencode；`<task-notification>` 的字段设计、进度追踪、`isolation` 参数取自 claude-code。两者不冲突，5.1 节"对标 opencode"指的正是骨架层。
3. **L3 解决的是 claude-code 的规模问题，不是 agentrs 的问题。** 其代码注释里那句 "~135 chars × 34M Explore runs/week" 是按周千万次调用量做的优化决策，agentrs 没有对应的量级压力。

**fork 缓存对齐单独说明**：claude-code 让多个 fork 子的请求前缀 byte-identical 以共享 prompt cache，代价是子必须拿父的**精确工具池**（含 `Spawn` 与网络工具），它靠运行时嗅探 `<fork-boilerplate>` 打补丁来防递归。agentrs 引入这一优化等于主动废掉"子无 Spawn"的结构性保证，且收益需同时满足"Anthropic 系 provider + 开启缓存 + 父上下文够大 + 多子紧邻运行"四个条件。**不做。**

---

## 六、分阶段改造指引

每个阶段独立可发布、独立可回滚。阶段内标注：改动点 → 接口变更 → 验收标准。

### 阶段 0：止血（半天，无接口变更）

**目标**：修掉不需要设计决策的确定性缺陷。

| 项 | 改动 | 位置 |
|---|---|---|
| P0-1 | 把 `bootstrap.rs:361` 构造的 `Arc<AgentSpawner>` 同时交给 `SkillTool::with_spawner`，并传入当前 `session_id` | `bootstrap.rs:355` |
| P2-7 | `describe()` 改读 `input["tasks"]` 数组，渲染为 `Spawn: N 个子任务（name1, name2, ...）` | `spawn_tool.rs:128` |

**验收**：

- 新增 `crates/agentrs-agent/tests/` 下一个**走 bootstrap 装配路径**的集成测试：注册表取出 `Skill` 工具，对一个 `execution_context: fork` 的 skill 调用，断言**不**返回 "no AgentSpawner is available"
- 新增 `describe` 单测：三任务输入应产出含三个名字的描述

> ⚠️ 关键点：P0-1 的测试必须覆盖 bootstrap，不能再写一个自建 `SkillTool` 的单测——那正是漏掉这个 bug 的原因。

### 阶段 1：取消传播与可寻址（2-3 天）

**目标**：子获得 id 与生命周期控制。这是后续一切的地基。

**改动点**

1. `SubAgentConfig` 增加 `id: SubAgentId`（新类型，`agentrs-types`），由调用方生成；`SubAgentResult` 回填同一 id
2. `Spawner` trait 与 `AgentSpawner::spawn_one/spawn_parallel` 增加 `cancel: CancellationToken` 参数，透传到子 `AgentEngine`（复用其已有的 `turn_cancel`，见 `engine.rs:126`）
3. `SpawnTool` 覆写 `execute_cancellable`，把 token 透传下去
4. `spawn_parallel` **保留 `JoinHandle`**，注册到一个进程内 `SubAgentRegistry`（`agentrs-agent` 新模块），token 触发时 `abort()` 全部在跑的子
5. `SubAgentRegistry` 提供 `list()` / `get(id)` / `cancel(id)`，为阶段 3 的 `TaskStop` 与阶段 4 的消息投递预留

**验收**

- 端到端测试：传入已 cancel 的 token → 子在下一轮边界前终止，`SubAgentResult.is_error = true` 且文本标明 "cancelled"（当前实测行为是跑完，此测试现在必然失败，改完必须转绿）
- 测试：父 `run()` 被 `cancel_running_tools()` 中断时，`SubAgentRegistry` 清空
- 回归：`spawn_test.rs` 与 `subagent_collaboration_test.rs` 全绿

### 阶段 2：提示词重建与用量入账（2-3 天）

**目标**：消除提示词/工具集矛盾，让成本可见。

**改动点**

1. **提示词重建**：`spawn_one` / `spawn_fork` 不再沿用父 `config.system_prompt`，改为按子的 `ToolPolicy` 调用 `build_system_prompt_with_shell_and_tool_policy`（该函数**已经接受 policy 参数**，只是这条路径没调用它，见 `bootstrap.rs:300`）
   - 子的提示词不含 skills 清单、不含 Spawn/Web 段落
   - 保留 `sub_config.system_prompt` 覆盖优先级不变
2. **用量入账**：`SubAgentResult.usage` 除渲染进文本外，经 `SubAgentRegistry` 汇总回传父引擎，累加进 `total_usage`（成本口径）。**不得**累加进 `context_state.context_usage`——子不占父的上下文窗口，误加会导致压缩过早触发
3. 子的 `turns` 同样汇总，为阶段 3 的预算判断提供输入

**验收**

- 测试：父 `system_prompt` 含 "Spawn"，子收到的 `request.system` **不含** Spawn/Skill/WebSearch 段落，且与 `request.tools` 一致（当前实测行为相反，改完必须转绿）
- 测试：3 个子各消耗已知 usage → 父的 `total_usage` 增量等于三者之和，且 `context_state.context_usage` **不因子的用量变化**（只随返回文本增长）
- 测试：子提示词中的 workspace / shell 信息与父一致（不能因重建而丢失环境上下文）

### 阶段 3：配置化预算与深度（2 天）

**目标**：把隐式保证变成显式策略。

**改动点**

1. `Config` 新增 `subagent` 段（`agentrs-config`）：

   | 字段 | 默认 | 说明 |
   |---|---|---|
   | `max_per_call` | 5 | 单次 `Spawn` 最多子数（原 `MAX_SUB_AGENTS`） |
   | `max_concurrent` | 5 | workspace 级并发上限，用 `tokio::sync::Semaphore` 实现 |
   | `max_turns` | 200 | 单子轮次（原常量） |
   | `max_tokens` | 4096 | 单子输出 token（原常量） |
   | `depth` | 1 | 最大嵌套深度；`> 1` 时子的工具表才注入 `Spawn` |
   | `total_token_budget` | `None` | 一次父 turn 内所有子的 output token 总预算，超出后 `Spawn` 直接报错 |

2. 深度实现：`SubAgentConfig` 增加 `depth: usize`，`build_tool_registry` 在 `depth < config.subagent.depth` 时才注册 `SpawnTool`（此时需要把 spawner 传进子注册表——注意避免 `Arc` 循环，用 `Weak` 或延迟注入）
3. `SpawnTool` 在超限时返回**可操作的错误文本**（"深度上限 N，如需嵌套请调高 subagent.depth"），对齐 opencode 的做法

**验收**

- 测试：`depth = 1`（默认）时子无 `Spawn`；`depth = 2` 时子有 `Spawn` 而孙无
- 测试：`max_concurrent = 2` 时，5 个子的实际并发峰值不超过 2（用计数器 provider 断言）
- 测试：`total_token_budget` 耗尽后 `Spawn` 返回错误而非静默继续

### 阶段 4：子会话持久化与续跑（1 周，本方案的核心收益）

**目标**：子从"匿名函数"变为"可持久化的子会话"，对齐 opencode。

**改动点**

1. 打开子的会话：`spawn_one` 不再无条件 `session.enabled = false`，改为创建**带 `parent_id` 的子会话**（`SessionManager` 需支持 `parent_id` 字段与 `children(parent_id)` 查询）
2. `Spawn` 工具 schema 增加可选 `task_id`：传入即恢复该子会话并追加新 prompt，而非新建（对齐 opencode 的 `task_id` 语义）
3. 上行结果改为结构化：

   ```
   <subagent id="..." status="completed|error|cancelled">
     <summary>...</summary>
     <result>...</result>
     <usage turns="N" input="N" output="N" />
   </subagent>
   ```

   并在 `Spawn` 描述中说明 `id` 可用于续跑（对齐 claude-code 的 `<task-notification>` 与 opencode 的 `<task>`）
4. 取消级联：父会话删除时递归取消并删除子会话（对齐 opencode `Session.remove` 的 children 递归）
5. `NullSink` 替换为**转发到父 OutputSink 的带前缀 sink**，让 TUI 能显示子的进度与 token 计数（不显示子的全部流式文本，只发进度事件）

**验收**

- 测试：子会话文件落盘，`parent_id` 正确，父会话删除后子会话与其文件一并清除
- 测试：`Spawn{task_id}` 续跑同一子会话，第二轮请求的 messages 含第一轮历史
- 测试：结构化输出可被解析（写一个解析器测试，防止格式漂移）
- 测试：子的进度事件到达父 sink

### 阶段 5（可选）：注册表投影与工作区隔离

**目标**：解决 P2-6 / P2-8，属于结构性重构，收益低于前四阶段，可延后。

- 子工具集改为**从父注册表按策略投影**，并把 `Spawn`/`Skill` 的排除写成显式的、带注释的排除列表（而非"忘了加"的硬编码白名单）
- `Spawn` 增加 `isolation: "worktree"` 选项：为子创建独立 git worktree，未改动则自动清理（对齐 claude-code）

### 明确**不做**的事

| 不做 | 理由 |
|---|---|
| `SendMessage` 式 agent-to-agent 通信 | 前提是子有身份且长期存活；阶段 4 之前无落脚点。阶段 4 完成后可重新评估 |
| 团队 / tmux 面板 / 跨机 peer | claude-code 的产品形态，与 agentrs 的 CLI + JSON stream 定位不符 |
| agent-memory（按 agent type 的持久记忆） | `agentrs-memory` 已有跨会话记忆；再加一层按 agent type 的分片，收益不明确 |
| 重写 graph 任务图 | 它是现有最强资产，一致性保证优于对标实现 |

---

## 七、接口契约草案

以下为阶段 1-4 涉及的公共接口变更草案，供实现时对齐。遵循 `AGENTS.md` 的可见性规范：能 `pub(crate)` 的不 `pub`。

```rust
// agentrs-types/src/spawner.rs

/// 子智能体的稳定标识。父用它取消、续跑、寻址子。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubAgentId(String);

#[derive(Debug, Clone)]
pub struct SubAgentConfig {
    pub id: SubAgentId,
    pub name: String,
    pub prompt: String,
    pub max_turns: usize,
    pub max_tokens: u32,
    pub system_prompt: Option<String>,
    /// 当前嵌套层级，0 表示父直接派生的子。
    pub depth: usize,
    /// 续跑既有子会话；None 表示新建。
    pub resume_session: Option<String>,
}

#[derive(Debug)]
pub struct SubAgentResult {
    pub id: SubAgentId,
    pub name: String,
    pub text: String,
    pub usage: TokenUsage,
    pub turns: usize,
    pub status: SubAgentStatus,   // 取代裸 bool，区分 error 与 cancelled
    /// 子会话 id，可回填给 `SubAgentConfig::resume_session`。
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAgentStatus { Completed, Error, Cancelled }

#[async_trait]
pub trait Spawner: Send + Sync {
    async fn spawn_fork(
        &self,
        config: SubAgentConfig,
        overrides: ForkOverrides,
        cancel: CancellationToken,   // 新增
    ) -> SubAgentResult;
}
```

**兼容性说明**：`Spawner` 是跨 crate 契约（`agentrs-skills` 依赖它），签名变更需同步改 `execute_fork`；`SubAgentResult.is_error` 被 `status` 取代后，`spawn_tool.rs` 的渲染与 `executor.rs:104` 的分支都要跟着改。建议一次性完成，不要留兼容垫片。

---

## 八、测试策略与回归基线

### 8.1 测试分层要求（遵循 `AGENTS.md`）

| 层 | 位置 | 本次改造应覆盖 |
|---|---|---|
| 单测 | 同目录 `*_test.rs` | 策略交集、预算判定、深度计算、描述渲染 |
| 集成 | `crates/<crate>/tests/` | 端到端子引擎行为、bootstrap 装配路径、取消传播、续跑 |

**硬性要求**：每个阶段至少有一个测试是"改造前必然失败"的——否则说明该阶段没有真正改变行为。阶段 1 的取消测试、阶段 2 的提示词测试都属此类。

### 8.2 已有可复用资产

`crates/agentrs-agent/tests/subagent_collaboration_test.rs` 中的 `ScriptedProvider`（按 prompt 分派 + 记录 `advertised_tools` / `system`）可直接复用于后续所有阶段。若多个测试文件需要，应上移到 `tests/common/mod.rs`。

### 8.3 回归基线（改造前后须一致）

- `cargo test --workspace --no-fail-fast` 除以下 6 个真实 LLM 用例外全绿：
  `compact_test::autocompact_triggers_llm_summary`、`compaction::case_9/10/11`、`openai::test_openai_single_turn_completion`、`openai::test_openai_tool_use`
- `cargo fmt --all`、`cargo clippy --workspace --all-targets` 无告警
- **环境注意**：本机 `http_proxy` 会拦截测试用的本地 mock server，导致 18 个 provider / mcp 用例误报。运行前需设 `NO_PROXY=127.0.0.1,localhost`。建议把这条写进 `CONTRIBUTING` 或 `.cargo/config.toml`，避免后来者误判为回归。

---

## 九、取舍记录

记录本方案做出的判断及其理由，便于后续 review 或推翻。

| 决策 | 选择 | 理由 | 推翻条件 |
|---|---|---|---|
| 对标形态 | 收敛到 opencode 的"子会话"模型，而非 claude-code 的"多 agent 平台" | 一个既有概念覆盖续跑/取消/可见/入账四件事，改造面最小；agentrs 已有 `SessionManager` | 若 agentrs 产品定位转向多人/多机协作 |
| 深度控制 | 改为显式配置 + 计数，放弃"子无 Spawn"的结构性禁止 | 隐式保证顺带砍掉了 MCP/Skill/Web，且新增工具时无人会想起同步白名单 | 若出现无法约束的递归成本失控 |
| agent-to-agent 通信 | 暂不做 | 前提是子可寻址且长期存活，阶段 4 之前无落脚点 | 阶段 4 完成且出现真实的兄弟协作需求 |
| graph 任务图 | 保留并作为地基 | 一致性保证优于两个对标实现 | 无 |
| 阶段顺序 | 取消传播（阶段 1）先于持久化（阶段 4） | 取消是当前唯一"会造成实际损害"的缺陷（继续写文件 + 烧钱），持久化只是能力缺失 | 无 |
| 接口兼容 | `Spawner` 签名直接改，不留兼容垫片 | 该 trait 只有一个生产实现与一个调用方，垫片成本高于收益 | 若出现外部 crate 依赖该 trait |

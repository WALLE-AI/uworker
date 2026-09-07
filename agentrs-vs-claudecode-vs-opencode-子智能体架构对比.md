# agentrs vs claude-code vs opencode —— 子智能体架构深度对比

> 本文是一份**纯对比**文档：只回答"三者各自怎么做、为什么这么做、差异的根源在哪"。
> 需要 agentrs 的改造方案与实施步骤，见姊妹文档 `agentrs-子智能体调度通信与协作-深度分析报告.md`。

- 文档版本：v1.0
- 编写日期：2026-09-07
- 对比对象与基线
  - `agentrs` @ `6dbf5116e4`（`crates/agentrs-agent`、`agentrs-types`、`agentrs-skills`、`agentrs-tools`）
  - `opensource/claude-code-main`（`src/tools/AgentTool/*`、`src/tasks/*`、`src/coordinator/`、`src/services/tools/`）
  - `opensource/opencode`（`packages/opencode/src/tool/task.ts`、`agent/`、`background/`、`session/`）
- 方法：逐文件静态阅读，关键结论标注代码位置；agentrs 侧结论另有端到端测试与探针实测支撑

---

## 目录

1. [执行摘要](#一执行摘要)
2. [根本分歧：子智能体的"本体"是什么](#二根本分歧子智能体的本体是什么)
3. [调度](#三调度)
4. [通信](#四通信)
5. [协作与共享状态](#五协作与共享状态)
6. [生命周期与取消](#六生命周期与取消)
7. [成本与可观测性](#七成本与可观测性)
8. [权限与安全边界](#八权限与安全边界)
9. [逐维度总表](#九逐维度总表)
10. [三种架构的适用性判断](#十三种架构的适用性判断)

---

## 一、执行摘要

三者的子智能体机制看起来都是"派生一个子跑任务、拿结果回来"，但底层抽象完全不同，且**这个抽象决定了各自能力的天花板**：

| | 一句话概括 | 抽象 |
|---|---|---|
| **agentrs** | 并行 map 函数 | 进程内临时 `AgentEngine`，无身份、返回即死 |
| **claude-code** | 多 agent 协作平台 | 有 `agentId` 的常驻任务实体 + 消息总线 + 团队编制 |
| **opencode** | 会话树 | `Session{parentID}`，子就是一条持久化的子会话 |

**关键洞察**：能不能续跑、能不能中途发消息、结果能不能重放、取消能不能级联、token 能不能入账——这五件事**不是五个独立特性**，而是同一个问题（子是否是一等实体）的五个投影。agentrs 缺的不是五个功能，是一个抽象。

**反直觉的一点**：agentrs 并非全面落后。它的 graph 任务图（`agentrs-tools/src/task/store.rs`）在一致性保证上**强于两个对标实现**——OS 文件独占锁贯穿整个读-改-写、`next_id` 持久化防 id 复用、依赖双向边 + BFS 环检测并渲染环路径。问题在于没有任何东西围绕它建：子既不能主动汇报，也不能被中途指挥。

---

## 二、根本分歧：子智能体的"本体"是什么

### 2.1 三种本体

**agentrs —— 临时引擎实例**

`AgentSpawner::spawn_one`（`spawner.rs:68`）为每个子克隆一份 `Config`、造一个 `AgentEngine`、跑完取 `result.text`、丢弃。子没有 id、没有会话文件、不在任何注册表里。`config.session.enabled = false` 是**主动关掉**的。

**claude-code —— 注册表中的任务实体**

子注册进 `LocalAgentTask` 表（`src/tasks/LocalAgentTask/LocalAgentTask.tsx`，682 行），拥有：

- 稳定 `agentId`（`toAgentId`）
- `ProgressTracker`：实时 token 数、工具调用次数、当前活动描述（`updateProgressFromMessage`）
- 状态机：`registerAsyncAgent` → `updateAgentProgress` → `completeAgentTask` / `failAgentTask` / `killAsyncAgent`
- 前后台切换：`registerAgentForeground` / `backgroundAgentTask` / `unregisterAgentForeground`
- 消息队列：`queuePendingMessage` / `drainPendingMessages`

完成后**仍在表中**，可通过 `resumeAgent.ts` 或 `SendMessage` 从其 transcript 继续。

**opencode —— 带 parentID 的子会话**

`tool/task.ts` 中 `sessions.create({ parentID: ctx.sessionID, title: ..., agent: next.name, permission: [...] })`。子会话落 SQLite（`session/session.ts` 的 `SessionTable`，有 `parent_id` 列与 `children(parentID)` 查询），在 UI 中就是一条可展开、可回看、可 revert 的子会话。`task_id` 参数**就是子 sessionID**。

### 2.2 这个分歧的下游后果

| 能力 | agentrs | claude-code | opencode | 为什么 |
|---|---|---|---|---|
| 续跑同一个子 | ❌ | ✅ `SendMessage` / `resumeAgent` | ✅ `task_id` | 需要子的历史可寻址 |
| 中途向子发消息 | ❌ | ✅ `SendMessage` → `queuePendingMessage` | ⚠️ 仅后台任务 `background.extend()` | 需要子有 id 且存活 |
| 取消某一个子 | ❌ | ✅ `TaskStop` / `killAsyncAgent` | ✅ `background.cancel(id)` | 需要句柄可寻址 |
| 用户看到子的进度 | ❌（`NullSink`） | ✅ 面板 + 进度条 + token | ✅ UI 子会话 | 需要子有可订阅的事件源 |
| token 自动入账 | ❌ | ✅ `<usage>` + `costHook` | ✅ 走标准会话存储 | 需要子的消息在统一存储里 |
| 结果可重放 | ❌ | ✅ transcript | ✅ 子会话消息 | 需要持久化 |

**六项全部由"本体"决定**。agentrs 每补一项都要单独造轮子，而 opencode 因为选了"子会话"这个既有概念，六项几乎是免费的。

---

## 三、调度

### 3.1 并发触发方式

| | 机制 | 上限 |
|---|---|---|
| **agentrs** | `Spawn` 工具接受 `tasks[]` 数组，内部 `tokio::spawn` 并发 | `MAX_SUB_AGENTS = 5`（`spawn_tool.rs:14` 硬编码），**仅约束单次调用** |
| **claude-code** | `AgentTool.isConcurrencySafe() = true`（`AgentTool.tsx:1273`）⇒ 同一条 assistant 消息里的多个 `Agent` 调用天然并发，数量由模型决定 | 全局 `CLAUDE_CODE_MAX_TOOL_USE_CONCURRENCY`，默认 10（`services/tools/toolOrchestration.ts:10`） |
| **opencode** | 同一条消息多个 `task` 调用并发，`task.txt` 第 1 条明确鼓励"Launch multiple agents concurrently whenever possible" | 无显式数量上限，靠 `BackgroundJob` 注册表管理 |

**设计差异**：agentrs 把"一次派生多少"做成**工具参数**（`tasks[]` 数组），另两者做成**模型的自然行为**（发多个 tool call）。前者的代价是 `Spawn` 必须 `is_concurrency_safe = false`（`spawn_tool.rs:69`）以免两组 fan-out 撞车，于是失去了与其他工具并发的机会；后者由统一的工具执行器调度（`StreamingToolExecutor.canExecuteTool`：只要队列里出现非并发安全工具就断批，`services/tools/StreamingToolExecutor.ts:129`）。

### 3.2 深度控制 —— 三种截然不同的哲学

**agentrs：结构性禁止**

`build_tool_registry`（`spawner.rs:229`）硬编码 6 个工具，不含 `Spawn`。深度**恒为 1**，不可配置。

- 优点：绝对安全，无需运行时检查
- 代价：这个"顺带"的副作用同时砍掉了 MCP、`Skill`、`WebFetch`/`WebSearch`、`ViewImage`、`ToolSearch`；新增工具时没有任何机制提醒开发者同步这份白名单

**claude-code：不禁止，按需防护**

普通 subagent 是否持有 `Agent` 工具由 agent 定义的 `tools` 白名单决定（`resolveAgentTools`）。coordinator 模式下 worker 可再派生。只有 fork 路径需要特殊防护：因为 fork 子为了 prompt cache 一致性必须保留父的**精确工具池**（含 `Agent`），所以改用运行时嗅探——`isInForkChild()` 在会话历史里找 `<fork-boilerplate>` 标签，命中即拒绝再 fork（`forkSubagent.ts:73`）。

**opencode：显式配置 + 实际计数**

```ts
let current = parent, depth = 0
while (current.parentID) { depth++; current = yield* sessions.get(current.parentID) }
if (depth >= (cfg.subagent_depth ?? 1))
  return yield* Effect.fail(new Error(`Subagent depth limit reached (${...}). Increase "subagent_depth" ...`))
```

沿 `parentID` 链真实计数，超限时给出**可操作的错误文本**（告诉模型改哪个配置项）。同时默认 deny 子的 `task` 权限（`subagent-permissions.ts`），除非该 agent 自己声明。

> **评价**：opencode 这套最干净——把"深度"做成一等配置项而不是架构副作用，用权限规则给默认值，错误消息还教用户怎么解。agentrs 的做法安全但不可调，claude-code 的做法灵活但防护是补丁式的。

### 3.3 执行形态

**agentrs**：仅一种——同步阻塞父的工具调用直到子返回。

**claude-code**：三种，且 fork 是重头戏。

| 形态 | 触发 | 特征 |
|---|---|---|
| sync | 默认 | 共享父 `abortController`（`runAgent.ts:522`），父 ESC 立即中断子 |
| async | `run_in_background: true` | 新建独立 `AbortController`，注册进任务表，结果以 `<task-notification>` 回注 |
| **fork** | 省略 `subagent_type`（feature gate） | 子继承父的**完整对话上下文**与**已渲染的 system prompt 字节** |

fork 的工程细节值得单独记：

- `useExactTools` ⇒ 子拿父的精确工具池，不做过滤（`runAgent.ts:500`）
- `override.systemPrompt` 传父**已渲染的字节**而非重新调 `getSystemPrompt()`——注释明确写了理由："重新调用会 GrowthBook 冷→热分叉、打爆 prompt cache"（`forkSubagent.ts:52-58`）
- `buildForkedMessages` 给父 assistant 消息里的**所有** `tool_use` 块填**同一个占位 result**（`FORK_PLACEHOLDER_RESULT = 'Fork started — processing in background'`），只让最后一个 directive 文本块因子而异
- `thinkingConfig`：fork 子继承父配置以对齐请求前缀；普通 subagent 则**强制 `disabled`** 以控输出成本（`runAgent.ts:680`）

净效果：多个 fork 子的 API 请求前缀 **byte-identical**，共享同一份 prompt cache。这是三者中唯一为缓存命中做过系统性设计的实现。

**opencode**：foreground / background 两种，且**可互转**——这是它调度层最有意思的地方。

- `background.start({ id, run, onPromote })`：启动
- `background.promote(id)`：前台任务**转**后台
- `background.extend({ id, run })`：若该 id 的后台任务仍在跑，则向它**追加**新上下文而非新建（`task.ts` 中 `if (yield* background.extend(...))` 分支）
- `Effect.raceFirst(background.wait(...), background.waitForPromotion(...))`：前台等待期间若被 promote，立刻返回"已转后台"而非继续阻塞

> 这套"前台等待 / 后台运行"可动态切换的模型，agentrs 与 claude-code 都没有对应物（claude-code 的 `backgroundAgentTask` 接近，但没有 `extend` 这种向运行中任务追加上下文的能力）。

### 3.4 隔离

| | 机制 |
|---|---|
| **agentrs** | 无。全部子共享父 `cwd`，均持 Write/Edit 且 `auto_approve = true` |
| **claude-code** | `isolation: 'worktree' \| 'remote'`——独立 git worktree（未改动则自动清理）或远程云环境；另有 `cwd` 参数（KAIROS gate） |
| **opencode** | 有 `Worktree` 服务（`worktree/index.ts`，含 create/remove/reset/list 与完整错误类型），但属**会话级**设施，不是 task 工具的按子自动隔离 |

---

## 四、通信

### 4.1 下行（父 → 子）

| | 可传递的内容 |
|---|---|
| **agentrs** | 仅 `SubAgentConfig.prompt` 字符串；`Config`（含 system prompt）、`ToolPolicy`、`cwd`、`runtime_env` 隐式继承 |
| **claude-code** | `prompt` + `description` + `subagent_type` + `model` + `mode`（权限模式）+ `isolation` + `name` + `team_name`；子按 agent 定义**重建** system prompt（`getAgentSystemPrompt(agentDefinition, ..., resolvedTools)`），可预载 skills 为初始消息，可挂**自己的 MCP server**（additive，`initializeAgentMcpServers`），触发 `SubagentStart` hook 注入额外上下文 |
| **opencode** | `prompt`（支持 `@file` 等 part 模板，经 `ops.resolvePromptParts` 解析）+ `subagent_type` + `task_id` + `background`；子会话携带自己的 agent 定义（prompt / model / variant / permission / steps / temperature / topP） |

**关键差异——system prompt 的处理**：

- agentrs：**沿用父的原文**（除非 `sub_config.system_prompt` 显式覆盖）。实测确认子收到的 `request.system` 为父提示词全文，而工具表只有 7 个本地工具 ⇒ 提示词宣传的 Spawn/Skill/WebSearch 全部不存在
- claude-code：**按 agent 定义 + 实际 `resolvedTools` 重建**；只有 fork 路径才复用父的字节，且那是为缓存刻意为之
- opencode：子会话有自己的 agent，提示词自然独立

> agentrs 的做法看似与 claude-code 的 fork 相似，方向却相反：既没拿到缓存收益（工具表已不同，前缀本就不一致），又制造了提示词与工具集的矛盾。

### 4.2 上行（子 → 父）

**agentrs**：Markdown 拼接字符串，一次性。

```
## <name> [OK|ERROR]
<text>
[turns: N | tokens: I in / O out]
```

以 `\n\n---\n\n` 连接（`spawn_tool.rs:104`），随后在 `orchestration::execute_single` 经 `compact_output` → `max_result_size`(50 KB) ∧ `tool_output_max_bytes` 做头尾保留式截断。不可解析。

**claude-code**：`<task-notification>` XML，作为 **user-role 消息**注入父会话：

```xml
<task-notification>
<task-id>{agentId}</task-id>
<status>completed|failed|killed</status>
<summary>{human-readable status summary}</summary>
<result>{agent's final text response}</result>
<usage>
  <total_tokens>N</total_tokens>
  <tool_uses>N</tool_uses>
  <duration_ms>N</duration_ms>
</usage>
</task-notification>
```

coordinator 提示词专门教模型识别："Worker results arrive as **user-role messages** containing `<task-notification>` XML. They look like user messages but are not."（`coordinator/coordinatorMode.ts:144`）

一个体现工程成熟度的细节：对 `Explore` / `Plan` 这类**一次性 agent**，省略 agentId/SendMessage/usage 尾巴，注释写明理由——"~135 chars × 34M Explore runs/week"（`tools/AgentTool/constants.ts:6`）。按量算过账的取舍。

**opencode**：`<task id state>` XML（`renderOutput`）：

```xml
<task id="{sessionID}" state="running|completed|error">
<summary>...</summary>
<task_result>...</task_result>
</task>
```

后台任务完成时，以 `synthetic: true` 的 user part 注入父会话（`inject()`），并在文本里反复叮嘱模型"DO NOT sleep, poll for progress, ask the task for status, or duplicate this task's work"。

### 4.3 运行中通信 —— agentrs 完全缺失的一层

**claude-code：完整的 agent 消息总线**

`SendMessage` 工具（`tools/SendMessageTool/SendMessageTool.ts`，917 行）：

| `to` 取值 | 含义 |
|---|---|
| `"researcher"` | teammate 名 |
| `agentId` | 具体 agent 实例（含已完成的，触发 resume） |
| `"*"` | 广播（提示词标注"expensive (linear in team size)"） |
| `"uds:/path/to.sock"` | 同机另一个 Claude 会话（Unix domain socket） |
| `"bridge:session_01..."` | 跨机 Remote Control peer |

投递路径：`queuePendingMessage(taskId, msg)` → 子在下一个工具轮 `drainPendingMessages`；且子**只 drain 寄给自己的通知**（`query.ts:1575`：`cmd.mode === 'task-notification' && cmd.agentId === currentAgentId`）。tmux teammate 另走文件 mailbox（`utils/teammateMailbox.ts`）。

除自由文本外还有**结构化协议消息**：`shutdown_request` / `shutdown_response` / `plan_approval_response`，带 `request_id` 关联。

提示词里的一句话点破了整个模型的前提：
> **"Your plain text output is NOT visible to other agents — to communicate, you MUST call this tool."**

**opencode**：无 agent-to-agent 通道。最接近的是 `background.extend()`——父向运行中的后台子追加上下文，单向，无兄弟互通。

**agentrs**：零。

---

## 五、协作与共享状态

| 共享面 | agentrs | claude-code | opencode |
|---|---|---|---|
| **任务/计划** | graph 模式共享 `<workspace>/.agentrs/tasks/tasks.json`；list 模式子各自私有内存 store | `TaskCreate` / `TaskGet` / `TaskList` / `TaskUpdate` 共享任务表；`TodoWrite` 私有 | todo 按 `sessionID` 存；子**默认被 deny `todowrite`**（`subagent-permissions.ts`） |
| **跨运行记忆** | 无（子 `session.enabled = false`） | **agent-memory**：按 agent type 的持久记忆目录，三个 scope——`user`(`~/.claude/agent-memory/`)、`project`(`.claude/agent-memory/`)、`local`(`.claude/agent-memory-local/`，可经 `CLAUDE_CODE_REMOTE_MEMORY_DIR` 挂远端)；另有 `agentMemorySnapshot.ts` | 无等价物 |
| **共享工作区** | 同 cwd，无协调 | coordinator 的 **scratchpad 目录**："Workers can read and write here without permission prompts. Use this for durable cross-worker knowledge"；worktree 隔离 | 会话级 worktree |
| **编队** | 无 | `TeamCreate` / `TeamDelete` + team lead 角色（`TEAM_LEAD_NAME`）+ tmux 面板（`TeammateSpawnedOutput` 含 `tmux_session_name` / `tmux_window_name` / `tmux_pane_id`）+ 广播 | 无 |

### 5.1 agentrs 的 graph 任务图值得单独评价

`agentrs-tools/src/task/store.rs` 的实现质量高于两个对标：

- `with_state()` 对状态文件取 **OS 独占锁**，贯穿整个读-改-写，因此"认领任务"是原子的；注释还说明了为什么不用进程内 mutex（"a second agentrs sharing the workspace cannot interleave a half-written graph"）
- `next_id` **持久化**而非从现存任务推导——注释点明理由：否则删除任务后 id 复用会"silently re-point every dependency that still names it"
- `add_dependency` 维护双向边，`dependency_cycle` 用 BFS 找环并**渲染出环路径**供模型阅读
- `update` 的状态检查针对**改后图**，因此"同一次调用里先解依赖再启动任务"是合法的

claude-code 的 `TaskCreateTool` 系列与 opencode 的 `Todo` 都没有这种强一致保证。

**但它的价值被浪费了**：唯一的使用方式是"父规划 → 子认领 → 父在子返回后读取"。没有主动汇报、没有中途指挥、没有阻塞等待某个任务完成。它具备支撑真正协作的一致性，却只被当作一块共享白板用。

### 5.2 隔离哲学的差异

三者对"子智能体并发写同一个 workspace"的态度：

- **agentrs**：不处理。所有子共享 cwd，均可 Write/Edit，免审批
- **claude-code**：提供 worktree/remote 隔离作为**可选参数**，由模型按任务性质决定；同时用 scratchpad 目录承载"故意共享"的部分
- **opencode**：不在 task 层处理，但会话级 worktree 存在；更依赖"子默认权限更窄"来降低风险

---

## 六、生命周期与取消

| | 取消机制 |
|---|---|
| **agentrs** | ❌ **无任何路径**。`SpawnTool` 未覆写 `execute_cancellable`（走 `Tool` 默认实现丢弃 token），`spawn_parallel` 丢弃 `JoinHandle` 无法 abort。实测：传入已 cancel 的 token，子照常跑完 |
| **claude-code** | sync 子共享父 `abortController`（父 ESC 立即断）；async 子独立 controller + `killAsyncAgent(taskId)` + `TaskStop` 工具 + `killAllRunningAgentTasks`；`SubagentStop` hook |
| **opencode** | `ctx.abort` 监听 → `ops.cancel(childSessionID)`；`Effect.acquireUseRelease` 在 `Exit.hasInterrupts` 时同时 `cancel` 会话与 `background.cancel`；父会话删除时 `remove()` **递归删子会话并 `cancelBackgroundJobs`**（`session/session.ts:608-624`） |

opencode 的级联删除值得展开：

```ts
if (hasInstance) yield* cancelBackgroundJobs(background, sessionID)
const kids = yield* children(sessionID)
for (const child of kids) yield* remove(child.id)
```

因为子就是会话，"删除父会话"自然意味着"递归清理整棵子树 + 取消其后台作业"。这又是"选对本体"带来的免费收益。

agentrs 是三者中唯一**没有任何取消路径**的实现，且这是当前唯一会造成**实际损害**的缺陷（用户 Ctrl-C 后子继续写文件、继续烧 token）。

---

## 七、成本与可观测性

| | token 归集 | 用户可见性 |
|---|---|---|
| **agentrs** | ❌ `SubAgentResult.usage` **只拼进文本给模型看**（`spawn_tool.rs:110`），不进 `total_usage` 会话累计用量 | ❌ `NullSink` 丢弃全部子输出，无进度、无 token、无活动描述 |
| **claude-code** | ✅ `<usage>` 随 `<task-notification>` 回传；`costHook.ts` / `cost-tracker.ts` 全局归集 | ✅ `ProgressTracker` 实时统计 token / 工具调用次数 / 当前活动描述（`createActivityDescriptionResolver`），面板可见；`TaskOutput` 工具可查进度文件 |
| **opencode** | ✅ 子会话消息走标准会话存储，token **天然计入** | ✅ UI 中即一条子会话 |

agentrs 的后果是：会话累计用量与预算判断基于**残缺的数据**——5 个子烧掉的 token 对父完全不可见。

> 口径澄清：子的 token **不占父的上下文窗口**，因此不应计入 `context_usage`；父的上下文只因子的返回文本增长，那部分已由 `record_tool_context_estimate` 正确计入。缺的是**成本口径**（`total_usage`），不是上下文口径——自动压缩的触发依据当前是正确的。

---

## 八、权限与安全边界

### 8.1 子权限的推导规则

**agentrs**：`effective_child_tool_policy`（`spawner.rs:204`）

```rust
if allowed_tools.is_empty() { return parent.clone(); }
ToolPolicy::allow_only(allowed_tools.iter().filter(|t| parent.allows(t)).cloned())
```

对父策略**取交集** ⇒ fork 的 `allowed_tools` 只能收窄不能扩权。有专门的单测（`a_child_cannot_recover_a_network_tool_its_parent_was_denied`）。**这条规则设计正确，是 agentrs 权限模型的亮点。**

**opencode**：`deriveSubagentSessionPermission`（`agent/subagent-permissions.ts`）

```ts
[
  ...parentSessionPermission.filter(r => r.permission === "external_directory" || r.action === "deny"),
  ...(canTodo ? [] : [{ permission: "todowrite", pattern: "*", action: "deny" }]),
  ...(canTask ? [] : [{ permission: "task",      pattern: "*", action: "deny" }]),
]
```

语义与 agentrs 不同但同样保守：**只继承父的 deny 规则与 external_directory 规则**，不继承父的 allow（注释说明"Parent agent restrictions only govern that agent; the subagent's own permissions determine its capabilities"），再默认 deny `todowrite` 与 `task`。另外 task 工具还会追加 `cfg.experimental.primary_tools` 的 deny。

**claude-code**：`AgentTool.isReadOnly() = true`，注释写着"delegates permission checks to its underlying tools"——即 Agent 工具本身不做权限判断，由子执行的每个具体工具各自检查。子的能力由 agent 定义的 `tools` 白名单 + `permissionMode`（其中 fork 用 `'bubble'` 把权限提示冒泡到父终端）决定。

### 8.2 审批模式

| | 子的审批 |
|---|---|
| **agentrs** | `config.tools.auto_approve = true` 强制免审批 —— 子的写操作无人把关 |
| **claude-code** | 依 `permissionMode`；fork 用 `'bubble'` 提示到父终端；auto 模式下走分类器 |
| **opencode** | `ctx.ask({ permission, patterns, always })`，权限请求作为事件发布（按 sessionID），客户端可渲染子会话的审批请求 |

agentrs 的 `auto_approve = true` 与"子共享父 cwd 且持有 Write/Edit"叠加，是当前安全面上最宽的一处。

---

## 九、逐维度总表

| 维度 | agentrs | claude-code | opencode |
|---|---|---|---|
| **子的本体** | 临时 `AgentEngine` | 任务表实体（有 agentId） | 子会话（parentID） |
| **持久化** | ❌ | ✅ transcript | ✅ SQLite |
| **可寻址** | ❌ | ✅ agentId | ✅ sessionID |
| **续跑** | ❌ | ✅ SendMessage / resume | ✅ task_id |
| **并发上限** | 单次 5（硬编码） | 全局 10（env 可调） | 无显式上限 |
| **深度控制** | 结构性禁止，恒 1 | 白名单 + fork 嗅探 | `subagent_depth` 配置 + 链计数 |
| **执行形态** | 同步 | sync / async / fork | foreground / background（可互转） |
| **prompt cache 优化** | ❌ | ✅ byte-identical 前缀（fork） | ❌ |
| **隔离** | ❌ | worktree / remote | 会话级 worktree |
| **下行内容** | prompt | prompt + 8 个参数 + MCP + skills + hook | prompt parts + agent 定义 |
| **上行格式** | Markdown 拼接 | `<task-notification>` XML + usage | `<task>` XML |
| **运行中通信** | ❌ | ✅ SendMessage（名/id/广播/uds/bridge）+ 结构化协议 | ⚠️ 仅 `background.extend()` |
| **共享计划** | graph 任务图（**一致性最强**） | Task 系列工具 | 子默认无 todowrite |
| **跨运行记忆** | ❌ | agent-memory（3 scope） | ❌ |
| **编队** | ❌ | Team + tmux + mailbox | ❌ |
| **取消** | ❌ 无 | ✅ 分形态 + TaskStop | ✅ 级联 + 递归删除 |
| **token 入账** | ❌ | ✅ | ✅ |
| **用户可见** | ❌ NullSink | ✅ 面板 + 进度 | ✅ 子会话 |
| **子权限** | 父策略交集（正确） | 白名单 + permissionMode | 继承 deny + 默认收窄 |
| **子审批** | 强制 auto_approve | permissionMode | ask 事件 |

---

## 十、三种架构的适用性判断

### 10.1 各自的设计意图

**claude-code** 是**多 agent 协作平台**。它的复杂度（团队编制、tmux 面板、文件 mailbox、跨机 peer、按 agent type 的持久记忆、coordinator 模式）都指向同一个场景：多个长期存活的 agent 在同一个工程上分工协作，人类作为协调者旁观。fork + prompt cache 对齐这类优化说明它在意**大规模并发下的成本**（注释里那个 "34M Explore runs/week" 不是随口一说）。

**opencode** 是**会话树**。它没有 agent 间通信、没有团队，但把"子 = 子会话"这个抽象做到了自洽：续跑、取消级联、UI 可见、token 入账全部是同一个决策的免费产物。`background.promote/extend` 那套前后台互转是它在调度上的独到之处。工程上最简洁。

**agentrs** 目前是**并行 map 函数**。适合"把 N 个独立只读调研任务并行掉"这一种场景，超出即失效。

### 10.2 agentrs 应当对标谁

**结论：对标 opencode，不对标 claude-code。**

理由：

1. **改造面最小**：opencode 用一个既有概念（session + parentID）覆盖了续跑、取消级联、UI 可见、token 入账四件事；agentrs 已有 `SessionManager`，而 `session.enabled = false` 是主动关掉的——打开它即获得地基
2. **定位匹配**：agentrs 是 CLI + JSON stream 协议的单机工具，claude-code 的 tmux/team/跨机 peer 与之不符
3. **依赖顺序**：agent-to-agent 通信的前提是子可寻址且长期存活；在"子会话"落地之前谈 `SendMessage` 是空中楼阁

### 10.3 值得从 claude-code 借鉴的**局部**设计

即使不采用其整体架构，以下四点是可独立移植的：

1. **`<task-notification>` 的字段设计**——status / summary / result / usage 四段式，比 agentrs 当前的 Markdown 拼接可解析得多
2. **按量做取舍的意识**——对一次性 agent 省略 trailer 那种"算过账的省"
3. **`isolation: worktree`**——按任务性质可选的隔离，比"全都隔离"或"全都不隔离"都合理
4. **子提示词按实际工具集重建**——这不是 claude-code 的特色，而是基本正确性；agentrs 当前的做法是明确的 bug

### 10.4 明确不建议引入的

| 不引入 | 理由 |
|---|---|
| 团队 / tmux 面板 / 跨机 peer | 产品形态不符 |
| agent-memory（按 agent type 分片的持久记忆） | `agentrs-memory` 已有跨会话记忆，再分片收益不明 |
| `SendMessage` 式消息总线 | 前提未就绪；子会话落地后再评估 |
| 重写 graph 任务图 | 它的一致性保证优于两个对标实现，是资产不是负债 |

---

## 附：关键代码位置索引

**agentrs**

| 主题 | 位置 |
|---|---|
| 派生实现 | `crates/agentrs-agent/src/spawner.rs:68`(spawn_one) `:116`(spawn_parallel) `:154`(spawn_fork) |
| 子工具表 | `crates/agentrs-agent/src/spawner.rs:229` |
| 权限交集 | `crates/agentrs-agent/src/spawner.rs:204` |
| Spawn 工具 | `crates/agentrs-agent/src/spawn_tool.rs` |
| Spawner 契约 | `crates/agentrs-types/src/spawner.rs` |
| fork skill | `crates/agentrs-skills/src/executor.rs:78` |
| 任务图 | `crates/agentrs-tools/src/task/store.rs` |
| 装配 | `crates/agentrs-agent/src/bootstrap.rs:343-371` |

**claude-code**

| 主题 | 位置 |
|---|---|
| Agent 工具 | `src/tools/AgentTool/AgentTool.tsx`（1397 行） |
| 子执行 | `src/tools/AgentTool/runAgent.ts`（973 行） |
| fork | `src/tools/AgentTool/forkSubagent.ts` |
| 续跑 | `src/tools/AgentTool/resumeAgent.ts` |
| agent 记忆 | `src/tools/AgentTool/agentMemory.ts` / `agentMemorySnapshot.ts` |
| 任务注册表 | `src/tasks/LocalAgentTask/LocalAgentTask.tsx`（682 行） |
| 消息 | `src/tools/SendMessageTool/SendMessageTool.ts`（917 行） |
| 并发调度 | `src/services/tools/StreamingToolExecutor.ts` / `toolOrchestration.ts:10` |
| coordinator | `src/coordinator/coordinatorMode.ts` |

**opencode**

| 主题 | 位置 |
|---|---|
| task 工具 | `packages/opencode/src/tool/task.ts`（360 行） |
| 子权限 | `packages/opencode/src/agent/subagent-permissions.ts` |
| agent 定义 | `packages/opencode/src/agent/agent.ts`（453 行） |
| 后台作业 | `packages/opencode/src/background/job.ts` |
| 会话与级联 | `packages/opencode/src/session/session.ts:608`(remove) `:598`(children) |
| worktree | `packages/opencode/src/worktree/index.ts` |

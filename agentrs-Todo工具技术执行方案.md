# agentrs Todo 工具 —— 横向深度分析与技术执行方案

> 范围：`opensource/claude-code-main`、`opensource/pi`、`opensource/opencode`、
> `opensource/deepseek-harness` 四家 Todo 工具的技术设计横向拆解，以及在
> `agentrs/crates/agentrs-tools` 中引入 Todo 工具的落地方案。
> 日期：2026-09-04

---

## 0. 结论摘要

四家实现在**模型面语义**上高度收敛，在**状态归属**上分歧最大：

| 维度 | 收敛结论 |
|------|----------|
| 写入语义 | **整表替换（whole-list replacement）**，无增量补丁。4 家中 3 家如此，pi 的 action 式是 demo 特例 |
| 状态机 | `pending` / `in_progress` / `completed` 三态，opencode 多一个 `cancelled` |
| 并发纪律 | 默认「同一时刻至多一个 `in_progress`」，deepseek 将其做成**可配置策略**并在服务端强校验 |
| 权限 | 零审批（claude-code 直接 `behavior: 'allow'`；opencode 走 permission 但默认 `always: ["*"]`） |
| 提示词 | 描述极长（claude-code 184 行 prompt），核心是「何时用/何时不用 + 完成判据」 |
| 复活机制 | claude-code 有 **turn 计数驱动的 system-reminder 回灌**，是唯一让 Todo 真正被持续使用的机制 |

**对 agentrs 的选型建议**：
- 工具名 `TodoWrite`，整表替换，三态，`content` + `activeForm`。
- 校验借鉴 deepseek-harness（trim / 非空 / 去重 / 单活跃校验 + 可配置并行）。
- 状态**不新增持久化字段**：内存 `TodoStore` + **从会话历史重放重建**（借鉴 pi/deepseek 的 event-fold 思路）。这一点对 agentrs 尤其关键，因为 agentrs 已有 `SessionManager::fork_from` / `ForkBoundary`，重放式重建能让**分叉自动获得正确的 Todo 快照**，零迁移成本。
- 提醒机制借鉴 claude-code，但落点选在 `AgentEngine::build_request()` 的**临时消息注入**（不写回 `self.messages`），避免污染会话与缓存前缀。

---

## 1. 四家横向深度分析

### 1.1 claude-code-main（工业级完全体）

**文件**：`src/tools/TodoWriteTool/{TodoWriteTool.ts, prompt.ts, constants.ts}`、
`src/utils/todo/types.ts`、`src/utils/attachments.ts`、`src/utils/messages.ts`

**数据模型**（`utils/todo/types.ts`）：

```ts
TodoItem = {
  content: string     // 祈使句 "Run tests"
  status: 'pending' | 'in_progress' | 'completed'
  activeForm: string  // 现在进行时 "Running tests" —— 驱动 spinner 文案
}
```

`activeForm` 是 claude-code 独有的设计：**同一任务的两种时态**，`content` 用于清单展示，
`activeForm` 用于「正在做什么」的实时状态行。这是 UI 体验上的关键差异点。

**状态归属**：`AppState.todos[todoKey]`，`todoKey = context.agentId ?? getSessionId()`。
即 **主 agent 与每个 subagent 各持一份独立清单**，互不干扰。

**执行语义**（`TodoWriteTool.ts`）：
- 整表替换；
- **全部完成即清空**：`const newTodos = allDone ? [] : todos` —— 收尾时自动收纳，避免陈旧清单留在上下文里；
  注意返回给模型的 `data.newTodos` 仍是原 `todos`（展示用），写入 state 的是 `newTodos`（可能为空）；
- `checkPermissions` 直接 allow；`shouldDefer: true`（schema 延迟下发）；`renderToolUseMessage() → null`（UI 不显示调用行）；
- 固定文本回执：`"Todos have been modified successfully. Ensure that you continue to use the todo list..."` ——
  **回执不回显清单内容**，节省 token，靠 UI 渲染；
- **verification nudge**：主线程 agent 一次性关闭 3+ 条且没有任何一条包含 "verif" 时，
  在 tool_result 追加一段强制去调 verification subagent 的提示。这是「结构性提醒挂在 loop 退出时刻」的典型手法。

**提醒回灌（最有价值的部分）**：`utils/attachments.ts`

```ts
TODO_REMINDER_CONFIG = { TURNS_SINCE_WRITE: 10, TURNS_BETWEEN_REMINDERS: 10 }
```

- `getTodoReminderTurnCounts()` 反向扫描消息，统计「距上次 TodoWrite 的 assistant turn 数」与
  「距上次提醒的 turn 数」；
- 两者都 ≥10 时产出 `todo_reminder` attachment；
- `utils/messages.ts:3663` 渲染成 `<system-reminder>` 包裹的 meta user message，
  文案要点：*"This is just a gentle reminder - ignore if not applicable. Make sure that you NEVER mention this reminder to the user"*，并附上当前清单快照；
- 前置门控：工具不在 toolkit 中 → 不提醒；`SendUserMessage`（Brief）在场 → 不提醒（避免与主沟通渠道抢戏）。

**Todo v2（TaskUpdateTool）**：claude-code 已在演进第二代——从「一次替换整表」变成
**逐条 CRUD 的任务图**（`utils/tasks.ts`）：

```ts
Task = { id, subject, description, activeForm?, owner?, status,
         blocks: string[], blockedBy: string[], metadata? }
```

落盘为文件 + 高水位 ID 文件 + 文件锁（支撑 swarm 多 agent 并发认领），配套
`TaskCreate/Get/List/Update/Stop/Output` 六个工具，`isTodoV2Enabled()` 做灰度切换。
**这条演进路线值得 agentrs 记在设计余量里，但 v1 不做**——依赖图 + 文件锁的复杂度只有多 agent 抢占场景才回本。

### 1.2 opencode（服务化 / 持久化 / 事件驱动）

**文件**：`packages/opencode/src/tool/todo.ts`、`src/tool/todowrite.txt`、`src/session/todo.ts`

- 工具体极薄（46 行）：校验 → `ctx.ask({ permission: 'todowrite', patterns: ['*'], always: ['*'] })` → `todo.update()`；
- **状态落 SQLite**：`TodoTable(session_id, content, status, priority, position)`，
  `update` 在一个事务里 `delete where session_id` + 批量 `insert`——**整表替换直接落到存储层语义**，
  `position` 列保序；
- 更新后 `events.publish(Event.Updated)`，UI 端订阅渲染（`session-ui/components/message-part.tsx`）；
- 输出 `title: "${未完成数} todos"`，`output: JSON.stringify(todos)` —— 与 claude-code 相反，**回执回灌全量清单**；
- 数据模型多了 `priority` 和 `cancelled` 状态；
- 提示词（`todowrite.txt`）是 claude-code prompt 的**精简重写版**（约 1/4 长度），保留了
  「3+ steps」「exactly ONE in_progress」「完成必须含验证」这三条硬规则，并额外强调
  *"Preserve user-provided commands verbatim"*。

**可借鉴点**：整表替换 → 存储层事务的映射非常干净；`position` 显式保序；权限位虽然形同虚设，但把
Todo 纳入统一权限框架而非开特例，架构上更一致。

### 1.3 pi（扩展示范 / 历史重放派）

**文件**：`packages/coding-agent/examples/extensions/todo.ts`

pi 核心没有内置 Todo，这是一个**扩展示例**，但它的设计取向恰恰最贴合 agentrs 的痛点：

- **action 式接口**（`list` / `add` / `toggle` / `clear`），与其他三家的整表替换相反——
  对模型更啰嗦、更易产生状态漂移，**不建议采纳**；
- 但状态管理思路是亮点：状态**不外存**，而是写在每次 tool result 的 `details` 里，
  `reconstructState(ctx)` 在 `session_start` / `session_tree` 事件时**顺序重放分支上的所有
  todo tool result**，取最后一次快照。文件头注释点明了理由：

  > *"State is stored in tool result details (not external files), which allows proper branching -
  > when you branch, the todo state is automatically correct for that point in history."*

- 同时注册 `/todos` 斜杠命令 + `renderCall` / `renderResult` 双渲染钩子（折叠时只显示 5 条 + "... N more"）。

**可借鉴点**：**分叉正确性来自「状态是历史的纯函数」**。agentrs 有 fork 能力，这条是刚需。

### 1.4 deepseek-harness（工程约束最严）

**文件**：`packages/todo/tool-todo/src/index.ts`（223 行，注释密度极高）

- 工具名 `todo_write`，整表替换，三态；item schema `additionalProperties: false`——
  注释解释得很到位：*"the logged snapshot must equal what the model believes it wrote,
  so a nested/extended item shape fails loud at the schema boundary instead of silently flattening"*；
- **`allowParallelInProgress` 是必填配置项**，不是可选开关。它同时驱动两件事：
  1. **描述文案分支**：`DESCRIPTION_HEAD + (PARALLEL | SINGLE) + DESCRIPTION_TAIL`——
     策略变了，给模型的话术也跟着变，而不是留一段永远矛盾的静态描述；
  2. **运行时校验**：`allowParallel=false` 且 `active > 1` 时直接 `throw`。
- `toTodoList()` 的校验清单：`trim` → 非空 → **按 content 去重** → 统计 active → 单活跃校验；
- **projection 折叠**：注册 `todos` session projection，
  `apply: (state, ev) => ev.type === 'todo/write' ? ev.data.todos : ev.type === 'turn/start' ? null : state`。
  即：Todo 是「本轮的 standing plan」，**新一轮 user turn 开始即清空**，`turn/end` 保留完成态清单可见；
- 结构化输出 + `render`：回执是
  `"Updated todo list: N pending, M in progress, K completed."` ——**只回计数，不回内容**；
- `execute` 中拒绝无归属 agent 的调用：`throw new Error('todo_write requires an owning agent session')`，
  注释明确「宁可失败也不静默 no-op」；
- 配套 `guard/repeat-tool-reminder` 插件（阈值 `[3,5,8]`），且其默认配置里
  **`exclude: [todo_write]`** —— 重复调用 Todo 是正常行为，不应触发循环告警。这是个容易漏掉的坑。

### 1.5 对比矩阵

| 维度 | claude-code | opencode | pi | deepseek-harness |
|------|-------------|----------|-----|------------------|
| 工具名 | `TodoWrite` | `todowrite` | `todo` | `todo_write` |
| 接口形态 | 整表替换 | 整表替换 | action 式 | 整表替换 |
| 字段 | content / status / **activeForm** | content / status / **priority** | id / text / done | content / status |
| 状态集 | 3 态 | 4 态（+cancelled） | bool done | 3 态 |
| 单活跃约束 | prompt 强调，**不校验** | prompt 强调，不校验 | 无 | **配置化 + 强校验** |
| 去重/trim | 无 | 无 | 无 | **有** |
| 状态存放 | 内存 AppState（按 agentId 分片） | SQLite（按 session） | **历史重放** | **session projection 折叠** |
| 生命周期 | 全完成即清空 | 持久 | 分支内持久 | **新 turn 开始即清空** |
| 回执内容 | 固定文案（不回清单） | 全量 JSON | 文本清单 | **计数摘要** |
| 权限 | 直接 allow | permission 框架（默认放行） | 无 | 无 |
| 提醒机制 | **turn 计数回灌 system-reminder** | 无 | 无 | 无（但排除在循环告警外） |
| UI | 独立面板 + spinner activeForm | 事件订阅渲染 | renderCall/renderResult + `/todos` | presentCall card |
| 演进 | → TaskTool 任务图 + 文件锁 | — | — | — |

---

## 2. agentrs 现状与约束

已核对的关键事实：

| 事项 | 现状 | 对方案的影响 |
|------|------|--------------|
| `Tool` trait | `crates/agentrs-tools/src/tool.rs`，含 `name/description/input_schema/is_concurrency_safe/execute/category/is_deferred/describe/max_result_size` | Todo 工具照此实现即可，无需扩 trait |
| 共享状态先例 | `FileCache` 经 `Arc` 注入 `ReadTool/WriteTool/EditTool`（`bootstrap.rs:227-229`）；plan 模式用 `Arc<AtomicBool>`（`bootstrap.rs:404-409`） | `TodoStore` 沿用同一模式，**不需要新 crate** |
| 依赖方向 | `agentrs-tools` 在 `agentrs-agent` 之下，不可反向依赖 | `TodoStore` 必须放在 `agentrs-tools`（或更低），engine 持 `Arc` |
| 提示注入点 | `AgentEngine::build_request()`（`engine.rs:670`）内部 `let mut messages = self.messages.clone()`，已有 `kind.control_prompt()` 追加临时消息的先例 | **临时 system-reminder 注入的理想落点**，不污染 `self.messages` / 会话文件 |
| 工具结果加料先例 | `append_tool_loop_warning()`（`engine.rs:1704`）向 tool_result 追加 guidance | verification/收尾 nudge 可复用同款手法 |
| 会话与分叉 | `Session { context_state, messages }` + `SessionManager::fork_from(ForkBoundary)` | 重放式重建可白嫖分叉正确性；若存 `context_state` 则 fork 时需额外裁剪 |
| 协议事件 | `ProtocolEvent::ToolResult { .., metadata: Option<Value> }` 已有 metadata 字段，但 `OutputSink::emit_tool_result` 签名不带 metadata | 结构化 UI 推送需改 sink 签名 → 排到 Phase 2 |
| plan 模式 | 只放行 `ToolCategory::Info` 工具 | Todo 标 `Info` 即可在 plan 模式下可用 |
| 循环护栏 | `TurnGuards` 的 cycle / exact-failure 追踪 | 需把 `TodoWrite` 排除出重复调用统计（借鉴 deepseek） |
| 代码规范 | 测试必须外置 `*_test.rs` + `#[path]` 挂载；禁止在 `lib.rs`/`mod.rs` 写业务逻辑；可见性最小化 | 目录与文件划分照此设计 |

---

## 3. 目标设计

### 3.1 模块划分

```
crates/agentrs-tools/src/todo/
├── mod.rs          # 仅可见性声明与再导出
├── item.rs         # TodoItem / TodoStatus / 校验与规范化 (to_todo_list)
├── item_test.rs
├── store.rs        # TodoStore：Arc<Mutex<HashMap<TodoKey, Vec<TodoItem>>>>
├── store_test.rs
├── tool.rs         # TodoWriteTool
├── tool_test.rs
└── prompt.rs       # 描述文案（含 parallel/single 分支拼装）

crates/agentrs-config/src/todo.rs        # TodoConfig（+ todo_test.rs）
crates/agentrs-agent/src/todo_reminder.rs # 提醒判定纯函数（+ todo_reminder_test.rs）
```

### 3.2 数据模型

```rust
// crates/agentrs-tools/src/todo/item.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus { Pending, InProgress, Completed }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// 祈使句，"Run the test suite"
    pub content: String,
    pub status: TodoStatus,
    /// 现在进行时，"Running the test suite"。缺省时 UI 回落到 content。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}
```

**取舍说明**：
- 采纳 claude-code 的 `activeForm`，但设为**可选**——required 字段会抬高小模型的调用失败率，
  而 agentrs 是多 provider（含本地模型）运行时。TUI 缺省回落到 `content`。
- 不采纳 opencode 的 `priority`：顺序即优先级，多一个字段只会增加模型决策负担。
- 不采纳 `cancelled` 态：v1 用「从列表中删除」表达取消（claude-code prompt 的原话是
  *"Remove tasks that are no longer relevant from the list entirely"*）。若后续 UI 需要展示取消痕迹再补。

### 3.3 工具契约

| 项 | 取值 | 理由 |
|----|------|------|
| `name()` | `"TodoWrite"` | 与 agentrs 既有 PascalCase 命名一致（Read/Glob/WebFetch/EnterPlanMode） |
| `category()` | `ToolCategory::Info` | 不触碰文件系统/网络；同时使其在 plan 模式可用 |
| `is_concurrency_safe()` | `false` | 整表替换是顺序敏感的；工具本身开销近零，串行代价可忽略 |
| `is_deferred()` | `false` | 必须常驻工具列表，否则模型不会主动想起它 |
| `max_result_size()` | 默认 | 回执是计数摘要，天然很小 |
| 审批 | 无（Info 类不触发审批） | 与四家一致 |

**入参 schema**：

```json
{
  "type": "object",
  "properties": {
    "todos": {
      "type": "array",
      "description": "The COMPLETE task list, replacing any previous list.",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "content":    { "type": "string" },
          "status":     { "type": "string", "enum": ["pending", "in_progress", "completed"] },
          "activeForm": { "type": "string" }
        },
        "required": ["content", "status"]
      }
    }
  },
  "required": ["todos"]
}
```

`additionalProperties: false` 直接采纳 deepseek 的理由：**落库快照必须等于模型自认为写下的内容**，
多余字段要在 schema 边界上大声失败，而不是被静默丢弃。

**校验管线**（`item.rs::normalize`，deepseek `toTodoList` 的 Rust 版）：

1. `content.trim()`，空串 → 错误 `invalid todo: content must be non-empty`；
2. 按 trim 后的 content 去重，重复 → 错误（附重复内容）；
3. `active_form` 同样 trim，空串归一为 `None`；
4. 统计 `in_progress` 数量；`!allow_parallel && active > 1` → 错误
   `at most one task may be in_progress (got N)`；
5. 通过后返回规范化 `Vec<TodoItem>`，顺序即模型给定顺序（保序，对应 opencode 的 `position`）。

校验失败返回 `ToolResult { is_error: true, .. }`，让模型自行纠正——这是它唯一能从中学习的信号。

**回执**（融合 deepseek 计数摘要 + claude-code 的持续使用提示）：

```
Updated todo list: 3 pending, 1 in progress, 2 completed.
Continue to use TodoWrite as you make progress. Proceed with the current task.
```

不回灌全量清单（省 token，且清单本身就在模型刚发出的 tool_use 入参里）。

**收尾行为**：采纳 deepseek 而非 claude-code。
- claude-code：全部 completed → 立即清空 state。
- deepseek：完成态清单保留到本轮结束，**下一个 user turn 开始时清空**。

agentrs 取 deepseek 语义——用户在轮末能看到「全绿的清单」这一完成信号，
清空发生在 `AgentEngine::run_inner()` 入口（新一轮 user 输入到达时）。

### 3.4 状态归属与重建

```rust
// crates/agentrs-tools/src/todo/store.rs
pub struct TodoStore {
    inner: Mutex<HashMap<String, Vec<TodoItem>>>,  // key = agent_id | session_id
}

impl TodoStore {
    pub fn replace(&self, key: &str, todos: Vec<TodoItem>);
    pub fn snapshot(&self, key: &str) -> Vec<TodoItem>;
    pub fn clear(&self, key: &str);
    /// 从会话历史重放重建（resume / fork 后调用）
    pub fn rehydrate_from_history(&self, key: &str, messages: &[Message]);
}
```

**按 key 分片**沿用 claude-code：主 agent 用 session_id，Spawn 出的 subagent 用其 agent id，
互不串扰。agentrs 的 `SpawnTool` 已有独立子 agent 概念，注入子 agent 自己的 key 即可。

**持久化策略：不新增字段，走历史重放。**

`rehydrate_from_history` 反向扫描 `messages`，找最后一个 `ContentBlock::ToolUse { name: "TodoWrite", input }`
且其配对 `ToolResult` 非错误者，反序列化 `input.todos` 即得快照。

为什么不写 `Session.context_state`：
1. **分叉正确性白拿**。`SessionManager::fork_from(ForkBoundary::AtTurn)` 裁剪 messages 后，
   重放自然得到该时点的正确清单；若另存字段，fork 必须额外裁剪，是个必然被漏掉的隐藏耦合。
   这正是 pi 注释里点明的理由。
2. **零 schema 迁移**，老会话文件直接可读。
3. 成本可忽略：只在 resume/fork 时扫一次尾部消息，可提前 break。

### 3.5 提醒回灌机制

移植 claude-code 的 turn 计数策略，落点选在 `build_request()`：

```rust
// crates/agentrs-agent/src/todo_reminder.rs
pub(crate) struct TodoReminderConfig {
    pub turns_since_write: usize,      // 默认 10
    pub turns_between_reminders: usize // 默认 10
}

/// 纯函数：仅依据历史与当前快照判定是否需要提醒。
pub(crate) fn maybe_todo_reminder(
    messages: &[Message],
    todos: &[TodoItem],
    cfg: &TodoReminderConfig,
) -> Option<String>;
```

判定逻辑与 claude-code 一致：反向扫描，统计「距最近一次 `TodoWrite` 的 assistant turn 数」
与「距最近一次提醒标记的 turn 数」，两者均达阈值才产出。

**注入方式**（关键差异）：在 `build_request()` 中，紧随 `kind.control_prompt()` 的既有分支之后，
把提醒作为**临时 user 消息**追加到 `messages` 克隆体上——**不写回 `self.messages`，不落会话文件**。

```rust
let mut messages = self.messages.clone();
if let Some(prompt) = kind.control_prompt() { /* 既有逻辑 */ }
if let Some(text) = self.pending_todo_reminder.take() {
    messages.push(Message::now(Role::User, vec![ContentBlock::Text { text }]));
}
```

由此带来两个必须处理的副作用：

1. **提醒计数无法从历史里读出**（因为没写进历史）。解法：`AgentEngine` 持
   `todo_reminder_turns_since_last: usize` 计数器，随 assistant turn 递增，注入后归零。
   比反扫历史更简单也更准。
2. **prompt 前缀缓存**：注入点在 messages 尾部而非 system prompt，不破坏前缀缓存；
   但注入的那一轮之后缓存会在该位置断开。10 轮一次的频率下代价可接受，
   需在 `cache_diagnostics` 里记一条 debug 日志便于排查。

**文案**（照搬 claude-code 的关键约束）：

```
<system-reminder>
The TodoWrite tool hasn't been used recently. If you're working on tasks that would
benefit from tracking progress, consider using it. Also consider cleaning up the list
if it has become stale. This is just a gentle reminder — ignore if not applicable.
Never mention this reminder to the user.

Current todo list:
1. [in_progress] Wire the TodoStore into bootstrap
2. [pending] Add TUI rendering
</system-reminder>
```

**门控**：`TodoWrite` 未注册（配置关闭）→ 不提醒；清单为空且从未写过 → 仍提醒（这正是要推动模型开始用它的场景）。

### 3.6 循环护栏豁免

`TurnGuards` 的 `ToolCallCycleTracker` / `ToolCallFailureTracker` 需把 `TodoWrite` 排除在指纹之外。
理由同 deepseek 的 `exclude: [todo_write]`：**反复调用 Todo 是设计意图，不是失败循环**。
否则「改状态 → 干活 → 改状态」的正常节奏会被误判成 cycle。

### 3.7 配置

```rust
// crates/agentrs-config/src/todo.rs  （对标既有 plan.rs 的粒度）
pub struct TodoConfig {
    /// 是否注册 TodoWrite 工具。默认 true。
    pub enabled: bool,
    /// 是否允许多条 in_progress 并存。默认 false。
    /// Spawn 并发子 agent 的部署可置 true —— 该开关同时切换工具描述文案与运行时校验。
    pub allow_parallel_in_progress: bool,
    /// 提醒阈值；0 表示关闭提醒。
    pub reminder_turns: usize,        // 默认 10
}
```

对应 `agentrs.toml`：

```toml
[todo]
enabled = true
allow_parallel_in_progress = false
reminder_turns = 10
```

`allow_parallel_in_progress` 采纳 deepseek 的**双驱动**设计：不仅校验行为变，
**给模型的描述文案也跟着切换**（`DESCRIPTION_SINGLE` vs `DESCRIPTION_PARALLEL`），
避免出现「文案说只能一个、校验却放行多个」的长期矛盾。

### 3.8 提示词

`prompt.rs` 采用 opencode 的精简结构（约 60 行）而非 claude-code 的 184 行。理由：
agentrs 面向多 provider，包含上下文窗口较小的本地模型（见 memory：27B 本地端点），
184 行的 prompt 在每次请求里都是常驻成本。保留三条硬规则：

- **何时用**：3+ 个不同步骤 / 用户给了多项任务 / 用户明确要求 / 收到新指令即刻捕获；
- **何时不用**：单步、纯信息性、trivial；
- **完成判据**（最重要，两家都花了大篇幅）：只有真正做完（含必需的验证）才置 `completed`；
  受阻时保持 `in_progress` 并新增一条描述阻塞点的任务；测试没过 / 实现残缺 / 有未解错误 → 不得标完成。

外加整表替换的显式声明（deepseek 的 `DESCRIPTION_HEAD`）：
*"Send the ENTIRE list every call — it REPLACES the previous list. There are no partial updates."*

### 3.9 UI / 协议

**Phase 1（本方案范围）**：工具回执是计数摘要文本，TUI 通过既有 tool result 渲染路径展示。
`describe(&input)` 返回 `"Update todo list (N items, M in progress)"`，供终端行内显示。

**Phase 2（后续）**：结构化推送。需要：
- `OutputSink::emit_tool_result` 增加 `metadata: Option<Value>` 参数（`ProtocolEvent::ToolResult`
  已有该字段，目前恒为 `None`）；
- 或新增 `ProtocolEvent::TodoUpdated { key, todos }`；
- TUI 侧在 `crates/agentrs-tui/src/ui.rs` 增加常驻 Todo 面板，`in_progress` 项用 `active_form`
  驱动 spinner 文案（claude-code 的 activeForm 设计在此处兑现价值）。

Phase 2 涉及 sink trait 签名变更，影响 `protocol_sink` / `terminal` / `null_sink` 三个实现，
单独立项，不与 Phase 1 混做。

---

## 4. 任务分解

### T1 — 数据模型与校验（`agentrs-tools`）
- 新建 `src/todo/item.rs`：`TodoStatus`、`TodoItem`、`normalize(raw, allow_parallel) -> Result<Vec<TodoItem>, TodoError>`；
- `TodoError` 用 `thiserror`（公开 API 错误类型，符合 AGENTS.md 规约）；
- `item_test.rs`：空 content / 全空白 content / 重复 content / 单活跃违规 /
  `allow_parallel=true` 放行多活跃 / 保序 / `active_form` 空串归一为 `None`。

### T2 — TodoStore（`agentrs-tools`）
- `src/todo/store.rs`：`replace` / `snapshot` / `clear` / `rehydrate_from_history`；
- 内部 `Mutex`，锁作用域最小化，禁止跨 await 持锁；
- `store_test.rs`：key 隔离（主 agent vs subagent 互不影响）、替换语义、
  重放重建（含「最后一次是错误调用则跳过」「历史中无 TodoWrite 则为空」两个边界）。

### T3 — TodoWriteTool（`agentrs-tools`）
- `src/todo/prompt.rs`：`describe(allow_parallel) -> String`，HEAD + (SINGLE|PARALLEL) + TAIL 拼装；
- `src/todo/tool.rs`：实现 `Tool` trait；持 `Arc<TodoStore>` + `key: String` + `allow_parallel: bool`；
- `src/todo/mod.rs`：仅 `pub mod` / `pub use`，不含逻辑；
- `src/lib.rs` 增 `pub mod todo;`；
- `tool_test.rs`：schema 快照、正常写入、校验失败返回 `is_error`、
  回执计数正确、`is_concurrency_safe == false`、`category == Info`、`is_deferred == false`。

### T4 — 配置（`agentrs-config`）
- 新建 `src/todo.rs` + `todo_test.rs`（对标 `plan.rs`）；
- 挂进 `config.rs` 的顶层 `Config`，补 `schema.rs` 条目；
- 默认值测试 + TOML 反序列化测试。

### T5 — 装配（`agentrs-agent/bootstrap.rs`）
- `register_todo_tool(&mut registry, ...) -> Option<Arc<TodoStore>>`，按 `TodoConfig::enabled` 门控；
- `AgentEngine` 新增字段 `todo: Option<TodoRuntime>`（含 `Arc<TodoStore>`、key、提醒计数器）——
  按 AGENTS.md「结构体字段按职责分组」，这三项聚成一个内部结构而非平铺三个字段；
- `gating.rs` 的 `GATED_TOOLS` 增加 `"TodoWrite"` 及其提示文案
  （「已编译但未启用：在配置文件 `[todo]` 下设置 `enabled = true`」），
  使直接调用与 `ToolSearch` 查询给出一致答案；
- `resume` / `fork` 路径调用 `rehydrate_from_history`；
- `run_inner` 入口（新 user turn）调用 `clear`，实现 deepseek 的轮级生命周期。

### T6 — 提醒回灌（`agentrs-agent`）
- 新建 `src/todo_reminder.rs` + `todo_reminder_test.rs`：纯函数判定 + 文案渲染；
- `build_request()` 注入临时消息；
- turn 计数器在 `run_turn` 递增、注入后归零；
- 测试：未达阈值不注入 / 达阈值注入一次 / 注入后计数归零 / `reminder_turns = 0` 时永不注入 /
  **注入内容不出现在 `self.messages` 中**（防回归的关键用例）。

### T7 — 护栏豁免（`agentrs-agent/turn.rs`）
- 指纹计算跳过 `TodoWrite`；
- 测试：连续 5 次 `TodoWrite` 不触发 cycle 告警。

### T8 — 文档
- `docs/tools.md` 增 TodoWrite 章节；
- `AGENTS.md` 的 crate map 工具清单补 TodoWrite；
- `agentrs-tools` 的 `Cargo.toml` `description` 字段补上工具名；
- `CHANGELOG.md`。

**依赖顺序**：T1 → T2 → T3 → T4 → T5 → {T6, T7} → T8。T4 可与 T1-T3 并行。

**规模估计**：生产代码约 700–900 行，测试约 600–800 行，不含 Phase 2 UI。

---

## 5. 验收标准

功能：
1. 模型可创建、更新、完成清单，整表替换语义正确且保序；
2. 校验四条（trim / 非空 / 去重 / 单活跃）全部生效，失败返回 `is_error` 且信息可指导模型自纠；
3. `allow_parallel_in_progress` 同时改变**校验行为与工具描述文案**；
4. resume 与 fork 后清单正确重建，fork 在指定 turn 处得到该时点快照；
5. 主 agent 与 Spawn 子 agent 清单互相隔离；
6. 新 user turn 开始时清单清空；
7. 连续 N 轮未调用 TodoWrite 后注入提醒，且提醒不落入会话文件；
8. `[todo] enabled = false` 时工具不注册，`ToolSearch` 给出配置提示而非「未知工具」；
9. plan 模式下 TodoWrite 可用。

工程：
- `cargo fmt` 无 diff、`cargo clippy` 无告警、`cargo test` 全绿；
- 无 `unwrap()`（除已证明并注释的不变量）；
- 新增公开项经可见性复核，非跨 crate 使用者收敛为 `pub(crate)`；
- 日志：写入成功记 `debug`（**仅记条目数与状态计数，不记 content**——AGENTS.md 明令生产日志
  不得含工具输入输出内容）；校验失败记 `warn`（同样只记错误类型，不记原文）。

---

## 6. 风险与取舍

| 风险 | 影响 | 处置 |
|------|------|------|
| 提醒注入破坏 prompt 前缀缓存 | 该轮缓存命中率下降 | 10 轮一次，频率低；注入点在 messages 尾部而非 system；加 debug 日志观测 |
| 历史重放在超长会话上有开销 | resume 变慢 | 反向扫描 + 命中即 break；仅 resume/fork 触发，非每轮 |
| 压缩（compact）折叠掉 TodoWrite 调用 | 重建拿不到快照 | 重建失败时回落为空清单而非报错；**需与 `agentrs-compact` 确认 tool_use 块的折叠策略**（见下方待确认项） |
| 小模型不遵守单活跃约束 | 频繁校验失败刷屏 | 错误信息明确给出「至多一个」与实际数量；必要时该部署改 `allow_parallel = true` |
| 描述过长挤占本地模型上下文 | 小模型可用性下降 | 采用 opencode 精简版（~60 行）而非 claude-code 全量（184 行） |
| 与未来 Todo v2（任务图）冲突 | 返工 | v1 的 `TodoItem` 是 v2 `Task` 的严格子集（`content→subject`、`active_form` 同名），
  升级为增字段而非改语义；本方案不预埋 v2 抽象 |

**待确认项（需在 T2 开工前查证 `agentrs-compact`）**：上下文压缩后，历史中的
`ContentBlock::ToolUse` 是否被完整保留。若压缩会丢弃或摘要化 tool_use 入参，
则重放式重建在压缩发生后失效，届时需回退到「`Session.context_state` 存快照」方案
（代价是 fork 时要额外裁剪）。这是本方案唯一的架构性依赖，**建议第一步就验证**。

---

## 7. 分期

**Phase 1（本方案）**：T1–T8。模型侧完全可用，UI 走既有 tool result 文本路径。

**Phase 2（后续立项）**：结构化 UI。`OutputSink` 签名扩展 metadata / 新增 `TodoUpdated` 事件、
TUI 常驻面板、`active_form` 驱动 spinner、`/todos` 斜杠命令（对标 pi）。

**Phase 3（观望）**：任务图（对标 claude-code TaskTool）——`id` / `blocks` / `blockedBy` / `owner`
+ 文件锁 + 逐条 CRUD。**仅当 Spawn 多子 agent 出现任务抢占需求时才启动**；
在此之前，依赖图与文件锁的复杂度收不回成本。

---

## 8. 执行记录（2026-09-04）

**Phase 1 与 Phase 2 已实施完成并通过验证。Phase 3 未启动**（见 §7 的启动条件，条件尚未满足）。

### 8.1 §6 待确认项的查证结果

**结论：重放式重建单独不成立，已改为「持久化快照 + 重放优先」的混合方案。**

- `autocompact` 在 `compact/auto.rs:188` 用 `vec![boundary_msg, summary_msg]` 整体替换历史，
  **所有 `ToolUse` 块随之丢失**，压缩后纯重放拿不到任何快照。
- `microcompact`（`compact/micro.rs:74`）只把 `ToolResult.content` 置为占位串，
  `is_error` 与发起调用的 `ToolUse` 都完整保留，**不影响重放**。

最终实现：`Session.todos` 持久化快照作为兜底，但**重放优先**——历史里还有 `TodoWrite` 调用时以历史为准。
- fork 到历史某轮 → 重放得到该时点清单（原方案的分叉正确性保住了）；
- 压缩后 resume → 重放落空，回落到持久化快照；
- 两者都空 → 清空。

### 8.2 相对原方案的四处偏离

| # | 原方案 | 实际实现 | 原因 |
|---|--------|----------|------|
| 1 | 纯重放重建（§3.4） | 持久化快照 + 重放优先 | 见 8.1，压缩会吃掉 `ToolUse` |
| 2 | 每个新 user turn 清空（§3.3 / 验收 6） | **仅当全部 completed 时**在新 turn 开始清空 | 原方案验收 4（resume 恢复）与验收 6（每轮清空）互相矛盾——每轮都清则恢复无意义。折中同时满足 §3.3 的「轮末看到全绿清单」与「未完成清单跨轮存活」 |
| 3 | T7 把 `TodoWrite` 排除出循环护栏 | **不排除**，改为加回归测试 | agentrs 的护栏只对**失败**调用取指纹（`engine.rs:863-872`），成功的 TodoWrite 根本不进指纹，原担心的误判不存在；排除反而会让护栏对「反复被拒的 TodoWrite」失明 |
| 4 | `TodoStore` 按 agentId/sessionId 分片（§3.4） | 单 engine 单 store，**不要 key** | 每个 `AgentEngine` 自建 registry（`spawner.rs:205`），隔离是结构性的；子 agent 干脆不注册该工具 |

另有两处 T4/T8 的小修正：`schema.rs` 是 JSON-Schema 合法化器而非配置 schema，无需加条目；
`CHANGELOG.md` 由 release-please 从 commit message 生成，不手工编辑。

### 8.3 Phase 2 的接口选型

原方案 §3.9 给了两个选项，实际选择**新增 `ProtocolEvent::TodoUpdated` + `OutputSink::emit_todo_update` 默认空实现**，
而非扩展 `emit_tool_result` 的 metadata 参数：默认方法让 `terminal` / `null_sink` 与全部既有调用点零改动，
而改签名要动四个实现加调用点，代价只为两个 sink 关心的载荷。

`TodoSnapshot`（wire 类型）与 `TodoItem`（领域类型）**刻意分开**：`agentrs-tools` 已依赖 `agentrs-protocol`，
领域类型放不进 protocol（会成环）；分开也让宿主契约不随工具内部变动而漂移。
`status` 用裸 string，宿主遇到未知值应能展示而非解析失败。

### 8.4 验证结果

- `cargo fmt --check` 无 diff；`cargo clippy --workspace --all-targets` 无告警；
- 全量测试失败集合与改动前基线**逐字节一致**（余下失败均为本机代理拦截的网络用例，与本改动无关）；
- 新增约 130 个测试；改动 38 个文件，约 1200 行；
- **端到端实测**（本地 vLLM `Qwen3.6-35B-A3B`，见 [[local-llm-endpoints]]）：
  模型自发使用 TodoWrite 规划三步、单活跃纪律成立、`activeForm` 正常给出、计数与状态流转正确；
  会话文件正确落 `todos`；`--resume` + `--json-stream` 下 `todo_updated` 事件按重放结果恰好发一次。

---

## 9. 执行记录补遗（2026-09-07）：子 agent 清单与 Phase 3

用户明确要求继续推进，故在 §8 之后又完成两项。

### 9.1 子 agent 也获得 TodoWrite

原实现里 `spawner.rs` 的子 agent registry 不含 TodoWrite，子 agent 无法跟踪自己的多步工作。
现在每个子 agent 在 `build_tool_registry` 里自建 `TodoStore` 并注册 TodoWrite：
隔离性仍是结构性的（各自 engine、各自 store，父子互不可见），fork override 也能像收走其他工具一样收走它。
§8.2 表格第 4 行「子 agent 干脆不注册该工具」的描述由此作废。

### 9.2 Phase 3：任务图（`mode = "graph"`）

§7 原本把 Phase 3 标为「观望」，理由是「依赖图 + 文件锁的复杂度收不回成本」。
用户要求实施，故按方案建成，但对 agentrs 的实际并发模型做了两处调整：

| 方案设想 | 实际实现 | 原因 |
|---|---|---|
| 每任务一文件 + 高水位 ID 文件 + 文件锁（对标 claude-code） | 单个 `tasks.json` + 该文件上的独占锁，`next_id` 存在同一文件里 | claude-code 的每任务文件是为进程级 swarm 服务的；单文件 + 一把锁让「认领任务」这个读-改-写天然原子，高水位问题也随之消失 |
| 强调多 agent 抢占 | 保留 OS 文件锁，但注明 agentrs 子 agent 是**进程内**的 | 进程内并发用不上 OS 锁，但留着它才能保证两个 agentrs 进程共享同一 workspace 时不会写坏图 |

工具四件套：`TaskCreate` / `TaskList` / `TaskGet` / `TaskUpdate`，与 `TodoWrite` **互斥**（`[todo] mode` 二选一）。

store 强制执行 schema 表达不了的约束：
- 被未完成任务阻塞时拒绝置 `in_progress` / `completed`，错误里点名 blocker 并指出 `removeBlockedBy` 这条出路；`pending` 永远允许；
- 依赖双向镜像（A blocks B 同时写两侧），删除任务时清除所有指向它的边，不留悬空依赖；
- 拒绝自依赖与环，并把将要成环的路径打印出来；菱形（两支汇聚）不是环，放行；
- 状态是对**本次调用之后**的图判定的，所以「同一次调用里去掉依赖并开始任务」可行；
- 删除后 ID 不复用——复用会让仍然引用旧 ID 的依赖静默指向新任务。

**Phase 2 的 UI 在 graph 模式下同样可用**：`TodoRuntime` 抽象成 `PlanSource::{List, Graph}`，
task 转成 `TodoSnapshot` 时把 id 前缀进 subject（`#2 Build it`），这样 TUI 面板、
状态行与 `todo_updated` 协议事件全部复用，"blocked by 1" 才能被顺着找到对应任务。
两点差异：graph 的任务**不写进会话文件**（磁盘已是唯一真相，`TaskList` 就是模型重读状态的方式），
且**永不自动退休**（任务按 id 寻址，背着模型删掉会让引用它的依赖悬空）。

实现中发现并修掉一个真 bug：`TaskFile` 的 `#[serde(default = "first_id")]` 只在反序列化时生效，
derive 出来的 `Default` 会让 `next_id` 从 0 开始——首个任务拿到 id "0"，
而重开 store 后又从 "1" 开始。已改为手写 `Default`。

**验证**：`fmt` / `clippy` 干净；全量测试失败集合仍与基线逐字节一致；
task 模块新增 39 个测试。端到端（本地 vLLM）实测：模型一次 `TaskCreate` 建三个任务并用 `blockedBy`
串起依赖（含引用同批次兄弟任务），双向边正确落盘，`TaskUpdate` 对被阻塞任务的拒绝信息按预期返回给模型。

### 9.3 收尾自查发现的缺陷

自查「是否全部完成」时发现 §9.1 引入的子 agent 清单**没有考虑 §9.2 的 mode**：
`spawner.rs` 只判断 `todo.enabled`，于是 graph 模式的部署里子 agent 仍会拿到 `TodoWrite`——
一个该部署没有选择、父 agent 也没有的工具。已修。

修复顺带定了一个此前未明确的语义：**子 agent 跟随 workspace 的 mode，但两种模式的共享程度不同**。
- list 模式：子 agent 自建内存清单，与父完全隔离（保持 §9.1 的行为）；
- graph 模式：子 agent 指向**同一个** workspace 图。这正是把 store 做成文件+锁的意义——
  任务按 id 寻址且带 `owner`，子 agent 应当能认领父 agent 规划好的任务，
  而不是维护一份父 agent 永远看不见的私有副本。fork override 拒掉 `Task*` 时子 agent 无跟踪工具。

同时删掉了 `TodoRuntime::for_list`：两个调用点改走 `PlanSource` 之后它只剩测试在用，
按 AGENTS.md 的可见性要求不保留仅为测试存在的构造函数。

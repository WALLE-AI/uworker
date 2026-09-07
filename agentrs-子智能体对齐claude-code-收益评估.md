# agentrs 子智能体对齐 claude-code —— 分层收益评估

> 本文回答一个决策问题：**把 agentrs 的子智能体技术对齐 claude-code，预期效果相对现状能提升多少，代价是什么，哪些该做哪些不该做。**
> 现状事实与改造步骤见 `agentrs-子智能体调度通信与协作-深度分析报告.md`；三方架构差异见 `agentrs-vs-claudecode-vs-opencode-子智能体架构对比.md`。本文只做**成本收益判断**。

- 文档版本：v1.0
- 编写日期：2026-09-07
- 代码基线：`agentrs` @ `6dbf5116e4`
- 数据来源：端到端测试 + 探针实测 + 提示词尺寸实测（方法见附录）

---

## 目录

1. [结论速览](#一结论速览)
2. [评估口径：对齐分三层](#二评估口径对齐分三层)
3. [L1 机制层：确定收益](#三l1-机制层确定收益)
4. [L2 交互层：能力扩展](#四l2-交互层能力扩展)
5. [L3 平台层：不建议](#五l3-平台层不建议)
6. [总账与实施建议](#六总账与实施建议)
7. [伪收益识别](#七伪收益识别)
8. [附录：实测方法与原始数据](#八附录实测方法与原始数据)

---

## 一、结论速览

| 层 | 内容 | 工作量 | 收益确定性 | 建议 |
|---|---|---|---|---|
| **L1 机制层** | 取消传播、子提示词重建、usage 入账、预算配置化 | 5–8 人日 | **确定且可量化** | ✅ 立即做 |
| **L2 交互层** | 子可寻址、结构化回传、续跑、进度可见、worktree 隔离 | 3–4 周 | 高 | ✅ 做，但骨架用 opencode 的子会话 |
| **L3 平台层** | SendMessage 总线、Team/tmux、跨机 peer、agent-memory、fork 缓存对齐 | 2 月+ | **接近零** | ❌ 不做 |

**核心判断**：问题中的"对齐 claude-code"在 L1 层是**伪命题**——那些不是 claude-code 的特色，是任何正确实现都该有的东西，claude-code 只是恰好都做对了。真正的对齐选择只发生在 L2/L3，而建议是**只取 L2 的表皮，拒绝 L3 的骨架**。

**一句话回答"预期效果 vs 现状"**：

- L1 之后：子智能体从"会偷偷烧钱且停不下来"变为"行为正确、成本可见"。**每次 Spawn 批次省约 17k input token，取消从完全无效变为一个轮次内生效。**
- L2 之后：子从"匿名函数"变为"可寻址实体"，用户第一次能看见它、复用它、隔离它。
- L3 之后：在 agentrs 的实际场景下，几乎没有可观测的变化。

---

## 二、评估口径：对齐分三层

claude-code 的子智能体是三层叠加的，收益/成本每层差一个数量级。不分层就无法评估。

| 层 | 具体内容 | 性质 | claude-code 侧的实现 |
|---|---|---|---|
| **L1 机制层** | 取消传播；子提示词按实际工具集重建；token 用量入账；深度/并发/预算可配；工具描述渲染正确 | **正确性** | sync 子共享 `abortController`；`getAgentSystemPrompt(agentDefinition, ..., resolvedTools)`；`<usage>` + `costHook`；`CLAUDE_CODE_MAX_TOOL_USE_CONCURRENCY` |
| **L2 交互层** | 子有稳定 id；结构化结果回传；可续跑；进度/token 实时可见；async 执行形态；按需 worktree 隔离 | **能力扩展**（需子成为一等实体） | `LocalAgentTask` 注册表；`<task-notification>` XML；`resumeAgent`；`ProgressTracker`；`run_in_background`；`isolation` |
| **L3 平台层** | agent 间消息总线；团队编制；跨机 peer；按 agent type 的持久记忆；fork prompt-cache 字节对齐 | **产品形态**（需多 agent 长期共存） | `SendMessage`（名/id/广播/uds/bridge）；`TeamCreate` + tmux + mailbox；`agentMemory`（3 scope）；`buildForkedMessages` |

---

## 三、L1 机制层：确定收益

### 3.1 指标对照

| 指标 | 现状（实测） | L1 后 | 证据 |
|---|---|---|---|
| Ctrl-C 后子的残留运行 | **最长 200 轮**，期间继续写文件、继续烧 token | ≤1 个轮次边界 | 探针：传入已 cancel 的 `CancellationToken`，子仍返回 `## slow [OK] finished anyway` |
| 每子每请求的无效 prompt | **1663 字节 ≈ 420 token** | 0 | 实测：父提示词（10 skill）2496 B vs 按子策略重建 833 B |
| 上述浪费的复利 | 5 子 × 8 轮 ≈ **17k input token / 次 Spawn 批次** | 0 | 上行 × 典型批次规模 |
| 幻觉工具调用 | 提示词宣传 Spawn/Skill/WebSearch，实际工具表仅 7 个本地工具 | 消除 | 探针：`request.system` 含父原文，`request.tools = [Edit, ExecCommand, Glob, Grep, Read, TodoWrite, Write]` |
| 子 token 进父账本（成本口径） | **0%** —— 仅拼进文本给模型阅读 | 100% | `spawn_tool.rs:110` |
| 会话累计用量 | 少算全部子的消耗 | 准确 | 同上 |
| 深度 / 并发 / 预算可配 | 全部硬编码；`max_per_call=5` 仅约束单次调用，跨调用无上限 | 配置化 + workspace 级信号量 | `spawn_tool.rs:12-14` |
| fork 模式 skill 可用性 | **不可用**（生产装配路径 spawner 为 None） | 可用 | `bootstrap.rs:355` |

### 3.2 成本

**约 5–8 人日。**

关键在于工作量比直觉低：`build_system_prompt_with_shell_and_tool_policy`（`context.rs:206`）**已经接受 `tool_policy` 参数**，并在策略变化时主动 invalidate `tool_guidance` 与 `skills` 两个缓存段（`context.rs:219`）。子提示词重建基本是"把已有函数在这条路径上调一次"，不是新功能开发。

契约变更仅一处：`Spawner::spawn_fork` 增加 `CancellationToken` 参数。该 trait 只有一个生产实现（`AgentSpawner`）与一个调用方（`agentrs-skills::executor::execute_fork`）。

### 3.3 风险

**低。** 无架构改动，无新增依赖，无跨 crate 依赖方向变化。唯一需注意的是子提示词重建后**不能丢失环境上下文**（workspace / shell / AGENTS.md），需专门测试覆盖。

### 3.4 判断

**无条件做。** 420 token/请求的浪费与"取消完全无效"是纯亏损，不存在任何设计权衡。这一层与其说是"对齐 claude-code"，不如说是"修复已知缺陷"。

---

## 四、L2 交互层：能力扩展

### 4.1 逐能力价值判断

| 能力 | 现状 | L2 后 | 价值 | 理由 |
|---|---|---|---|---|
| 子的过程可见性 | `NullSink` 全丢，用户看不到任何过程 | 进度事件 / token 计数 / 活动描述 | **高** | 长任务下"5 个子在跑但屏幕不动"是明确的体验缺陷，也无法判断是否卡死 |
| 结果格式 | Markdown 拼接，不可解析 | `<task-notification>` 四段式（status / summary / result / usage） | **高** | 当前格式对 JSON stream 宿主（AgentrsUI）等同于无结构；且截断发生在 orchestration 层，宿主拿到的可能是被腰斩的 Markdown |
| 续跑同一个子 | 不可能（`session.enabled = false`） | `task_id` 复用子上下文 | **中高** | 省掉"重新描述任务 + 子重新读一遍文件"的重复成本；对多轮迭代型任务收益显著 |
| async 执行形态 | 只有同步阻塞父的工具调用 | 父可继续工作，子完成后回注通知 | **中** | 价值与典型子任务时长成正比；短任务下收益有限 |
| worktree 隔离 | 无。5 个子共享 cwd，均持 Write/Edit 且 `auto_approve = true` | 按需隔离，未改动自动清理 | **中高** | 当前是整个子系统安全面最宽的一处 |

### 4.2 成本

**约 3–4 周。**

主要开销**不在** claude-code 那套 UI，而在**打开子的会话持久化**——它是 L2 全部五项能力的共同前提。当前 `spawn_one` 里 `config.session.enabled = false` 是主动关掉的，打开后需要 `SessionManager` 支持 `parent_id` 字段与 `children(parent_id)` 查询。

### 4.3 关键摩擦点（claude-code 侧不存在的问题）

| 摩擦点 | 说明 | 影响 |
|---|---|---|
| **状态载体差异** | claude-code 的 `LocalAgentTask` 表挂在 React `AppState` 上，`setAppState` 天然驱动 UI 重绘。agentrs 需自建 `Arc<RwLock<SubAgentRegistry>>` + 扩展 JSON stream 协议事件 + TUI 渲染 | 三处都要动，没有免费午餐 |
| **无轮次间钩子** | agentrs 的 `AgentEngine::run` 是阻塞 turn loop，没有"轮次之间检查外部输入"的检查点 | async 形态与后续任何消息投递都必须先在这里开口子 |
| **输出通道** | 子当前用 `NullSink`；改为转发到父 sink 时需区分"进度事件"与"完整流式文本"，否则父的输出会被子刷屏 | 需要一个带前缀/降采样的 sink 实现 |

### 4.4 判断

**做，但要认清它实际是"opencode 的架构 + claude-code 的表皮"。**

- 子会话持久化（`Session{parent_id}`、`task_id` 续跑、删除级联）是 **opencode** 的做法
- `<task-notification>` 的字段设计、进度追踪、`isolation` 参数是 **claude-code** 的做法

两者不冲突。这也是《优化指引》中"对标 opencode"的确切含义：**骨架抄 opencode，交互面抄 claude-code**。

---

## 五、L3 平台层：不建议

| 能力 | 判断 | 理由 |
|---|---|---|
| `SendMessage` 消息总线 | ❌ | 前提是多个子长期共存且互相知道对方在做什么。agentrs 的典型场景是"父派 3-5 个独立调研任务"，兄弟间本就无依赖。做出来无人调用 |
| Team / tmux 编队 / 跨机 peer | ❌ | claude-code 的产品形态（人当协调者、多 agent 各占一个 tmux pane）。agentrs 是 CLI + JSON stream，形态不符；mailbox 是文件 IPC，Rust 侧需重造 |
| agent-memory（按 agent type 分片的持久记忆） | ❌ | `agentrs-memory` 已有跨会话记忆，再加一层按 agent type 的分片，收益不明确 |
| fork prompt-cache 字节对齐 | ❌（暂不做） | 见下 |

### 5.1 fork 缓存对齐为何不划算

claude-code 为此做了三件事：`useExactTools` 让子拿父的**精确工具池**、复用父**已渲染的 system prompt 字节**、`buildForkedMessages` 给所有 `tool_use` 填**同一个占位 result**——目的是让多个 fork 子的 API 请求前缀 byte-identical，共享 prompt cache。

收益条件苛刻，四条需同时满足：

1. provider 为 Anthropic / Bedrock 系（有 prompt cache）
2. `prompt_caching` 开启
3. 父上下文足够大（小上下文缓存收益不显著）
4. 多个 fork 子同时或紧邻运行（缓存窗口内）

更关键的是**它与安全约束直接冲突**：要求子拿父的精确工具池，意味着子会拿到 `Spawn` 与网络工具。claude-code 接受了这个冲突，改用运行时嗅探 `<fork-boilerplate>` 标签打补丁（`forkSubagent.ts:73`）。agentrs 当前用"子的工具表里没有 Spawn"做结构性禁止，引入缓存对齐等于主动废掉这条保证。

### 5.2 L3 的共同问题

**它们解决的是 claude-code 的规模问题，不是 agentrs 的问题。** claude-code 的代码注释里那句 "~135 chars × 34M Explore runs/week"（`tools/AgentTool/constants.ts:6`，解释为何对一次性 agent 省略结果尾巴）是这一点最直白的证据：那是按周千万次调用量做的优化决策。

---

## 六、总账与实施建议

### 6.1 总账

| | 工作量 | 主要收益 | 风险 | 建议 |
|---|---|---|---|---|
| L1 | 5–8 人日 | 消除 420 token/请求浪费；取消生效；成本可见；fork skill 恢复可用 | 低 | ✅ 立即做 |
| L2 | 3–4 周 | 可见性 / 可解析结果 / 续跑 / 隔离 | 中（会话模型改动） | ✅ 做，骨架用 opencode 子会话 |
| L3 | 2 月+ | 多 agent 协作 | 高（形态不符） | ❌ 不做 |

### 6.2 实施顺序（与《优化指引》的阶段对应）

| 本文分层 | 指引文档阶段 | 说明 |
|---|---|---|
| L1 | 阶段 0（止血）+ 阶段 1（取消传播）+ 阶段 2（提示词与用量）+ 阶段 3（配置化） | 四个阶段合起来即 L1 全集 |
| L2 | 阶段 4（子会话持久化与续跑）+ 阶段 5（隔离） | 阶段 4 是 L2 其余能力的前提 |
| L3 | 指引文档"明确不做的事" | 与本文结论一致 |

### 6.3 重新评估的触发条件

以下情况出现时，本文的结论应重新审视：

| 结论 | 推翻条件 |
|---|---|
| L3 的 `SendMessage` 不做 | 阶段 4 完成后，出现真实的兄弟协作需求（例如子 A 的产出必须实时喂给运行中的子 B） |
| fork 缓存对齐不做 | agentrs 的主力 provider 固定为 Anthropic 系，且出现"同一父上下文并发派生多子"的高频场景 |
| Team/tmux 不做 | agentrs 产品定位转向多人/多机协作 |

---

## 七、伪收益识别

评估过程中识别出的、看似收益实则不成立的项，记录以免后续重复讨论：

| 伪收益 | 为何不成立 |
|---|---|
| "继承父提示词能命中 prompt cache" | agentrs 子的工具表与父不同，请求前缀本就不一致，缓存不可能命中。当前做法是**既没拿到缓存收益，又制造了提示词与工具集的矛盾**——方向与 claude-code 的 fork 恰好相反 |
| "子拿到 MCP / Web 工具能力更强" | 子拿到网络与 MCP 工具的同时也拿到了绕过父策略的路径。真正该修的是"工具集从父注册表按策略投影"，而不是简单放开 |
| "对齐 claude-code 的 Team 能提升并行度" | 并行度受限于 provider 速率与 workspace 写冲突，不受编队机制影响。当前连全局并发信号量都没有，先做 L1 的预算控制收益更直接 |
| "agent-memory 能让子更聪明" | agentrs 已有 `agentrs-memory`；按 agent type 分片的价值前提是"同类子被反复调用且积累领域知识"，agentrs 当前的子是匿名一次性的，没有"同类"概念 |

---

## 八、附录：实测方法与原始数据

### 8.1 提示词尺寸实测

**方法**：在 `agentrs-agent` crate 内临时挂一个测试，用同一份 `ResolvedShell` 分别构建两份系统提示词，打印字节数后移除该测试（已确认工作区干净、528 个单测全绿）。

```rust
// 父：不受限策略 + 10 个 skill
let parent = build_system_prompt_with_shell_and_tool_policy(
    &mut SystemPromptCache::new(), None, "/tmp", "test-model", &shell, &skills,
    None, None, false, false, &ToolPolicy::Unrestricted,
);
// 子：按实际工具集重建 + 无 skill
let child_policy = ToolPolicy::allow_only(
    ["Read","Write","Edit","ExecCommand","Grep","Glob","TodoWrite"]);
let child = build_system_prompt_with_shell_and_tool_policy(
    &mut SystemPromptCache::new(), None, "/tmp", "test-model", &shell, &[],
    None, None, false, false, &child_policy,
);
```

**原始数据**：

| 场景 | 父 | 子 | 差额 |
|---|---|---|---|
| 无 skill | 1163 B | 833 B | **330 B**（≈ 80 token，纯 tool guidance 段） |
| 10 个 skill | 2496 B | 833 B | **1663 B**（≈ 420 token） |

**说明**：该测量**未包含** memory 段、AGENTS.md 段与自定义提示词。真实部署中父提示词更大，差额只会高于此值——1663 B 是保守下界。

### 8.2 取消传播实测

对 `SpawnTool::execute_with_follow_up` 传入已 `cancel()` 的 `CancellationToken`，子智能体照常执行完毕并返回 `## slow [OK] finished anyway`。确认 `SpawnTool` 未覆写 `execute_cancellable`，走 `Tool` trait 默认实现丢弃 token。

### 8.3 提示词/工具集不一致实测

父 `system_prompt` 设为 `"PARENT PROMPT: you may use Spawn, Skill and WebSearch."`，子收到的 `request.system` 为该字符串原文，而 `request.tools` 为 `["Edit","ExecCommand","Glob","Grep","Read","TodoWrite","Write"]`。

### 8.4 复利估算口径

"17k input token / 次 Spawn 批次" = 1663 B ÷ 4 B/token × 5 子 × 8 轮 ≈ 16.6k。其中"8 轮"为典型调研型子任务的轮次估计，非实测；实际值随任务复杂度线性变化。子任务越长，浪费越大。

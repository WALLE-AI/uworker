# agentrs 子智能体技术架构设计

> 本文定义 agentrs 子智能体子系统的**目标架构**：核心抽象、模块归属、接口契约、生命周期与通信语义。
> 现状分析与差距见 `agentrs-子智能体调度通信与协作-深度分析报告.md`；选型依据见 `agentrs-子智能体对齐claude-code-收益评估.md`；三方对比见 `agentrs-vs-claudecode-vs-opencode-子智能体架构对比.md`。
> 本文只回答"应该长成什么样"，不重复"现在什么样"。

- 文档版本：v1.0
- 编写日期：2026-09-07
- 代码基线：`agentrs` @ `6dbf5116e4`
- 约束基准：`agentrs/AGENTS.md`

---

## 目录

1. [设计目标与约束](#一设计目标与约束)
2. [核心抽象](#二核心抽象)
3. [模块与 crate 归属](#三模块与-crate-归属)
4. [接口契约](#四接口契约)
5. [生命周期](#五生命周期)
6. [子上下文构建](#六子上下文构建)
7. [通信设计](#七通信设计)
8. [取消与清理语义](#八取消与清理语义)
9. [用量与可观测](#九用量与可观测)
10. [并发与预算控制](#十并发与预算控制)
11. [协议事件扩展](#十一协议事件扩展)
12. [与 Team 的接缝](#十二与-team-的接缝)
13. [兼容性与迁移](#十三兼容性与迁移)
14. [待决问题](#十四待决问题)

---

## 一、设计目标与约束

### 1.1 目标

| # | 目标 | 度量 |
|---|---|---|
| G1 | 子智能体是**可寻址实体**，不是匿名 future | 有稳定 id、可查状态、可单独取消 |
| G2 | 父的取消**一个轮次边界内**到达每个子 | 无残留执行 |
| G3 | 子的上下文**与其真实能力一致** | 提示词宣传的工具 ≡ 工具表中的工具 |
| G4 | 子的成本**对父可见** | 计入会话累计用量与协议事件 |
| G5 | 一套运行时同时承载 `Spawn` 与未来的 Team | 不出现两套生命周期管理 |
| G6 | 子可**续跑** | 复用其已加载的上下文，无需重述任务 |

### 1.2 约束

**来自 `AGENTS.md`（硬约束）**

- 依赖只能向下流动：`types → config → tools → agent`。子智能体运行时属 `agentrs-agent`；跨 crate 契约下沉到 `agentrs-types`
- 不在 `mod.rs` / `lib.rs` 写业务逻辑
- 单文件 < 1000 行；测试外置到同目录 `*_test.rs`
- 可见性就窄不就宽：优先 `pub(crate)`
- 平台差异集中封装；shell 一律走 `agentrs_config::shell`
- 生产日志不含 prompt / 工具输入输出 / 文件内容 / 消息正文

**来自现状（工程约束）**

- `Spawner` trait 已被 `agentrs-skills` 依赖，签名变更需同步 `execute_fork`
- `Session` 已有 `forked_from` / `root_id` 血缘字段与 `fork_from(source, id, ForkBoundary)`，**复用而非新建**父子关系模型
- `AgentEngine::run_inner` 的 turn loop 中已存在 `follow_up_blocks` 通道（工具执行后追加一条 user 消息），**收件注入复用它**，不新造机制

### 1.3 非目标

明确不在本设计范围内：跨进程/跨机寻址（`uds:` / `bridge:`）、tmux 等终端编排后端、按 agent type 的持久记忆、fork 的 prompt-cache 字节对齐。理由见评估文档。

---

## 二、核心抽象

### 2.1 一句话

**子智能体 = 一条带父指针的会话 + 一个常驻句柄。**

- **会话**（持久面）承载历史、用量、上下文状态、可续跑性 —— 复用 `Session` 与 `forked_from`
- **句柄**（运行面）承载 id、状态、取消令牌、收件箱、`JoinHandle` —— 新增 `SubAgentHandle`

两者一一对应，由 `SubAgentRegistry` 统一持有。这是本设计的**唯一核心决策**，其余都是它的推论。

### 2.2 为什么是这个抽象

| 需求 | 由"会话"满足 | 由"句柄"满足 |
|---|---|---|
| 续跑（G6） | ✅ 历史即上下文 | |
| 用量归集（G4） | ✅ `Session.total_usage` | |
| 取消（G2） | | ✅ `CancellationToken` + `JoinHandle` |
| 可寻址（G1） | ✅ session id | ✅ 运行态查询 |
| Team 成员（G5） | ✅ 队员即长命子会话 | ✅ 收件箱挂在句柄上 |

若只做句柄（claude-code 的 `LocalAgentTask` 路线），续跑与用量要另起一套持久化，并与 `SessionManager` 产生双写一致性问题。若只做会话（无句柄），取消与收件无处安放。**两者都要，且必须成对。**

### 2.3 三种子形态统一为一个模型

| 形态 | 差异 | 共用 |
|---|---|---|
| `Spawn` 的一次性子 | 父同步等待；完成即 `Finished` | 同一注册表、同一取消路径、同一用量归集 |
| fork 模式 skill 的子 | 带 `ForkOverrides`（model/effort/tools） | 同上 |
| Team 队员（未来） | 常驻；有名字；有收件箱 | 同上 |

**它们不是三套运行时，是同一运行时的三种参数组合。** 具体差异只体现在 `SubAgentSpec` 的字段取值上。

### 2.4 父子拓扑

父与子**同构**：两者都是 `AgentEngine` 实例，跑同一套 turn loop。父额外持有一个 `Arc<SubAgentRegistry>`。

```
                 ┌─────────────────────────────┐
                 │  主智能体 AgentEngine        │
                 │  ├─ SessionManager           │
                 │  ├─ ToolRegistry（全量）      │
                 │  ├─ OutputSink（真实）        │
                 │  └─ Arc<SubAgentRegistry> ───┼──┐
                 └──────────┬──────────────────┘  │
                            │ Spawn 工具调用        │ 同一个 Arc
                ┌───────────┴───────────┐         │
                ▼                       ▼         │
   ┌────────────────────┐   ┌────────────────────┐│
   │ 子 AgentEngine      │   │ 子 AgentEngine      ││
   │ ├─ 子会话(forked_from)│   │ ├─ 子会话           ││
   │ ├─ 投影工具集         │   │ ├─ 投影工具集        ││
   │ ├─ SubAgentSink      │   │ ├─ SubAgentSink     ││
   │ └─ Arc<Registry> ────┼───┼─┴───────────────────┼┘
   └────────────────────┘   └────────────────────┘
        （depth>1 时子才用得到 registry 去再派生）
```

**关键决策：`SubAgentRegistry` 是进程内全局单例，不是每个父一个。**

理由：并发闸门（`Semaphore`）与 `cancel_all` 必须覆盖整棵树。若每层各持一个 registry，`depth = 2` 时孙辈不受祖父的并发上限约束，取消也无法穿透。子拿到的是**同一个 `Arc`**，不是新建实例。

**父子的不对称之处**（子不是缩小版的父）：

| 能力 | 父 | 子 |
|---|---|---|
| 面向的读者 | 人类用户 | 父智能体 |
| OutputSink | 真实终端 / 协议 | `SubAgentSink`（只发进度事件） |
| 斜杠命令 | 有（`handle_command`） | **无** —— 子的输入来自父，不来自人 |
| 计划模式 | 有 | **无**（见 §6.1 排除清单） |
| 会话可见性 | `/resume` 列表 | 隐藏但可寻址（见 §14 Q2） |
| 上下文压缩 | 有 | 有（独立于父，见 §5.4） |

---

## 三、模块与 crate 归属

```
agentrs-types/src/
  subagent.rs              // 跨 crate 契约：SubAgentId / Spec / Status / Result / Spawner trait
                           // （取代现有 spawner.rs，保留其 ForkOverrides）

agentrs-config/src/
  subagent.rs              // SubAgentConfig：并发、深度、预算、超时

agentrs-agent/src/
  subagent.rs              // 仅 mod 声明与再导出
  subagent/spawner.rs      // AgentSpawner：构造子引擎并驱动
  subagent/registry.rs     // SubAgentRegistry：id → SubAgentHandle
  subagent/handle.rs       // SubAgentHandle 与状态机
  subagent/context.rs      // 子的工具集投影 + 提示词重建
  subagent/inbox.rs        // 收件箱（Team 前置，Spawn 阶段仅留空实现）
  subagent/*_test.rs
  spawn_tool.rs            // 模型可见的 Spawn 工具（保持现位置）
```

**依赖方向检查**：`agentrs-types::subagent` 零内部依赖；`agentrs-config::subagent` 仅依赖 serde；`agentrs-agent::subagent` 依赖前两者 + `agentrs-tools` + `agentrs-providers`。无上行、无循环。

**为何 `Spawner` trait 留在 `agentrs-types`**：`agentrs-skills::executor::execute_fork` 需要它，而 skills 不得依赖 agent。这是现有正确设计，保留。

---

## 四、接口契约

### 4.1 跨 crate 契约（`agentrs-types/src/subagent.rs`）

```rust
/// 子智能体的稳定标识。
///
/// 与子会话 id 相同——一个子只有一个身份，避免两套 id 空间需要互相映射。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubAgentId(String);

/// 派生一个子智能体所需的全部输入。
#[derive(Debug, Clone)]
pub struct SubAgentSpec {
    /// 展示名。Team 队员的名字在队内唯一；Spawn 的子仅作标签。
    pub name: String,
    /// 使用哪份 agent 定义。None = `general-purpose`。见 §4.4。
    pub agent_type: Option<String>,
    /// 首轮任务描述。
    pub prompt: String,
    /// 轮次上限；None 表示取配置默认值。
    pub max_turns: Option<usize>,
    /// 单次响应输出 token 上限；None 表示取配置默认值。
    pub max_tokens: Option<u32>,
    /// 覆盖系统提示词。None 表示按子的工具策略重建（见 §6.2）。
    pub system_prompt: Option<String>,
    /// 当前嵌套层级。0 = 父直接派生。
    pub depth: usize,
    /// 续跑既有子；None 表示新建。
    pub resume: Option<SubAgentId>,
    /// 常驻子（Team 队员）：跑完首轮不结束，等待收件。
    pub persistent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAgentStatus {
    /// 已登记，尚未开始。
    Pending,
    /// 正在执行。
    Running,
    /// 常驻子完成一轮，等待收件。
    Idle,
    /// 正常结束。
    Finished,
    /// 执行出错。
    Failed,
    /// 被取消。
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct SubAgentResult {
    pub id: SubAgentId,
    pub name: String,
    pub text: String,
    pub usage: TokenUsage,
    pub turns: usize,
    pub status: SubAgentStatus,
}

/// fork 模式 skill 的覆盖项。沿用现有定义。
#[derive(Debug, Clone, Default)]
pub struct ForkOverrides {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// 只能收窄父策略，永不扩权。
    pub allowed_tools: Vec<String>,
}

/// 派生能力的抽象，使 `agentrs-skills` 不必依赖 `agentrs-agent`。
#[async_trait]
pub trait Spawner: Send + Sync {
    /// 派生并等待结果。`cancel` 触发时子在一个轮次边界内终止。
    async fn spawn(
        &self,
        spec: SubAgentSpec,
        overrides: ForkOverrides,
        cancel: CancellationToken,
    ) -> SubAgentResult;
}
```

**相对现状的变化**：`SubAgentConfig` → `SubAgentSpec`（增 `depth` / `resume` / `persistent`，预算字段改 `Option`）；`is_error: bool` → `status: SubAgentStatus`（区分 Failed 与 Cancelled）；`spawn_fork` 与 `spawn_one` 合并为 `spawn`（fork 只是 `overrides` 非空的情形）。

### 4.2 运行时（`agentrs-agent/src/subagent/registry.rs`）

```rust
/// 进程内所有活跃子智能体的登记处。
///
/// 单一实例由父引擎持有并注入 `SpawnTool`（未来还有 Team 工具），
/// 使 `Spawn` 的子与 Team 队员共用同一套生命周期管理。
pub(crate) struct SubAgentRegistry {
    handles: RwLock<HashMap<SubAgentId, SubAgentHandle>>,
    /// 全局并发闸门。
    permits: Arc<Semaphore>,
    /// 本次父 turn 内所有子累计输出 token，用于预算裁决。
    turn_output_tokens: AtomicU64,
}

impl SubAgentRegistry {
    pub(crate) fn register(&self, handle: SubAgentHandle);
    pub(crate) fn get(&self, id: &SubAgentId) -> Option<SubAgentSnapshot>;
    pub(crate) fn list(&self) -> Vec<SubAgentSnapshot>;
    /// 取消单个子；返回它是否存在。
    pub(crate) fn cancel(&self, id: &SubAgentId) -> bool;
    /// 取消全部并等待回收——父 turn 结束或用户中断时调用。
    pub(crate) async fn cancel_all(&self);
    /// 汇总本 turn 的子用量，供父引擎计入会话累计。
    pub(crate) fn drain_turn_usage(&self) -> TokenUsage;
}
```

```rust
/// 一个活跃子智能体的运行面。
pub(crate) struct SubAgentHandle {
    pub(crate) id: SubAgentId,
    pub(crate) name: String,
    pub(crate) status: Arc<RwLock<SubAgentStatus>>,
    pub(crate) cancel: CancellationToken,
    pub(crate) join: JoinHandle<SubAgentResult>,
    /// 收件箱。Spawn 的一次性子不使用；Team 队员由此接收消息。
    pub(crate) inbox: Option<mpsc::Sender<InboxMessage>>,
    pub(crate) started_at: Instant,
}
```

**设计要点**：

- `join` 必须被持有——这是取消能真正回收任务的前提（现状直接丢弃）
- `inbox` 是 `Option`：一次性子为 `None`，不为未启用的 Team 付出成本
- `status` 用 `Arc<RwLock<_>>` 而非消息传递：查询远多于变更，读锁更合适

### 4.3 父引擎侧的改动

本设计不只改子侧。`AgentEngine` 需要新增：

```rust
impl AgentEngine {
    /// 注入全局注册表。父在 bootstrap 时持有；子在 depth>1 时收到同一个 Arc。
    pub(crate) fn set_subagent_registry(&mut self, registry: Arc<SubAgentRegistry>);

    /// 把子的用量累加进会话累计（成本口径）。
    /// 明确不碰 `context_state`——见 §9.1。
    pub(crate) fn record_subagent_usage(&mut self, usage: &TokenUsage);

    /// 派生给子的取消令牌。父 turn 取消时整棵子树随之取消。
    fn subagent_cancel_token(&self) -> CancellationToken {
        self.turn_cancel.child_token()
    }
}
```

**`child_token()` 是关键**：它让"父取消 → 子取消 → 孙取消"成为 `tokio-util` 的天然行为，无需手写级联逻辑（对应 §14 Q4）。

父 turn loop 需要挂两个钩子：

| 位置 | 动作 |
|---|---|
| turn 正常结束 / 出错返回前 | `registry.drain_turn_usage()` → `record_subagent_usage()`；一次性子应已终态，未终态的记 `warn` |
| 用户中断（`cancel_running_tools`） | `turn_cancel.cancel()` 自动经 `child_token` 传导；随后 `registry.cancel_all().await` 等待回收 |

### 4.4 Agent 定义与预制子智能体

#### 4.4.1 要解决什么

现状 `Spawn` 的参数只有 `name` + `prompt`，**没有类型概念**。后果：无法表达"派一个只读的子"、无法按任务类型选模型、父每次都要现写完整任务描述、`ForkOverrides` 的能力只对 skill 生效而 `Spawn` 用不上。

两个对标实现都有预制 agent，且**预制的核心是权限配置包而非人设**：

- claude-code：`Explore` 的定义主体是 `disallowedTools: [Agent, ExitPlanMode, Edit, Write, NotebookEdit]`，外加对外部用户强制 `model: 'haiku'`、`omitClaudeMd: true`
- opencode：`explore` 的权限是 `"*": "deny"` 后逐个 allow grep/glob/list/bash/read/webfetch/websearch

没有 agent 定义，"派一个绝对不会改代码的子去调研"这件事在 agentrs 里**无法表达**。

#### 4.4.2 类型（`agentrs-types/src/subagent.rs`）

```rust
/// 一份可复用的子智能体配置。
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// 唯一标识，即 `SubAgentSpec.agent_type` 的取值。
    pub name: String,
    /// 选择依据，注入父提示词供模型决定派谁。hidden 定义可为空。
    pub when_to_use: String,

    /// 工具收窄。二者皆空 = 使用 §6.1 的默认投影结果。
    /// **只能收窄父策略，永不扩权**——与 §6.3 的交集语义一致。
    pub allowed_tools: Vec<String>,
    /// 显式排除，优先级高于 `allowed_tools`。
    pub denied_tools: Vec<String>,

    /// None = 继承父。
    pub model: Option<String>,
    pub effort: Option<String>,
    pub temperature: Option<f32>,
    pub max_turns: Option<usize>,
    pub max_tokens: Option<u32>,

    /// None = 按子的实际能力重建（§6.2）。
    pub system_prompt: Option<String>,
    /// 不向该子注入 AGENTS.md 段——只读调研类子不需要提交/规范约定。
    /// 对齐 claude-code 的 `omitClaudeMd`。
    pub omit_project_rules: bool,

    /// 不进模型可见的菜单。用于内部 LLM 任务，见 §4.4.6。
    pub hidden: bool,
    pub source: AgentSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSource {
    BuiltIn,
    /// `~/.agentrs/agents/*.md`
    User,
    /// `<workspace>/.agentrs/agents/*.md`
    Project,
}
```

**`ForkOverrides` 的归位**：它是 `AgentDefinition` 的真子集（model / effort / allowed_tools）。保留它作为**内联匿名定义**——skill 的 fork 配置不必先注册一个具名 agent。`spawn()` 内部把二者合一后再构建子。

#### 4.4.3 注册表与加载

`agentrs-agent/src/subagent/definitions.rs`：

| 来源 | 位置 | 优先级 |
|---|---|---|
| 内置 | 编译期常量 | 最低 |
| 用户 | `~/.agentrs/agents/*.md` | 中 |
| 项目 | `<workspace>/.agentrs/agents/*.md` | 最高 |

同名覆盖，高优先级胜出。文件格式复用 skills 的 frontmatter + 正文约定（正文即 `system_prompt`），避免再造一套解析。加载失败的单个文件记 `warn` 并跳过，不影响其余——与 skills 加载的现有行为一致。

配置项 `subagent.builtin_agents`（默认 `true`）可整体关闭内置定义，供需要空白起点的宿主使用（对标 claude-code 的 `CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS`）。

#### 4.4.4 预制清单

最小集，宁少勿多——每个可见定义都要占父提示词的 token（实测：10 个 skill 的清单段约 1663 B，agent 清单同理）。

| name | 工具 | 模型 | 其他 | 目的 |
|---|---|---|---|---|
| `general-purpose` | 默认投影全集 | 继承 | — | 兜底；`agent_type` 缺省即此项 |
| `explore` | Read / Grep / Glob + 只读 ExecCommand | 可配更廉价模型 | `omit_project_rules: true` | 高频只读调研，收益最大 |
| `plan` | 同 `explore` | 继承 | `omit_project_rules: true` | 输出结构化实施计划 |
| `summarize` | 全禁 | 可配 | `hidden: true` | 见 §4.4.6 |

`explore` 的"只读 ExecCommand"需要一个**只读命令白名单**（`ls` / `git status` / `git log` / `git diff` / `cat` / `head` / `tail` / `find`）。若该白名单机制尚未就绪，首版直接不给 `ExecCommand`——**宁可能力少一点，不要出现"号称只读却能写"的定义**。

#### 4.4.5 定义如何驱动子的构建

```
SubAgentSpec.agent_type
        │
        ▼
  definitions.resolve(name)  ──► AgentDefinition
        │
        ├──► §6.3 child_policy(parent, def.allowed_tools) 再减 def.denied_tools
        │         └─► §6.1 project_tools(...)              工具集
        ├──► §6.2 build_child_prompt(policy, …, omit_project_rules)
        │         └─► def.system_prompt 若有则覆盖          提示词
        ├──► def.model / effort / temperature               provider 参数
        └──► def.max_turns / max_tokens（spec 显式值优先）   预算
```

**优先级链**（高到低）：`SubAgentSpec` 显式字段 > `AgentDefinition` > `subagent` 配置默认值 > 父继承值。

**不可扩权不变量**：`def.allowed_tools` 进入 `child_policy` 时走的是**交集**，因此任何定义都无法拿到父没有的工具。这条必须有测试固化——预制定义是随版本分发的，一旦能扩权就是提权漏洞。

#### 4.4.6 hidden agent：收编内部 LLM 调用

opencode 把**生成标题、压缩上下文、生成摘要**三个纯内部的 LLM 调用也建模成 agent（`hidden: true` + `"*": "deny"`，`title` 还单设 `temperature: 0.5`），使模型、温度、提示词、权限走同一套配置面。

agentrs 现有对应物是散落的：`summarizer.rs`（压缩摘要）、`ProviderSummarizer`（WebFetch 提炼）。

**收编路径**（阶段 4 之后，非紧急）：把它们改为 `hidden` 定义，统一享有模型选择、温度、预算与可观测。收益不在功能而在**一致性**——现在这两处的模型选择逻辑各写各的。

#### 4.4.7 `whenToUse` 如何注入父提示词

在 `SystemPromptCache` 中新增 `subagents` 段，与 `skills` 段同构：

- 仅当父的 `ToolPolicy` 允许 `Spawn` 时渲染
- 过滤 `hidden = true` 的定义
- 每项渲染为 `- name: when_to_use`
- 该段随 `ToolPolicy` 变化 invalidate（沿用现有机制）

同时 `Spawn` 的 `input_schema` 增加 `agent_type` 字段，`description` 指向系统提示词里的清单——与 `Skill` 工具"名字在提示词里、工具描述只说怎么用"的现有约定一致。

#### 4.4.8 阶段依赖

**预制 agent 必须排在阶段 2（提示词按能力重建）与阶段 5（工具集投影）之后。** 在此之前：

- `denied_tools` 无处生效（工具集是硬编码的）
- 每个 agent 的独立提示词无法构建（子还在继承父的原文）

提前做只会得到一堆不起作用的配置。

---

## 五、生命周期

### 5.1 状态机

```
                 register
                    │
                    ▼
                 Pending ──────cancel──────┐
                    │                      │
              acquire permit               │
                    ▼                      ▼
   ┌──────────── Running ───cancel───▶ Cancelled
   │                │
   │           turn 结束
   │                │
   │       persistent?─── no ──▶ Finished / Failed
   │                │
   │               yes
   │                ▼
   └──inbox 收件── Idle ────cancel───▶ Cancelled
                    │
              shutdown 或 TeamDelete
                    ▼
                 Finished
```

**不变量**：

1. `Cancelled` 与 `Finished` / `Failed` 互斥且终态
2. 进入终态后 `join` 必被 await 或 abort，不得泄漏（对应 Team 方案的 R4）
3. `Idle` 仅对 `persistent = true` 的子存在

### 5.2 取消的时间界

**取消在一个轮次边界内生效**，具体是三个检查点：

| 检查点 | 位置 | 作用 |
|---|---|---|
| C1 | 子引擎 turn loop 顶部 | 未开始下一轮即退出 |
| C2 | 工具执行层（已有 `turn_cancel` 透传） | 长工具协作式中断 |
| C3 | provider 流式读取 | 断开正在进行的请求 |

C2/C3 的机制已存在（`execute_tool_calls_with_output_limit` 收 `&CancellationToken`，`Tool::execute_cancellable`），本设计只需把父的 token **接到子引擎的 `turn_cancel` 上**，并在 C1 加判断。

**不做强杀**：`JoinHandle::abort` 仅作为超时兜底（默认 5s），正常路径走协作式取消，以保证会话状态一致落盘。

### 5.3 父 turn 与子生命周期的时序

```
父 turn 开始
  │
  ├─ LLM 返回 tool_use(Spawn)
  │    │  Spawn 的 is_concurrency_safe = false ⇒ 自成串行批次
  │    ├─ 子 A ─────────┐
  │    ├─ 子 B ─────────┤ 并发（受 max_concurrent 闸门）
  │    └─ 子 C ─────────┤
  │                     ▼ 全部终态后按请求顺序拼接
  │    tool_result = <subagent>…</subagent> × 3
  │
  ├─ 下一轮 LLM 调用（结果已进父上下文）
  │
父 turn 结束
  │
  ├─ 一次性子：必为终态，已回收
  └─ 常驻子（Team）：保持 Idle，跨 turn 存活
```

**父子的 turn 是嵌套关系，不是对等并行**：子的整个生命周期（可能几十轮）内嵌在父的**一次工具调用**里。这意味着父在等待期间不做任何事——这是当前同步形态的固有代价，也是未来 async 形态要解决的问题。

**常驻子的存活边界**：跨父 turn 存活，但不跨父会话。父会话结束 → 全部子（含 Idle）终止并清理。

### 5.4 边界情形

| 情形 | 语义 |
|---|---|
| 单个子失败 | **不中断父 turn**。该子渲染为 `status="error"`，兄弟结果照常返回（现状已如此，保留） |
| 全部子失败 | `Spawn` 的 `ToolResult.is_error = true`，父仍继续本轮（工具错误不终止 turn） |
| 父 autocompact 触发时有子在跑 | 子**不受影响**——它有独立上下文与独立引擎。但父压缩会折叠历史，`<subagent id>` 可能随之消失 ⇒ **子 id 必须记入会话 metadata，不能只存在于消息文本里**，否则压缩后无法续跑 |
| 子自身上下文超限 | 子独立触发自己的压缩，父无感知 |
| 子请求审批 | 同步子冒泡到父终端；排队由 `max_concurrent` 限流（§14 Q6） |
| 子修改了文件 | 父**不会收到通知**。缓解：提示词要求子在 `<summary>` 中列出改动路径（见 §6.5）。这是已知限制，不是本设计要解决的问题 |

---

## 六、子上下文构建

这是 G3 的核心，位于 `subagent/context.rs`。

### 6.0 父子上下文契约

**继承 / 重建 / 隔离**三分。任何新增的引擎状态都必须在此表中明确归类。

| 项 | 父 → 子 | 说明 |
|---|---|---|
| `cwd` | **继承** | 同一工程；隔离由未来的 worktree 选项提供 |
| `runtime_env` | **继承** | 子执行的命令需要同样的环境 |
| provider / model | **继承**（可被 `ForkOverrides.model` 覆盖） | |
| `ToolPolicy` | **继承并取交集** | 永不扩权，见 §6.3 |
| `Config`（compact / todo / shell / file_cache） | **继承** | 行为一致性 |
| 预算（turns / tokens） | **重建** | 取 `spec` 或子专属配置，非父的值 |
| system prompt | **重建** | 见 §6.2 |
| **对话历史** | **不继承** | 子从干净上下文起步。这是与 claude-code fork 的根本区别——本设计**不做** fork 式上下文继承，理由见评估文档 §5.1 |
| 工具集 | **投影** | 见 §6.1 |
| MCP 连接 | **共享实例** | 连接昂贵，按策略过滤可见性而非重连 |
| hooks | **部分继承** | `PreToolUse` / `PostToolUse` 继承（安全策略应贯穿）；`UserPromptSubmit` / `Stop` 不继承（子无人类输入）。见 §14 Q5 |
| 审批模式 | **继承** | 同步子冒泡到父终端；现状无条件 `auto_approve = true` 应删除。见 §14 Q6 |
| 会话 | **新建并挂血缘** | `forked_from = 父 id`，见 §6.4 |
| 任务图（graph 模式） | **共享文件** | 现有设计，保留 |
| todo 清单（list 模式） | **隔离** | 现有设计，保留 |
| OutputSink | **替换** | `SubAgentSink`，见 §9.2 |
| 斜杠命令 | **不提供** | 子的输入来自父 |

反向（子 → 父）：

| 产出 | 去向 | 是否进父上下文 |
|---|---|---|
| `<subagent>` 文本 | `ToolResult` | ✅ 经 `record_tool_context_estimate` 计入 |
| `usage` | `total_usage` | ❌ 不占父上下文窗口（§9.1） |
| 状态变更 | 协议事件 / TUI | ❌ |
| 任务图变更 | 共享文件 | ❌ 父需主动 `TaskList` 才看见 |
| 文件系统变更 | 直接生效 | ❌ 无通知机制（§5.4） |

### 6.1 工具集：从父注册表投影，不再硬编码

```rust
/// 子智能体永不继承的工具，附排除理由。
///
/// 显式列表优于隐式白名单：新增工具时默认进入子的工具集，
/// 只有在此登记才被排除，避免"忘记同步"这一类缺陷。
const NEVER_INHERITED: &[(&str, &str)] = &[
    ("Spawn",      "递归派生由 depth 配置控制，不通过工具表隐式禁止"),
    ("EnterPlanMode", "计划模式属父会话的交互状态"),
    ("ExitPlanMode",  "同上"),
];

/// 按父注册表与子策略投影出子的工具集。
pub(crate) fn project_tools(
    parent: &ToolRegistry,
    policy: &ToolPolicy,
    depth: usize,
    config: &SubAgentConfig,
) -> ToolRegistry;
```

规则，按序：

1. 从**父注册表**取全部工具（含 MCP、Web、ViewImage、ToolSearch —— 现状拿不到）
2. 剔除 `NEVER_INHERITED`
3. `depth + 1 < config.depth` 时**重新加入** `Spawn`（深度由配置裁决，非结构性禁止）
4. 按子的 `ToolPolicy` 过滤（该策略已是父策略的子集，见 §6.3）
5. 按 `todo.mode` 追加追踪工具（沿用现有 `build_task_tracking` 逻辑）

> **工具实例的共享问题**：`ToolRegistry` 持 `Box<dyn Tool>`，无法直接克隆到子注册表。落地时需将注册表改为持 `Arc<dyn Tool>`，或为可共享工具提供 `fn clone_for_child(&self) -> Option<Box<dyn Tool>>`。**这是本设计里工作量最大的单点，见 §14 Q1。**

### 6.2 提示词：按子的真实能力重建

```rust
pub(crate) fn build_child_prompt(
    policy: &ToolPolicy,
    config: &Config,
    workspace: &Path,
    shell: &ResolvedShell,
    identity: Option<&SubAgentIdentity>,   // Team 队员：名字、队友列表
) -> String;
```

- 复用 `build_system_prompt_with_shell_and_tool_policy` —— 它**已接受 `tool_policy` 并按其过滤 tool guidance 与 skills 段**，且策略变化会主动 invalidate 缓存
- 传 `skills = &[]`：子无 `Skill` 工具，技能清单是纯浪费（实测：10 个 skill 占 1663 B ≈ 420 token）
- 保留 workspace / shell / AGENTS.md / memory 段 —— 子仍在同一工程里工作
- `identity` 为 `Some` 时追加身份段（"你是队员 X，队友有 Y、Z，用 SendMessage 与他们通信"）——Team 前置，当前恒为 `None`

**优先级**：`spec.system_prompt` > 重建结果。显式覆盖始终最高。

### 6.3 策略：交集语义不变

```rust
pub(crate) fn child_policy(parent: &ToolPolicy, requested: &[String]) -> ToolPolicy
```

保持现有 `effective_child_tool_policy` 的语义——请求为空则继承父策略，否则取交集。**这条规则是现有代码里最正确的部分，不动。**

### 6.4 会话：子会话挂在父的血缘树上

- 子会话通过 `SessionManager::fork_from(parent_session, Some(child_id), ForkBoundary::…)` 创建？**否**——fork 会复制父的全部历史，而子应从干净上下文起步
- 正确做法：`create()` 新会话后填 `forked_from = Some(parent_id)`、`root_id = 父的 root_id 或父 id`
- 语义澄清：`forked_from` 当前注释为"fork 树中的直接父"，子智能体复用该字段后应改述为"**血缘父**：fork 或子智能体派生"，并在注释中说明两种来源

> 需补一个反向索引 `children(parent_id)`——现状 `forked_from` 仅作血缘记录，"never followed at load time"。级联清理依赖它。

### 6.5 子身份段：让子知道自己在跟谁说话

**当前一个未被记录的缺陷**：子继承父的提示词，因此它以为自己在直接面对人类用户。它不知道自己的最终文本会被父当作返回值读取，也不知道自己的中间输出无人可见。

claude-code 与 opencode 都在提示词层解决了这件事（前者用 agent 定义的 system prompt，后者用 `task.txt` 第 3-4 条要求父在 prompt 里说清）。本设计把它做成**子提示词的固定段落**：

```
你是一个子智能体，由主智能体派生来完成一项特定任务。

- 你的**最后一条文本消息即返回值**，会被主智能体读取；它不会展示给用户。
- 你的中间输出、工具调用过程对任何人都不可见——不要写给用户看的过渡语。
- 你无法向主智能体提问；遇到歧义按最合理的假设推进，并在结论中说明该假设。
- 若你修改了文件，必须在结论开头列出改动路径——主智能体不会自动感知。
- 结论应自包含：主智能体没有你的上下文，只有你返回的这段文本。
```

最后两条直接对应 §5.4 的两个已知限制（文件变更无通知、上下文不共享），用提示词兜住了机制上不打算解决的部分。

Team 队员的身份段在此基础上追加队名、队友列表与 `SendMessage` 用法（`SubAgentIdentity` 参数，当前恒为 `None`）。

---

## 七、通信设计

### 7.0 通信拓扑

四条通道，方向与时机各不相同：

| # | 方向 | 载体 | 时机 | 当前阶段 |
|---|---|---|---|---|
| ① | 父 → 子 | `SubAgentSpec` | 派生时 | ✅ 落地 |
| ② | 子 → 父 | `<subagent>` XML（经 `ToolResult`） | 子终态时 | ✅ 落地 |
| ③ | 子 → 宿主 | 协议事件 | 运行中持续 | ✅ 落地 |
| ④ | 父 ↔ 子 | inbox 消息 | 子的每个工具轮边界 | ⏸ 预留（Team） |

**没有的通道**（明确不做）：兄弟间直连（②的汇总由父完成）、子向父提问（子不可阻塞等待父，会死锁——父正阻塞在子上）。

> 第二条是硬约束而非取舍：当前形态下父在 `Spawn` 工具调用里同步阻塞，若子反过来等父应答，双方互锁。这也是 §6.5 提示词里"你无法向主智能体提问"的机制依据。

### 7.1 下行（父 → 子）

结构化 `SubAgentSpec`，见 §4.1。相对现状新增 `depth` / `resume` / `persistent`。

### 7.2 上行（子 → 父）：结构化，可解析

```xml
<subagent id="{id}" name="{name}" status="completed|error|cancelled">
  <summary>{一行结论}</summary>
  <result>{子的最终文本}</result>
  <usage turns="{n}" input="{n}" output="{n}" />
</subagent>
```

多个子的结果按**请求顺序**拼接（现状已保证，保留）。

设计要点：

- 选 XML 而非 JSON：与 `<system-reminder>` 等既有注入格式一致，且模型对标签边界的鲁棒性优于 JSON 转义
- `id` 必须回传 —— 它是续跑（`spec.resume`）与取消（`TaskStop` 类工具）的唯一入口
- 截断发生在 `<result>` **内部**，不得破坏标签结构。现状的头尾截断作用于整个拼接串，会产出半个标签

### 7.3 运行中（收件）：复用 `follow_up_blocks` 通道

**不新造机制。** 子引擎的 turn loop 中已有：

```rust
if !follow_up_blocks.is_empty() {
    self.push_history(Role::User, follow_up_blocks);
}
```

这正是"工具执行后追加一条 user 消息"的既有通道（`ViewImage` 用它回传图像块）。收件注入挂在同一点：

```
工具执行完成
  → drain inbox
  → 若非空，包装为 <subagent-message from="..."> 文本块
  → 并入 follow_up_blocks
  → push_history(Role::User, ...)
```

收益：与 Team 方案要求的"每轮工具调用边界 drain 一次"天然对齐，零新增机制，且注入内容自动进入上下文统计（`record_tool_context_estimate`）。

**当前阶段**：`inbox` 恒为 `None`，drain 是空操作。检查点先落地，Team 到来时只需接上发送端。

---

## 八、取消与清理语义

| 触发源 | 行为 |
|---|---|
| 用户中断父 turn | `registry.cancel_all()` → 全部子进 `Cancelled`，各自落盘后回收 |
| 父 turn 正常结束 | 一次性子必已终态；常驻子保持 `Idle` |
| `TaskStop`（未来）指定 id | `registry.cancel(id)` |
| 父会话删除 | 递归取消并删除全部子会话（依赖 §6.4 的反向索引） |
| 子自身超预算 / 超轮次 | 子引擎自行终止，状态 `Failed`，原因写入 `text` |

**清理不变量**：任何路径下 `SubAgentHandle` 都不得在未 await/abort `join` 的情况下被丢弃。落地时应为 `SubAgentRegistry` 实现 `Drop` 兜底并记 `warn` 日志——泄漏是 Team 方案 R4 标注的高影响风险。

---

## 九、用量与可观测

### 9.1 成本口径与上下文口径必须分开

**这是一处容易做错的地方**：

| 口径 | 子的 token 是否计入父 | 理由 |
|---|---|---|
| **成本**（`Session.total_usage` / `engine.total_usage`） | ✅ **计入** | 子消耗的是同一份配额，不计则会话累计用量少算 |
| **上下文**（`context_state.context_usage`） | ❌ **不计入** | 子的 token 不占父的上下文窗口；父只因 `<subagent>` 返回文本增长，而该部分**已由 `record_tool_context_estimate` 计入** |

误把子用量灌进上下文口径，会导致自动压缩被过早触发。实现上应新增 `record_subagent_usage(&TokenUsage)`，只累加 `total_usage`，不碰 `context_state`。

### 9.2 可观测

- 子输出 sink 由 `NullSink` 换为 `SubAgentSink`：**只转发进度类事件**（状态变更、轮次推进、token 增量、当前工具名），不转发子的完整流式文本，避免刷屏父输出
- 日志按 `AGENTS.md`：`info` 记生命周期边界（派生 / 终态 / 取消），`debug` 记轮次推进；**不得记 prompt、工具输入输出、消息正文**

---

## 十、并发与预算控制

新增 `agentrs-config/src/subagent.rs`：

| 字段 | 默认 | 语义 |
|---|---|---|
| `enabled` | `true` | 关闭时不注册 `Spawn` |
| `max_per_call` | 5 | 单次 `Spawn` 调用的子数上限 |
| `max_concurrent` | 5 | workspace 级并发上限（`Semaphore`） |
| `max_turns` | 200 | 单子轮次上限 |
| `max_tokens` | 4096 | 单子单次响应输出上限 |
| `depth` | 1 | 最大嵌套深度；`>1` 时子的工具集才含 `Spawn` |
| `turn_output_budget` | `None` | 单个父 turn 内所有子的输出 token 总预算 |
| `cancel_grace` | 5s | 协作式取消的等待上限，超时 `abort` |

**超限行为**：一律返回**可操作的错误文本**（说明超了哪项、如何调整），不静默截断、不 panic。这一点对齐 opencode 的做法（其深度超限提示直接告诉模型改 `subagent_depth`）。

---

## 十一、协议事件扩展

`agentrs-protocol` 新增（供 TUI 与 JSON stream 宿主渲染）：

```rust
SubAgentStarted   { id, name, parent_msg_id, depth }
SubAgentProgress  { id, status, turns, usage }
SubAgentFinished  { id, status, usage, turns }
```

设计要点：

- 与既有 `ToolRunning` / `ToolResult` **并存而非替代**——`Spawn` 本身仍是一次工具调用，子事件是其内部细节
- 事件里**不含** prompt 与结果正文，只含标识与计量；正文经 `ToolResult` 走既有通道
- Team 到来时追加 `TeamEvent`，与本组事件并列，不复用

---

## 十二、与 Team 的接缝

本设计为 Team 预留三个接缝，**当前不实现**：

| 接缝 | 预留形式 | Team 落地时补什么 |
|---|---|---|
| 常驻子 | `SubAgentSpec.persistent` + `SubAgentStatus::Idle` | 首轮结束后进 `Idle` 而非 `Finished` 的分支 |
| 收件 | `SubAgentHandle.inbox: Option<Sender>` + turn 边界 drain（空实现） | 接上发送端与 `<subagent-message>` 包装 |
| 身份 | `build_child_prompt(identity: Option<&SubAgentIdentity>)` | 传入队名/队友列表 |

**关键约束**：Team 的队员必须由**同一个 `SubAgentRegistry`** 管理。若 Team 另起一套注册表，将出现两套取消路径、两套用量归集、两套泄漏风险——这正是 Team 方案 R4 想避免的。

对应地，Team 的 `SendMessage` 工具必须能进入子的工具集（§6.1 的投影规则天然支持：它在父注册表里、不在 `NEVER_INHERITED` 里、策略允许即可继承）。

---

## 十三、兼容性与迁移

| 变更 | 影响面 | 处理 |
|---|---|---|
| `SubAgentConfig` → `SubAgentSpec` | `spawn_tool.rs`、`agentrs-skills::executor` | 一次性改完，不留兼容垫片（该类型只有两个调用方） |
| `is_error: bool` → `status` | 同上 + 结果渲染 | 同上 |
| `Spawner::spawn_fork` → `spawn`（+ cancel 参数） | `agentrs-skills::executor::execute_fork` | 同上 |
| `ToolRegistry` 持 `Arc<dyn Tool>` | 全部工具注册点 | 机械改动，但面广；见 §14 Q1 |
| `Session.forked_from` 语义扩展 | 会话加载、fork 逻辑、TUI 展示 | 改注释 + 补 `children()` 反向索引；旧会话文件天然兼容（字段已存在且 `Option`） |
| 子提示词不再继承父 | 子的行为 | 需回归测试确认环境上下文（workspace/shell/AGENTS.md）未丢失 |

**旧会话兼容**：`forked_from` / `root_id` 已是 `Option` 且带 `serde(default)`，新增子会话不破坏既有文件格式。

---

## 十四、待决问题与对标答案

六个问题中，**五个在 claude-code / opencode 里有直接答案**，逐条给出证据与结论。仅 Q3 需自行拍板。

### Q1｜工具实例如何向子共享？——**已解决：把状态移出工具**

**对标答案**（claude-code）：这个问题在它那里根本不存在，因为**工具是无状态的，状态挂在上下文对象上**。

```ts
// Tool.ts:181 —— 文件读取状态属于 ToolUseContext，不属于 ReadTool
readFileState: FileStateCache

// runAgent.ts:375 —— 子如何取得它，按形态分
const agentReadFileState = forkContextMessages !== undefined
    ? cloneFileStateCache(toolUseContext.readFileState)      // fork 子：继承父读过什么
    : createFileStateCacheWithSizeLimit(READ_FILE_STATE_CACHE_SIZE)  // 普通子：干净起步

// runAgent.ts:828 —— 子结束时释放
agentToolUseContext.readFileState.clear()
```

**结论**：§14 原先的 A/B/C 三方案是伪选择。真正该做的是**把 `ReadTool` 的文件缓存从工具实例提到一个显式的 `ToolContext`（每个引擎一份）**，工具无状态后共享就不再是问题——`Arc` 还是 `Box` 退化为纯实现细节。

**附带解决了一个此前未意识到的语义问题**：子该不该继承父的"已读文件"记录？claude-code 的答案是**按形态分**——fork 子继承（上下文连续），普通子不继承（干净起步）。agentrs 的 Spawn 子不继承对话历史，因此也**不应**继承文件读取状态，与 §6.0 的"对话历史不继承"保持一致。

`ExecCommandTool` 的进程管理同理：若确有跨调用状态，提到 `ToolContext`；若无，直接共享。

### Q2｜子会话是否进 `/resume` 列表？——**已解决：落库但默认过滤**

**对标答案**（opencode）：不加类型字段，直接用**父指针是否为空**做过滤条件。

```ts
// session/session.ts:560
if (input?.roots) conditions.push(isNull(SessionTable.parent_id))

// cli/cmd/session.ts:87 —— 会话选择器只要根会话
svc.list({ roots: true, limit: args.maxCount })

// server/routes/.../session.ts:70 —— HTTP API 把 roots 暴露为查询参数，调用方自决
roots: ctx.query.roots
```

**结论**：优于我原先提议的 `SessionMeta.kind` 字段——**无需新字段、无需迁移**。agentrs 已有 `forked_from`，`/resume` 与会话列表默认加 `forked_from IS NONE` 过滤，同时把"是否包含子会话"作为参数暴露给协议宿主。

### Q3｜一次性子的会话是否默认落盘？——**需拍板，但两个对标都倾向持久化**

- **opencode**：无条件落 SQLite，无开关
- **claude-code**：**按需**——仅当 UI 持有该任务（`t.retain`）时实时追加写盘，注释说明 bootstrap 会并行读盘并按 UUID 合并前缀，保证"live 始终是 disk 的后缀"（`agentToolUtils.ts:557`）

**倾向**（维持不变，但吸收 claude-code 的分级）：默认落盘，配置项 `subagent.persist_sessions`（默认 `true`）；**实时写盘仅对被协议/TUI 持有的子**，其余在终态一次性落盘以降低 IO。淘汰复用现有 `max_sessions`。

### Q4｜`depth > 1` 时孙辈取消是否级联？——**已解决：两个对标都级联**

- claude-code：同步子**共享父的 `abortController`**（`runAgent.ts:522`），级联是天然的；异步子才新建独立 controller
- opencode：`Session.remove` 递归删子会话并 `cancelBackgroundJobs`；`Effect.acquireUseRelease` 在 `Exit.hasInterrupts` 时同时取消会话与后台作业

**结论**：级联。用 `CancellationToken::child_token()` 实现，与 claude-code 的"共享 controller"语义等价。在测试中固化。

### Q5｜父的 hooks 在子里生效到什么程度？——**已解决**

**对标答案**（claude-code）：

1. `SubagentStart` / `SubagentStop` 是**一等 hook 事件**（`types/hooks.ts:97`、SDK schema 均有定义），确认需要新增
2. agent 定义可带自己的 hooks，注册时**作用域限定在该 agent 的生命周期**，且传 `isAgent = true` 会把该 agent 的 `Stop` hook **自动转换为 `SubagentStop`**（`runAgent.ts:556-573`）——agent 作者写 `Stop` 语义上就是"我结束时"，由框架映射到正确事件，避免概念负担
3. 有信任门控：`isRestrictedToPluginOnly('hooks')` 时，仅 admin-trusted 来源的 agent 其 frontmatter hooks 才被注册

**结论**：

| 项 | 决定 |
|---|---|
| `PreToolUse` / `PostToolUse` | 继承——安全策略必须贯穿子 |
| `UserPromptSubmit` / `Stop` | 不继承——子无人类输入 |
| 新增 `SubagentStart` / `SubagentStop` | **做**。前者是"给子注入额外上下文"的唯一挂载点，后者是子结束后清理的挂载点 |
| 子自带 hooks 的 `Stop` | 自动映射为 `SubagentStop` |
| 子里 hook 失败 | 降级为该子的 `Failed`，不影响父 |

### Q6｜子的危险操作是否冒泡审批？——**已解决，且我原先的倾向是错的**

**对标答案**（claude-code）：判据不是"工具类目"，而是**"这个子能不能弹窗"**。

```ts
// runAgent.ts:440
const shouldAvoidPrompts =
  canShowPermissionPrompts !== undefined ? !canShowPermissionPrompts
  : agentPermissionMode === 'bubble' ? false      // bubble：总是弹
  : isAsync                                        // 默认：同步弹，异步不弹
```

配套的三条规则：

| 规则 | 代码位置 | 含义 |
|---|---|---|
| 无法弹窗 ⇒ `shouldAvoidPermissionPrompts = true` | `runAgent.ts:446` | **自动拒绝**，而非自动批准 |
| 异步但允许弹窗 ⇒ `awaitAutomatedChecksBeforeDialog = true` | `runAgent.ts:459` | 先跑分类器与权限 hook，只有自动检查裁决不了才打扰用户 |
| 父模式更宽松时父优先 | `runAgent.ts:415-434` | agent 自带的 permissionMode 不覆盖父的 `bypassPermissions` / `acceptEdits` / `auto` |

opencode 侧同向：`ctx.ask()` 按 sessionID 发权限事件，**子会话的请求照常送达客户端**，并非自动放行。

**我原先倾向的方案 C（按 `Exec`/`Edit`/`Info` 类目分级）在两个实现里都不存在，应当放弃。**

**更关键的发现**：agentrs 现状是 `config.tools.auto_approve = true`（自动**批准**），而 claude-code 对无法弹窗的子是自动**拒绝**——**方向相反**。而 agentrs 当前的子全部是同步的（父阻塞等待，用户注意力恰在此处），按 claude-code 的规则本就**应该冒泡**。

**结论**：

| 子的形态 | 审批行为 |
|---|---|
| 同步子（当前唯一形态） | **冒泡到父终端**，复用父的 confirmer / approval manager |
| 异步子（未来） | 无法弹窗 ⇒ **自动拒绝**危险操作，并在结果中说明被拒的操作 |
| 父处于 `auto_approve` / 免审批模式 | 父优先，子随父 |
| 并发排队问题 | 由 §10 的 `max_concurrent` 天然限流；后续可引入"先跑自动检查、裁决不了再问"以降低打扰频率 |

这条同时改写 §6.0 上下文契约表中"审批模式：待决"一行为**继承父的审批模式**，并使 `spawn_one` 里那句无条件的 `config.tools.auto_approve = true` 成为需要删除的代码。

---

## 附：与改造阶段的对应

| 本设计章节 | 指引文档阶段 |
|---|---|
| §4.1 契约、§5 生命周期、§8 取消 | 阶段 1（取消传播与可寻址） |
| §6.2 提示词、§9 用量 | 阶段 2 |
| §10 并发与预算 | 阶段 3 |
| §6.4 会话、§7.2 上行格式、§11 协议事件 | 阶段 4 |
| §6.1 工具投影（Q1） | 阶段 5 |
| §4.4 Agent 定义与预制 | 阶段 5 之后（依赖提示词重建与工具投影） |
| §12 Team 接缝 | Team 线（阶段 4 之后） |

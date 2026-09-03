# agentrs-memory 能力补齐执行方案

> 版本：v1.0 · 日期：2026-09-03
> 目标：按 `opensource/claude-code-main/src/memdir` 与相关服务的源码，补齐 agentrs 记忆系统缺失的模块
> 关联文档：[技术架构设计文档](./agentrs-memory-技术架构设计文档.md) · [横向技术对比报告](./agentrs-memory-横向技术对比报告.md)
> 适用规范：`agentrs/AGENTS.md`（分层、可见性、测试组织、跨平台、日志）

---

## 0. 预期效果与代价（先读这一节）

本节澄清"做完之后相对现状到底变了什么"，以及必须一并接受的代价。**如果只读一节，读这节。**

### 0.1 基线：现状实际能做到什么

不能说记忆系统"完全没用"。准确描述是：**它目前是一个只读的、需要人工维护的索引注入器。**

| 场景 | 现在实际发生什么 |
|---|---|
| 用户手工写好 `MEMORY.md` 和主题文件 | ✅ 会话启动时索引进入 system prompt，模型能看到"有哪些记忆"并主动 `Read` 展开 |
| 用户说"记住我偏好 X" | ⚠️ 模型会写文件，但只有 188 token 的精简规则——无文件命名规范、无 frontmatter 格式示例、无两步保存流程，产出格式大概率不一致 |
| 写完后本轮继续对话 | ❌ 新记忆不在上下文里，模型可能重复写、或以为没保存成功 |
| 下次会话启动 | ✅ 若索引行写对了，能看到 |
| 记忆超过 200 行 / 25KB | ❌ 静默截断，无任何降级检索手段 |
| 记忆写于三个月前、引用的函数已删除 | ❌ 无任何逐条陈旧性提示 |
| 中文路径项目 | ❌ `/home/张三/proj` 与 `/home/李四/proj` 共用同一记忆目录 |
| 用户想看 / 删记忆 | ❌ 无命令，只能自己翻目录 |

**一句话基线**：能用，但只在"用户愿意手工维护记忆文件"的前提下能用。自动积累这条路基本是断的。

### 0.2 每个迭代买到什么

**迭代 1（M0 + M1 + M2 + M6-4，约 1.5 周）——从半通到自洽**

| 行为 | 现状 | 之后 |
|---|---|---|
| 本轮保存的记忆 | 下次重启才可见 | **当轮下一次请求就可见** |
| 模型看到的保存指令 | 188 token 精简版 | 首次写入后升级为 ~3,100 token 完整版（4 类语义 + 8 组示例 + 格式规范） |
| 记忆目录 | 提示词声称"已存在"但可能不存在 | 启动时确保存在，断言成真 |
| 每条记忆的年龄 | 无 | 附 "47 days ago" + 陈旧性告警 |
| 中文路径 | 可能串项目 | 目录名带哈希，唯一 |
| 并发写索引 | 会丢条目 | 原子替换 |
| 记忆是否被用到 | 完全不知道 | 结构化日志可回答 |

> 这一步的性质是**修复而非增强**——它让已经写好的 4,200 行代码真正开始工作。

**迭代 2（M3 + M5，约 1 周）——解除容量天花板**

| 行为 | 现状 | 之后 |
|---|---|---|
| 记忆规模上限 | 200 行 / 25KB 硬顶，超出静默丢失 | 索引仍常驻，超出部分由按需召回补上，文件数上限升至 200 |
| 记忆进入上下文的方式 | 全量索引，与本轮问题无关 | 索引 + **本轮相关的 Top-5 全文** |
| 写记忆文件 | 触发审批提示 | 默认路径免打扰（自定义路径仍走审批） |

> 这是**唯一**能让记忆规模继续增长的路径。没有它，记忆系统在几十条之后就到顶。

**迭代 3（M4 + M6 其余，约 1.5 周）——从被动到主动、从黑盒到可控**

| 行为 | 现状 | 之后 |
|---|---|---|
| 记忆从哪来 | 只有用户明说"记住"时才产生 | 每轮结束后台自动抽取（**默认关闭**） |
| 用户能否审查 | 手工翻目录 | `/memory` 列表 / `show` / `rm` |
| 能否关闭 | 只能靠环境变量绕开 | `[memory] enabled = false` |

### 0.3 代价（必须一并接受）

| 项 | 增量 | 说明 |
|---|---|---|
| **首次写入后每轮 token** | **+约 2,900** | 精简档 188 → 完整档 ~3,100（按 `prompt.rs` 各常量实测字符数换算）。会话内一次性升级，之后每轮都带 |
| **迭代 2 后首 token 延迟** | **+0.5～2 秒** | 召回是一次小模型往返，必须在构建主请求前收敛。异步预取只能让它与其他准备工作并行，**不能消除**。3s 超时是上限保护，不是零成本 |
| **迭代 2 后每轮调用数** | +1 次小模型调用 | 记忆目录为空时不发起 |
| **迭代 3 开启后每轮成本** | **接近翻倍** | 一次完整子 agent 调用——这正是它默认关闭的原因 |
| 代码量 | +约 1,200 行生产代码（含测试约 +3,000 行） | |

诚实判断：**迭代 1 的代价接近于零**（只有模型真的开始写记忆后才涨 token）；**迭代 2 的延迟代价每轮可感知**；**迭代 3 的成本代价大到必须默认关闭**。

### 0.4 本方案承诺不了什么

1. **不承诺"记忆质量变好"。** 补齐的是管道。写什么、写得准不准取决于模型与提示词；完整档能改善格式一致性，但"这条值不值得存"仍是模型判断。
2. **不承诺数字化的效果提升。** 当前无任何遥测，**没有基线就无法给出"命中率提升 X%"这类指标**——任何这样的数字都是编的。这也是 0.5 节把遥测提前的原因。
3. **不承诺记忆不会误导。** 陈旧性标注降低风险但不消除；`age.rs` 只声明"这条 47 天了"，不验证内容对错。
4. **迭代 3 引入真实的隐私变化**：会话内容会被另一个模型读取并**持久化落盘**。内容本来就发给了模型，但"落盘保存"是新增行为，必须显式告知用户。

### 0.5 排期修正：遥测必须提前

初版方案把遥测排在 M6（最后），却又主张"基于遥测数据决定要不要投入 M4"——这是自相矛盾的：等 M6 做完，M4 已经做了。

**修正：M6-4 基础遥测提前到迭代 1**（约 0.5 人日），使迭代 3 成为数据驱动的决策而非预设动作。

### 0.6 最小验证路径

若希望先验证再投入，最省的做法是**只做迭代 1**（含遥测，约 1.5 周），然后观察两周：

| 观察结果 | 结论 |
|---|---|
| 记忆文件基本没被 `Read` 过 | 当前记忆内容对模型无价值。做 M3/M4 是放大一个没人用的功能，应考虑收缩（pi / opencode 路线） |
| 读取频繁，且索引已接近 200 行 | M3 相关性召回是明确刚需 |
| 读取频繁，但记忆条目常年个位数（用户懒得手工存） | M4 自动抽取是明确刚需 |
| 读取频繁且两者皆是 | 按原计划推进迭代 2、3 |

---

## 1. 背景与目标

### 1.1 现状

agentrs 的记忆系统在生产链路上只做了一件事：启动时读 `MEMORY.md`、截断、拼进 system prompt。

```
bootstrap.rs:214   auto_memory_dir(workspace)
context.rs:288     build_memory_prompt_minimal(dir) → read_index() → truncate_index()
```

`store` / `paths` 中的绝大部分 API 与 `prompt` 的完整档均为零生产调用的死代码。对比 claude-code 的 ~5720 行实现，缺失的是**让该范式成立的全部闭环**。

### 1.2 目标

按 claude-code 源码引入以下能力，使 agentrs 记忆系统形成"写入 → 索引 → 召回 → 使用 → 失效"的完整回路：

| 编号 | 能力 | claude-code 源模块 | 优先级 |
|---|---|---|---|
| C1 | 写入感知与提示词升级 | `memdir.ts:419 loadMemoryPrompt` + `paths.ts:274 isAutoMemPath` | P0 |
| C2 | 记忆新鲜度标注 | `memdir/memoryAge.ts` | P0 |
| C3 | 相关性召回 | `memdir/findRelevantMemories.ts` | P1 |
| C4 | 后台自动抽取 | `services/extractMemories/extractMemories.ts` | P1 |
| C5 | 权限边界 carve-out | `utils/permissions/filesystem.ts:1572,1716` | P1 |
| C6 | 过去上下文检索指引 | `memdir.ts:375 buildSearchingPastContextSection` | P2 |
| C7 | 开关、命令、遥测 | `paths.ts:30` + `commands/memory` + `memoryShapeTelemetry` | P2 |

### 1.3 不在本次范围（及理由）

| 不引入 | claude-code 模块 | 理由 |
|---|---|---|
| 团队记忆同步 | `teamMemorySync/` 1256 行 | 需要服务端 API 与 OAuth 体系，agentrs 无对应基础设施。若未来要做，opencode 的"远程指令 URL"方案成本低两个数量级，应优先评估 |
| 上传前密钥扫描 | `secretScanner.ts` 324 行 | 是团队同步的附属能力，无同步则无需求 |
| 会话内记忆 | `SessionMemory/` ~700 行 | 与 `agentrs-compact` 的上下文压缩职责重叠，需先做产品定位澄清 |
| 子 Agent 独立记忆 | `AgentTool/agentMemory.ts` | 依赖 C4 落地后才有意义，列为后续 |
| daily-log 模式 | `buildAssistantDailyLogPrompt` | claude-code 内部仍在 feature flag 灰度，范式未收敛 |

---

## 2. 总体设计

### 2.1 分层约束与关键决策

AGENTS.md 规定依赖严格向下，且 `agentrs-memory`、`agentrs-providers`、`agentrs-tools` **同处 Mid 层，不得互相依赖**。这决定了两个关键拆分：

**决策 1：召回选择器用 trait 拆分（照搬 `TextSummarizer` 先例）**

`summarizer.rs:25-27` 的注释已经确立了这个模式：

> "Lives in the agent crate so `agentrs-tools` can consume the capability without depending on `agentrs-providers`, which sits in the same layer."

因此：

- **trait 定义**放在 `agentrs-memory`（memory 与 agent 的最低公共 crate，符合"放在语义所属的最低 crate"）
- **LLM 实现**放在 `agentrs-agent`（可以依赖 `agentrs-providers`）

**决策 2：抽取 agent 只能放在 `agentrs-agent`**

它需要 `AgentSpawner`（`spawner.rs`）与 `ToolPolicy`（`tool_policy.rs`），二者都在 Top 层。

### 2.2 目标模块图

```
crates/agentrs-memory/                      [Mid]
  src/
    types.rs      (改) + MemoryAge 相关字段
    paths.rs      (改) sanitize_path 哈希修复
    index.rs      (改) 原子写、精确删除
    store.rs      (不变)
    prompt.rs     (改) 新增 searching-past-context 段
    age.rs        (新) ← memoryAge.ts               [C2]
    recall.rs     (新) trait MemorySelector + 候选过滤 ← findRelevantMemories.ts 前半 [C3]
    config.rs     (新) MemoryConfig                  [C7]

crates/agentrs-agent/                       [Top]
  src/
    memory/
      mod.rs      (新) 仅模块声明
      recall.rs   (新) ProviderMemorySelector ← findRelevantMemories.ts 后半 [C3]
      extract.rs  (新) 后台抽取 ← extractMemories.ts  [C4]
      watch.rs    (新) 写入感知与缓存失效 ← isAutoMemPath 消费侧 [C1][C5]
    commands/
      memory.rs   (新) /memory 命令 ← commands/memory [C7]
    context.rs    (改) 精简/完整档分派
    engine.rs     (改) 三处接线：召回预取、写入感知、stop 抽取
    bootstrap.rs  (改) ensure_memory_dir + 配置读取

crates/agentrs-config/                      [Mid]
  src/config.rs   (改) Config 增加 memory: MemoryConfig
```

### 2.3 数据流（目标态）

```
会话启动
  bootstrap.rs  ensure_memory_dir() ──► 目录确保存在（提示词断言成真）
                build_memory_prompt_minimal() ──► 精简档 + MEMORY.md

每轮用户输入（run_inner 入口，push_history 之前）
  memory/recall.rs  预取（异步、带超时）
      scan_memory_files → format_memory_manifest
      → MemorySelector::select()（侧查询小模型，JSON 输出，Top-5）
      → 过滤 already_surfaced
      → 逐条附加 age.rs 的新鲜度告警
      → 作为 <system-reminder> 块拼入本轮 user content

工具执行后（execute_tool_round 返回处）
  memory/watch.rs
      is_memory_path(写入路径) 命中？
        → cache.invalidate("memory") + 重建 system prompt（本轮生效）
        → 首次命中：memory section 由精简档升级为完整档
        → 标记 memory_written_this_turn = true（供抽取互斥）

query loop 结束（run_stop_hooks 附近）
  memory/extract.rs
      memory_written_this_turn ? 跳过（主 agent 已写，抽取冗余）
      : AgentSpawner 派生受限子 agent（ToolPolicy 白名单 + 记忆目录写限制）
        → 从本轮 transcript 抽取记忆 → 写盘 → invalidate("memory")
```

---

## 3. 分阶段执行计划

### M0 — 缺陷修复（前置，1 人日）

补齐能力之前必须先修掉会让新能力放大伤害的三个缺陷。

| 任务 | 文件 | 内容 |
|---|---|---|
| M0-1 | `paths.rs:148` | `sanitize_path` 无条件追加原始路径哈希后缀。当前只在 >200 字符时加哈希，导致 `/home/张三/proj` 与 `/home/李四/proj` 清洗同名、记忆串项目。C3/C4 会显著放大这个问题（自动写入的记忆污染更隐蔽） |
| M0-2 | `index.rs:131,158` | `append_index_entry` / `remove_index_entry` 改为写临时文件 + `fs::rename` 原子替换。C4 引入后台并发写，非原子写必然丢条目 |
| M0-3 | `index.rs:165` | `remove_index_entry` 改用行首锚定 + Markdown 链接解析，替代 `contains("({filename})")` 子串匹配 |

**验收**：新增 `paths_test.rs` 用例——两个仅中文段不同的路径必须产出不同目录名；新增 `index_test.rs` 并发用例——16 个线程并发 append 后条目数等于 16。

---

### M1 — C1 写入感知与提示词升级（P0，3 人日）

这是**改动量最小、收益最大**的一步：一处接线同时解决"完整提示词不可达"和"会话内索引不刷新"两个 P0 问题。

#### 借鉴源码

| claude-code | 借鉴内容 |
|---|---|
| `memdir/paths.ts:274 isAutoMemPath` | 判定路径是否属于记忆目录 |
| `memdir/memdir.ts:419 loadMemoryPrompt` | 按状态分派精简/完整档的装配骨架 |
| `memdir/memdir.ts:129 ensureMemoryDirExists` | 目录创建的责任归属：**builder 只读不 mkdir，由 loader 负责** |

#### 任务分解

**M1-1 目录创建接线**
`bootstrap.rs:214` 之后调用 `paths::ensure_memory_dir(&dir)`；失败仅 `warn!` 并把 `memory_dir` 置 `None`（整体降级关闭记忆），不中断启动。这让 `prompt.rs:24` 的 `DIR_EXISTS_GUIDANCE`（"This directory already exists"）从虚假断言变成事实。

**M1-2 提示词档位状态**
`context.rs` 的 `SystemPromptCache` 增加字段：

```rust
/// Whether the memory section has been upgraded to the full instruction set.
/// Set once the model first writes into the memory directory; the full
/// taxonomy is only needed for writing, not reading.
pub(crate) memory_full_instructions: bool,
```

memory section 装配改为：

```rust
if let Some(dir) = memory_dir {
    let full = cache.memory_full_instructions;
    let memory_section = cache.sections.entry("memory").or_insert_with(|| {
        if full { build_memory_prompt(dir) } else { build_memory_prompt_minimal(dir) }
    });
    ...
}
```

**M1-3 写入感知模块** — 新建 `crates/agentrs-agent/src/memory/watch.rs`

```rust
/// Outcome of inspecting one tool round for memory-directory writes.
pub(crate) struct MemoryWriteObservation {
    /// A Write/Edit call targeted a path inside the memory directory.
    pub(crate) wrote_memory: bool,
    /// This is the first such write in the session — the memory prompt
    /// section must be upgraded from the minimal to the full instruction set.
    pub(crate) first_write: bool,
}

/// Inspect executed tool calls for writes into `memory_dir`.
///
/// Returns `None` when memory is disabled or no write targeted the directory.
pub(crate) fn observe_memory_writes(
    tool_calls: &[ContentBlock],
    memory_dir: Option<&Path>,
    already_upgraded: bool,
) -> Option<MemoryWriteObservation>;
```

判定逻辑：从 `ContentBlock::ToolUse` 中取 `name ∈ {Write, Edit}` 的 `input.file_path`，经 `paths::is_memory_path` 判定。**这是激活 `is_memory_path` 死代码的接线点。**

**M1-4 引擎接线** — `engine.rs:734 execute_tool_round` 返回后

```rust
if let Some(obs) = observe_memory_writes(tool_calls, self.memory_dir.as_deref(), self.prompt_cache.memory_full_instructions) {
    if obs.first_write {
        self.prompt_cache.memory_full_instructions = true;
    }
    self.prompt_cache.invalidate("memory");   // 首个生产调用点
    self.rebuild_system_prompt();
    self.memory_written_this_turn = true;      // 供 M4 抽取互斥使用
    self.refresh_local_context_estimate();
}
```

`AgentEngine` 需新增 `memory_dir: Option<PathBuf>` 与 `memory_written_this_turn: bool` 字段（按 AGENTS.md"结构体字段按职责分组"，与其他会话级状态放在一起）。

#### 验收标准

- AC1-1：会话中模型写入 `<memory_dir>/user_role.md` 并追加索引行后，**同一会话的下一轮** system prompt 中包含该索引行。
- AC1-2：首次写入后，system prompt 的 memory 段包含 `## Types of memory` 与 `<type><name>feedback</name>` 等完整档标记；写入前不包含。
- AC1-3：写入记忆目录之外的文件不触发升级与失效。
- AC1-4：`memory_dir` 为 `None` 时全链路无副作用。

#### 测试

- 单元：`memory/watch_test.rs` — 覆盖 Write/Edit/其他工具、目录内外、`..` 穿越路径、`memory_dir=None`。
- 集成：`crates/agentrs-agent/tests/acceptance/memory_test.rs` 增加 TC-A1-03（会话内刷新）与 TC-A1-04（档位升级），沿用现有无需 LLM 调用的风格。

---

### M2 — C2 记忆新鲜度（P0，1.5 人日）

#### 借鉴源码

`memdir/memoryAge.ts`（53 行）。移植时保留其两条注释所记录的设计动因，这是该模块的核心价值：

> 模型不擅长日期算术——原始 ISO 时间戳不会像 "47 days ago" 那样触发陈旧性推理。

> 动因是用户报告：陈旧的代码状态记忆（指向已变更代码的 `file:line` 引用）被当作事实断言——引用的存在让陈旧的论断听起来更权威，而不是更可疑。

#### 任务 — 新建 `crates/agentrs-memory/src/age.rs`

```rust
/// Days elapsed since `mtime`, floor-rounded. Future timestamps (clock skew)
/// clamp to 0.
pub fn memory_age_days(mtime: DateTime<Utc>, now: DateTime<Utc>) -> i64;

/// Human-readable age: "today" / "yesterday" / "N days ago".
///
/// Models are poor at date arithmetic — a raw ISO timestamp does not trigger
/// staleness reasoning the way "47 days ago" does.
pub fn memory_age(mtime: DateTime<Utc>, now: DateTime<Utc>) -> String;

/// Staleness caveat for memories older than one day. Returns `None` for fresh
/// memories, where the warning would be noise.
pub fn memory_freshness_text(mtime: DateTime<Utc>, now: DateTime<Utc>) -> Option<String>;

/// `memory_freshness_text` wrapped in `<system-reminder>` tags, for callers
/// that do not add their own wrapper.
pub fn memory_freshness_note(mtime: DateTime<Utc>, now: DateTime<Utc>) -> Option<String>;
```

**与 TS 版的差异**：`now` 显式作为参数传入，而不是内部调用 `Utc::now()`——便于测试且符合 Rust 惯例。

#### 配套改动

- `store.rs:122 format_memory_manifest`：ISO 时间戳改为 `memory_age()` 的相对表述（该 manifest 将在 M3 被真正使用）。
- `prompt.rs`：`BEFORE_RECOMMENDING` 段保留，但 M3 落地后逐条记忆额外附加 `memory_freshness_note`。

#### 验收 / 测试

- AC2-1：mtime 为今天 → `"today"`，昨天 → `"yesterday"`，47 天前 → `"47 days ago"`。
- AC2-2：≤1 天的记忆 `memory_freshness_text` 返回 `None`。
- AC2-3：mtime 在未来（时钟偏移）→ 天数钳到 0，不产生负数或 panic。
- 单元测试 `age_test.rs`，用 `rstest` 参数化边界（0/1/2/47 天、未来时间、跨夏令时）。

---

### M3 — C3 相关性召回（P1，4 人日）

解除 25KB 索引天花板的**唯一路径**。

#### 借鉴源码

`memdir/findRelevantMemories.ts`（141 行）+ 已移植的 `memoryScan.ts`。

必须一并移植的两个非显然设计：

1. **`already_surfaced` 前置过滤**（TS :44）——在调用选择器**之前**剔除前几轮已注入的文件，让 Top-N 预算花在新候选上，而不是选完再由调用方丢弃。
2. **`recent_tools` 抑制**（TS :87-92）——正在使用某工具时不召回其 API 参考文档（对话里已有可用示例），**但仍召回关于它的 warning/gotcha**——正在用它，恰恰是坑最要紧的时候。

#### M3-1 trait 与纯逻辑 — 新建 `crates/agentrs-memory/src/recall.rs`

```rust
/// Selects the memories most relevant to a query from a candidate manifest.
///
/// Defined here rather than in `agentrs-agent` so the pure candidate-filtering
/// logic can live alongside it without pulling the agent crate downward.
/// The LLM-backed implementation lives in `agentrs-agent` — mirroring the
/// `TextSummarizer` / `ProviderSummarizer` split.
#[async_trait]
pub trait MemorySelector: Send + Sync {
    /// Return the filenames to surface, at most `limit`. Implementations must
    /// return an empty vec on failure rather than erroring — recall is
    /// best-effort context, never a hard dependency.
    async fn select(&self, query: &str, manifest: &str, limit: usize) -> Vec<String>;
}

pub struct RecallRequest<'a> {
    pub query: &'a str,
    pub memory_dir: &'a Path,
    pub recent_tools: &'a [String],
    pub already_surfaced: &'a HashSet<PathBuf>,
    pub limit: usize,
}

/// Scan, filter, select, and resolve — the full recall pipeline.
pub async fn find_relevant_memories(
    request: RecallRequest<'_>,
    selector: &dyn MemorySelector,
) -> Vec<MemoryHeader>;
```

`find_relevant_memories` 内部：`scan_memory_files` → 过滤 `already_surfaced` → 空则早退（不调用选择器）→ `format_memory_manifest` → `selector.select()` → 用 `validFilenames` 集合校验模型返回值（防幻觉文件名，对应 TS :83,:130）。

#### M3-2 LLM 实现 — 新建 `crates/agentrs-agent/src/memory/recall.rs`

照 `summarizer.rs:ProviderSummarizer` 的形制：

```rust
/// [`MemorySelector`] backed by a secondary (small, fast) model.
pub struct ProviderMemorySelector {
    provider: Arc<dyn LlmProvider>,
    model: String,
    timeout: Duration,
}
```

要点：
- 系统提示词直接移植 `SELECT_MEMORIES_SYSTEM_PROMPT`（TS :18-24），含 `recent_tools` 那段。
- **结构化输出**：优先走 provider 的 JSON schema 能力；不可用时退回"只输出 JSON 数组"的提示 + 宽容解析。**注意**：是否支持结构化输出必须通过 `ProviderCompat` 读取，**不得硬编码 provider 判断**（AGENTS.md 最重要的一条规则）。需新增 compat 字段 `structured_output: Option<StructuredOutputStyle>` 并在各 preset 中设默认值。
- `max_tokens = 256`；超时（默认 3s）与任何错误一律返回空 vec + `debug!` 日志。
- 模型默认取配置 `memory.recall.model`，未配置时回落到主模型。

#### M3-3 引擎接线 — `engine.rs:517 run_inner`

在 `push_history(Role::User, ...)` **之前**发起召回，并把结果作为独立 `ContentBlock::Text` 追加到本轮 user content：

```
<system-reminder>
Relevant memories for this query:

## user_role.md (updated 3 days ago)
<file content>

## feedback_testing.md (updated 47 days ago)
<system-reminder>This memory is 47 days old. Memories are point-in-time observations...</system-reminder>
<file content>
</system-reminder>
```

**性能要求（对应 claude-code 的 `startRelevantMemoryPrefetch`）**：召回是一次额外的 LLM 往返，不能串行阻塞主请求。实现为：`run_inner` 入口 `tokio::spawn` 预取任务 → 构建首个 `LlmRequest` 前 `tokio::time::timeout` 收敛。目录为空或无候选时**完全不发起请求**。

`AgentEngine` 需维护 `surfaced_memories: HashSet<PathBuf>` 与 `recent_tools: VecDeque<String>`（保留最近一轮工具名即可）。

#### 验收 / 测试

- AC3-1：记忆目录有 10 个文件时，注入的记忆数 ≤ `limit`（默认 5）。
- AC3-2：同一文件不会在连续两轮被重复注入（`already_surfaced` 生效）。
- AC3-3：选择器返回不存在的文件名时被静默丢弃，不产生错误。
- AC3-4：选择器超时/报错时，主流程正常完成，无用户可见错误。
- AC3-5：记忆目录为空时，不产生任何侧查询（可通过 mock provider 的调用计数断言）。
- 单元：`recall_test.rs` 用 mock `MemorySelector`（无需 LLM）覆盖过滤、校验、早退。
- 集成：`tests/recall_integration.rs` + agent 侧 VCR（仓库已有 `vcr.rs`）录制真实侧查询。

---

### M4 — C4 后台自动抽取（P1，6 人日）

工作量最大、风险最高，建议在 M1–M3 稳定后单独一个迭代。

#### 借鉴源码

`services/extractMemories/extractMemories.ts`（615 行）。三个必须一并移植的机制：

| 机制 | TS 位置 | 作用 |
|---|---|---|
| 互斥保护 | `hasMemoryWritesSince` :121 | 主 agent 本轮已写记忆 → 跳过抽取。二者每轮互斥，避免重复与冲突 |
| 受限工具策略 | `createAutoMemCanUseTool` :171 | 抽取 agent 只能 Read/Grep/Glob + 只读命令；Write/Edit **仅限记忆目录内** |
| 优雅收尾 | `drainPendingExtraction` :611 | 进程退出前 await 在途抽取，避免记忆写一半被打断 |

#### M4-1 抽取模块 — 新建 `crates/agentrs-agent/src/memory/extract.rs`

```rust
/// Background memory extraction: at the end of a completed query loop, fork a
/// restricted sub-agent that reads the turn transcript and persists durable
/// memories.
///
/// Skipped when the main agent already wrote memory this turn — its prompt
/// carries the full save instructions, so a second pass is redundant.
pub(crate) struct MemoryExtractor {
    spawner: Arc<AgentSpawner>,
    memory_dir: PathBuf,
    config: ExtractConfig,
    in_flight: Mutex<Option<JoinHandle<()>>>,
}

impl MemoryExtractor {
    /// Fire-and-forget. Returns immediately; the caller must call `drain`
    /// before process exit.
    pub(crate) fn spawn_extraction(&self, transcript: Vec<Message>, cursor: usize);

    /// Await the in-flight extraction with a soft timeout.
    pub(crate) async fn drain(&self, timeout: Duration);
}
```

**工具策略**（对应 `createAutoMemCanUseTool`）：

```rust
let policy = ToolPolicy::allow_only(["Read", "Grep", "Glob", "Write", "Edit"]);
```

`ToolPolicy` 目前只做**按名白名单**（`tool_policy.rs:12`），无法表达"Write 仅限某目录"。因此需要扩展：

```rust
pub enum ToolPolicy {
    Unrestricted,
    AllowOnly(BTreeSet<String>),
    /// Allow-list plus a filesystem write confinement: Write/Edit calls whose
    /// `file_path` falls outside `write_root` are denied before execution.
    AllowOnlyWithinRoot { tools: BTreeSet<String>, write_root: PathBuf },
}
```

拒绝判定放在 `engine.rs:734 execute_tool_round` 已有的 `policy_denied_tool_names` 计算处——那里已经在做按名策略拒绝，扩展为可读 `input.file_path` 即可，不新增执行路径。

**抽取提示词**：移植 `extractMemories/prompts.ts`（154 行）。关键是预先把 `format_memory_manifest` 的结果注入提示（TS `memoryScan.ts:27-28` 注释：*"pre-injects the listing so the extraction agent doesn't spend a turn on `ls`"*）——这直接复用 M3 已经激活的 manifest 能力。

#### M4-2 引擎接线

- **游标管理**：`AgentEngine` 增加 `extract_cursor: usize`，记录已抽取到的历史消息下标，避免重复抽取。
- **触发点**：`engine.rs:1676 run_stop_hooks` 附近（该函数已经是"一轮完整结束"的语义位置）。条件：`!self.memory_written_this_turn`（M1 已设置该标志）。
- **收尾**：CLI 退出路径调用 `drain(Duration::from_secs(5))`。
- **抽取完成后**：`invalidate("memory")`，使下一轮 prompt 反映新记忆。

#### 风险与缓解

| 风险 | 缓解 |
|---|---|
| 每轮多一次完整子 agent 调用，成本翻倍 | 默认 `memory.extract.enabled = false`；可配 `memory.extract.model` 走小模型；主 agent 已写时跳过 |
| 抽取写入与主流程并发写索引 | 依赖 M0-2 的原子写；抽取 agent 与主 agent 每轮互斥 |
| 隐私：transcript 内容被持久化落盘 | 显式配置开关，默认关闭；文档明确说明；`/memory` 命令（M6）提供审查与删除入口 |
| 子 agent 越权写盘 | `AllowOnlyWithinRoot` 在执行前拒绝，且拒绝发生在工具层之前 |

#### 验收 / 测试

- AC4-1：主 agent 本轮写过记忆 → 不派生抽取子 agent（用 spawner 调用计数断言）。
- AC4-2：抽取子 agent 尝试写记忆目录之外的路径 → 被拒绝，返回工具错误，不落盘。
- AC4-3：抽取产出的记忆文件带合法 frontmatter，且 `MEMORY.md` 有对应索引行。
- AC4-4：`drain` 在超时内返回；进程退出不留半截文件。
- AC4-5：`enabled = false` 时零开销（无 spawn、无扫描）。

---

### M5 — C5 权限边界（P1，2 人日）

#### 借鉴源码

`utils/permissions/filesystem.ts:1572`（写）、`:1716`（读）。以及其中一个精细边界：

> 默认路径在 `~/.claude/` 下、属于危险目录，所以需要 carve-out；但**用户自定义的 override 路径不给特殊待遇**——那是调用方指定的任意目录，走正常权限流程询问用户。

#### 任务

- 在 `confirm.rs:ToolConfirmer` 与 `engine.rs:772` 的 approval 分支之前插入记忆目录判定：路径命中 `is_memory_path(memory_dir)` **且** `memory_dir` 来自默认解析（非 `AGENTRS_MEMORY_DIR` 覆盖）→ 直接放行，`decision_reason = "auto memory files are allowed"`。
- `paths.rs` 增加 `pub fn is_default_memory_dir() -> bool`，供上述判定区分默认路径与用户覆盖路径。
- 放行事件按 AGENTS.md 日志规范打 `debug!`（高频内部流程），**不得记录文件内容**。

#### 验收

- AC5-1：默认路径下写记忆文件不触发审批提示。
- AC5-2：设置 `AGENTRS_MEMORY_DIR` 后，写该目录仍走正常审批流程。
- AC5-3：读记忆文件同样放行。

---

### M6 — C6 检索指引 + C7 开关/命令/遥测（P2，3 人日）

#### M6-1 过去上下文检索指引

移植 `memdir.ts:375 buildSearchingPastContextSection`，追加到完整档提示词末尾。需按 agentrs 的实际工具与会话目录改写：

```
## Searching past context

When looking for past context:
1. Search topic files in your memory directory:
   Grep with pattern="<search term>" path="<memory_dir>" glob="*.md"
2. Session transcripts (last resort — large files, slow):
   Grep with pattern="<search term>" path="<session_dir>" glob="*.jsonl"
Use narrow search terms (error messages, file paths, function names)
rather than broad keywords.
```

会话目录取自 `session.rs:242` 的 session dir。工具名需按 `ToolPolicy` 过滤——若 Grep 未授权则改用 shell 形式（对应 TS 的 `embedded` 分支）。

#### M6-2 配置开关 — 新建 `crates/agentrs-memory/src/config.rs`

```rust
pub struct MemoryConfig {
    /// Master switch. Disabling skips all memory work including prompt injection.
    pub enabled: bool,
    /// Explicit base directory override (also settable via AGENTRS_MEMORY_DIR).
    pub dir: Option<PathBuf>,
    pub recall: RecallConfig,     // enabled, model, limit(=5), timeout_ms(=3000)
    pub extract: ExtractConfig,   // enabled(=false), model, max_turns
}
```

挂到 `agentrs-config` 的 `Config`（`config.rs:291`）为 `pub memory: MemoryConfig`，与 `file_cache` / `compact` 同层级。TOML 键为 `[memory]` / `[memory.recall]` / `[memory.extract]`。

> **分层注意**：`MemoryConfig` 的类型定义放在 `agentrs-memory`，`agentrs-config` 反向依赖会形成环。因此按现有 `ProviderCompat` 的做法，**类型定义放 `agentrs-config`**，`agentrs-memory` 通过已有的 `agentrs-config` 依赖读取。这与 §2.1 决策 1 不同，因为配置是 config crate 的固有职责。

#### M6-3 `/memory` 命令 — 新建 `crates/agentrs-agent/src/commands/memory.rs`

实现 `SlashCommand` trait（`commands/registry.rs:50`），子命令：

| 子命令 | 行为 |
|---|---|
| `/memory`（无参） | 列出所有记忆：`[type] filename (age) — description`，直接复用 `format_memory_manifest` + `age.rs` |
| `/memory show <file>` | 打印单个记忆全文 |
| `/memory rm <file>` | 删除文件 + 清索引（**激活 `delete_memory` 与 `remove_index_entry` 两处死代码**） |
| `/memory path` | 打印记忆目录绝对路径 |

在 `commands/registry.rs` 注册，返回 `CommandResult::Continue`（`rm` 返回 `ContextChanged` 以触发 prompt 重建）。

#### M6-4 基础遥测（**排期提前到迭代 1**，见 §0.5）

按 claude-code `memoryShapeTelemetry` 的思路，但用 `tracing` 结构化日志实现（agentrs 无分析后端）。

迭代 1 阶段先落地不依赖召回的两项：**记忆文件被 `Read` 的次数**（在 M1 的 `watch.rs` 里顺带观测 Read 工具命中记忆目录）与**会话启动时的记忆条目数 / 索引行数**。这两项就足以支撑 §0.6 的决策判据。M3 落地后再补召回形状：

```rust
debug!(target: "agentrs_memory",
    candidates = memories.len(),
    selected = selected.len(),
    max_age_days = ...,
    "memory recall completed");
```

**关键**：空选择也要记录——"选择率需要分母"（`findRelevantMemories.ts:64-65`）。按 AGENTS.md 日志规范，**不得记录记忆内容本身**，只记形状。

---

## 4. 排期与依赖

```
M0   缺陷修复    ██                     1 人日   ── 无依赖，先做
M1   写入感知    ████████               3 人日   ── 依赖 M0
M2   新鲜度      ███                  1.5 人日   ── 可与 M1 并行
M6-4 基础遥测    █                    0.5 人日   ── 提前（见 §0.5），无依赖
M3   相关性召回  ██████████             4 人日   ── 依赖 M0/M2
M4   自动抽取    ███████████████        6 人日   ── 依赖 M1/M3
M5   权限边界    ████                   2 人日   ── 依赖 M1（复用 is_memory_path 接线）
M6   开关/命令   █████                2.5 人日   ── 依赖 M2（age）、M3（manifest）
                                     ─────────
                                      20.5 人日
```

**建议节奏**：

| 迭代 | 内容 | 交付价值 |
|---|---|---|
| 迭代 1（1.5 周） | M0 + M1 + M2 + **M6-4 遥测** | 记忆系统从"读一半"变为自洽：会话内可见、完整指令可达、陈旧性可判；**并首次具备"记忆有没有被用到"的观测能力** |
| 迭代 2（1 周） | M3 + M5 | 解除索引容量天花板；记忆目录读写免打扰 |
| 迭代 3（1.5 周） | M4 + M6 其余 | 记忆自动积累；用户可审查、可关闭 |

**迭代 1 是无条件应做的最小集**——它不预设后续路线，即使最终决定收缩或外包，这部分修复也不浪费。遥测随迭代 1 一起交付，使迭代 2、3 的投入成为数据驱动的决策（判据见 §0.6）。

---

## 5. 工程规范检查清单

每个 milestone 合并前逐项自查（源自 `agentrs/AGENTS.md`）：

- [ ] `cargo clippy` 无警告，`cargo fmt --all` 无 diff
- [ ] 推送使用 `just push`（自动跑 fmt → clippy → test）
- [ ] 注释与提交信息为英文
- [ ] 错误处理：公开 API 用 `thiserror`，内部传播用 `anyhow`；无裸 `unwrap()`
- [ ] **可见性复核**：新增项默认 `pub(crate)`；仅跨 crate 使用的才 `pub`。本方案会激活多个现有死代码 `pub`，同时应把仍无跨 crate 调用者的收窄
- [ ] 无业务逻辑写在 `mod.rs` / `lib.rs`
- [ ] 两个 `::` 以上的路径一律 `use` 导入后按本地名引用
- [ ] 测试拆分到同目录 `*_test.rs`，源文件只保留 `#[cfg(test)] #[path=...] mod ..._test;` 挂载
- [ ] **无硬编码 provider 判断**：M3 的结构化输出能力必须走 `ProviderCompat` 字段
- [ ] 跨平台：路径用 `Path::join`；深度判断用 `Component::Normal`；shell 调用走 `agentrs_config::shell`
- [ ] 日志：关键路径评估分级；生产日志不含记忆内容、prompt、文件内容
- [ ] 文件保持在 1000 行以内
- [ ] `docs/advanced.md` 的 Memory System 章节同步更新（新增召回、抽取、命令、配置说明）

---

## 6. 风险登记

| # | 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|---|
| R1 | M3 召回增加每轮延迟 | 高 | 中 | 异步预取 + 超时降级 + 空目录早退；默认 `limit=5`、`timeout=3s` |
| R2 | M4 抽取使 token 成本翻倍 | 高 | 高 | 默认关闭；主 agent 已写时跳过；可配小模型 |
| R3 | 结构化输出在部分 provider 不可用 | 中 | 中 | `ProviderCompat` 声明能力 + 宽容 JSON 解析回退 + 失败返回空 |
| R4 | 自动抽取写入低质量/错误记忆 | 中 | 高 | `/memory` 命令提供审查与删除；`age.rs` 标注陈旧性；提示词强调"用前验证" |
| R5 | 隐私：transcript 内容落盘 | 中 | 高 | 默认关闭 + 文档显式说明 + 目录路径可配 + 删除入口 |
| R6 | `ToolPolicy` 扩展影响既有 Spawn 路径 | 低 | 中 | 新增枚举变体而非修改现有变体；既有 `AllowOnly` 语义不变；补 `tool_policy_test.rs` 回归 |
| R7 | M0-1 修改路径清洗规则导致现有记忆"丢失" | 中 | 低 | 目录名变化会使旧记忆不可见。提供一次性迁移提示：启动时若旧目录存在而新目录不存在，`warn!` 输出迁移命令；不自动移动用户数据 |

---

## 附录 A：claude-code → agentrs 源码映射表

| claude-code 源文件 | 行数 | agentrs 目标位置 | 状态 | 里程碑 |
|---|---|---|---|---|
| `memdir/memdir.ts:34-38` 常量 | — | `paths.rs:14`, `index.rs:17-20` | ✅ 已移植 | — |
| `memdir/memdir.ts:57 truncateEntrypointContent` | 60 | `index.rs:49 truncate_index` | ✅ 已移植 | — |
| `memdir/memdir.ts:199 buildMemoryLines` | 75 | `prompt.rs:295 build_memory_instructions` | ✅ 已移植，❌ 未接线 | M1 |
| `memdir/memdir.ts:129 ensureMemoryDirExists` | 25 | `paths.rs:96 ensure_memory_dir` | ✅ 已移植，❌ 未接线 | M1 |
| `memdir/memdir.ts:375 buildSearchingPastContextSection` | 45 | `prompt.rs` 新增段 | ❌ 未移植 | M6 |
| `memdir/memdir.ts:419 loadMemoryPrompt` | 90 | `context.rs` 档位分派 | ❌ 未移植 | M1 |
| `memdir/paths.ts:274 isAutoMemPath` | 10 | `paths.rs:80 is_memory_path` | ✅ 已移植，❌ 未接线 | M1/M5 |
| `memdir/paths.ts:30 isAutoMemoryEnabled` | 40 | `MemoryConfig.enabled` | ❌ 未移植 | M6 |
| `memdir/memoryScan.ts:35 scanMemoryFiles` | 45 | `store.rs:86 scan_memory_files` | ✅ 已移植，❌ 未接线 | M3 |
| `memdir/memoryScan.ts:84 formatMemoryManifest` | 12 | `store.rs:117 format_memory_manifest` | ✅ 已移植，❌ 未接线 | M3 |
| `memdir/memoryAge.ts` | 53 | `age.rs`（新） | ❌ 未移植 | M2 |
| `memdir/findRelevantMemories.ts` | 141 | `recall.rs` + `agent/memory/recall.rs`（新） | ❌ 未移植 | M3 |
| `services/extractMemories/extractMemories.ts` | 615 | `agent/memory/extract.rs`（新） | ❌ 未移植 | M4 |
| `services/extractMemories/prompts.ts` | 154 | `agent/memory/extract.rs` 提示词常量 | ❌ 未移植 | M4 |
| `utils/permissions/filesystem.ts:1572,1716` | — | `confirm.rs` / `engine.rs:772` | ❌ 未移植 | M5 |
| `commands/memory/memory.tsx` | — | `commands/memory.rs`（新） | ❌ 未移植 | M6 |
| `memdir/memoryShapeTelemetry.ts` | — | `tracing` 结构化日志 | ❌ 未移植 | M6 |
| `services/teamMemorySync/` | 1256 | — | 不引入 | — |
| `services/teamMemorySync/secretScanner.ts` | 324 | — | 不引入 | — |
| `services/SessionMemory/` | ~700 | — | 不引入 | — |
| `tools/AgentTool/agentMemory.ts` | — | — | 后续评估 | — |

## 附录 B：接线点速查

| 用途 | 文件:行 | 说明 |
|---|---|---|
| 记忆目录解析 | `bootstrap.rs:214` | `auto_memory_dir(&workspace_path)`；M1 在此后加 `ensure_memory_dir` |
| system prompt 装配 | `bootstrap.rs:~296` `configure_system_prompt` | 一次性写入 `config.system_prompt`，M1 需改为可重建 |
| memory section | `context.rs:284-292` | 档位分派改造点 |
| 缓存失效 | `context.rs:47` `SystemPromptCache::invalidate` | M1 的首个生产调用点 |
| 用户轮入口 | `engine.rs:517` `run_inner` | M3 召回注入点（`push_history` 之前） |
| 工具轮执行 | `engine.rs:734` `execute_tool_round` | M1 写入感知、M4 目录限制拒绝 |
| 审批分支 | `engine.rs:772` | M5 carve-out 插入点 |
| 一轮结束 | `engine.rs:1676` `run_stop_hooks` | M4 抽取触发点 |
| 上下文用量 | `context_usage.rs:119` `PromptUsage` | M6 遥测扩展点 |
| 侧查询模板 | `summarizer.rs:25-63` `ProviderSummarizer` | M3 `ProviderMemorySelector` 的形制参照 |
| 子 agent 派生 | `spawner.rs` `AgentSpawner::spawn_one` | M4 抽取 agent 派生 |
| 工具策略 | `tool_policy.rs:12` `ToolPolicy` | M4 需扩展 `AllowOnlyWithinRoot` |
| 斜杠命令 | `commands/registry.rs:50` `SlashCommand` | M6 `/memory` 实现 |
| 全局配置 | `agentrs-config/src/config.rs:291` `Config` | M6 挂载 `memory: MemoryConfig` |

# agentrs-memory 技术架构设计文档

> 版本：v1.0 · 日期：2026-09-03
> 代码基线：`agentrs/crates/agentrs-memory/`（生产代码 ~1130 行，测试 ~3060 行）
> 依赖层级：Mid（仅依赖 `agentrs-config`）· 消费方：`agentrs-agent`、`agentrs-cli`

---

## 1. 概述

### 1.1 定位

`agentrs-memory` 是 agentrs 的**跨会话长期记忆系统**。它让 agent 在多次独立会话之间保留对用户、协作方式、项目背景和外部资源的理解，从而避免用户反复交代同样的上下文。

它解决的不是"当前对话记不住"（那是上下文窗口与 `agentrs-compact` 的职责），而是"**下一次对话要从零开始**"。

### 1.2 设计范式：Prompt-as-Protocol

本模块采用**提示词即协议**的范式，而非传统的记忆中间件范式。三条核心判断：

| 维度 | 本设计的选择 | 被放弃的方案 |
|---|---|---|
| 存储介质 | 纯 Markdown 文件（YAML frontmatter + 正文） | SQLite / 向量库 / 专用二进制格式 |
| CRUD 执行者 | **LLM 自身**，通过通用 Read/Write/Edit 工具操作 | 专用 Memory 工具 / 宿主程序代管 |
| 召回机制 | 索引文件全量注入 system prompt | embedding 检索 / 关键词倒排 |

这样选择的理由：

- **人可读、可 git 管理、可手工编辑**。记忆内容是关于人的判断，用户必须能审查和修正它。二进制或数据库格式会让记忆变成黑盒。
- **零额外工具面积**。agent 已有完备的文件工具，引入 Memory 工具会增加 tool schema 的 token 开销与模型的选择负担。
- **无外部依赖**。不需要 embedding 服务、不需要数据库，在离线/内网环境同样工作。

代价是：**写入质量与一致性由提示词约束，而非由类型系统或运行时校验保证**。因此本模块最有价值的资产不是文件 I/O 代码，而是 `prompt.rs` 中约 250 行经过精心设计的英文行为指令。

### 1.3 非目标

- 不做语义检索 / 相似度召回。
- 不做会话内的短期记忆或上下文压缩（属 `agentrs-compact`）。
- 不做多用户/团队共享记忆（当前为 individual-only，提示词中已显式剔除 team/private scope 标签）。
- 不做记忆内容的真实性校验——提示词层面要求模型"使用前先验证当前状态"。

---

## 2. 系统架构

### 2.1 分层与依赖

```
                   ┌─────────────────────┐
                   │   agentrs-agent     │  bootstrap.rs / context.rs
                   │  (system prompt 装配) │
                   └──────────┬──────────┘
                              │ build_memory_prompt_minimal(dir)
                              │ auto_memory_dir(workspace)
              ┌───────────────▼──────────────────┐
              │        agentrs-memory            │
              │                                  │
              │   prompt.rs ────► index.rs       │
              │       │              │           │
              │   store.rs           │           │
              │       │              │           │
              │   paths.rs ◄─────────┘           │
              │       │                          │
              │   types.rs   error.rs            │
              └───────────────┬──────────────────┘
                              │ app_config_dir()
                   ┌──────────▼──────────┐
                   │   agentrs-config    │
                   └─────────────────────┘
```

依赖严格向下，无循环。`types.rs` / `error.rs` 为叶子；`paths.rs` 是唯一接触平台差异的模块。

### 2.2 模块职责矩阵

| 模块 | 行数 | 单一职责 | 对外关键 API |
|---|---|---|---|
| `types.rs` | 171 | 数据模型定义 | `MemoryType`、`MemoryFrontmatter`、`MemoryHeader`、`MemoryEntry`、`IndexTruncation` |
| `error.rs` | 23 | 错误类型 | `MemoryError`（Io / FrontmatterParse / PathValidation）、`Result<T>` |
| `paths.rs` | 219 | 路径解析、清洗、安全校验、目录创建 | `auto_memory_dir`、`memory_entrypoint`、`validate_memory_path`、`is_memory_path`、`ensure_memory_dir`、`sanitize_path` |
| `store.rs` | 379 | 记忆文件的读写删、目录扫描、清单格式化 | `read_memory`、`write_memory`、`delete_memory`、`scan_memory_files`、`format_memory_manifest` |
| `index.rs` | 194 | MEMORY.md 索引的读取、截断、条目增删 | `read_index`、`truncate_index`、`append_index_entry`、`remove_index_entry` |
| `prompt.rs` | 341 | 行为指令与索引内容的提示词装配 | `build_memory_prompt_minimal`、`build_memory_prompt`、`build_memory_instructions`、`memory_type_descriptions` |

`lib.rs` 仅做模块声明，不含业务逻辑（符合 AGENTS.md 规范）。

---

## 3. 数据模型

### 3.1 记忆类型分类学

四类固定类型，是整个系统的语义骨架：

| 类型 | 语义 | 触发保存的信号 | 正文结构要求 |
|---|---|---|---|
| `user` | 用户的角色、目标、职责、知识背景 | 学到任何关于用户身份/偏好/专长的细节 | 自由 |
| `feedback` | 用户对**工作方式**的指导（纠正 **与** 确认） | "别这样做" / "对，就这样保持" | 规则 + `**Why:**` + `**How to apply:**` |
| `project` | 项目中正在发生的、代码与 git 推导不出的上下文 | 学到"谁在做什么、为什么、什么时候" | 事实 + `**Why:**` + `**How to apply:**` |
| `reference` | 外部系统的指针（Linear 项目、Grafana 看板…） | 学到外部资源及其用途 | 自由 |

两个非显然的设计决策：

1. **`feedback` 强制记录成功而非只记录失败**。提示词中明确写道：只存纠正会让 agent 避免旧错误但**漂离已被验证的做法，并逐渐过度保守**。确认信号（"perfect, keep doing that"、无异议地接受一个非常规选择）更安静，需要专门提示模型去捕捉。
2. **`Why:` 是强制的**。知道规则背后的原因，模型才能在边界情况上做判断，而不是机械执行。`project` 类记忆衰减快，`Why:` 还能帮助未来的自己判断该记忆是否仍然成立。

### 3.2 记忆文件格式

```markdown
---
name: auth-rewrite
description: Auth 中间件重写由合规要求驱动，而非技术债清理
type: project
---

Auth 中间件重写的动因是法务/合规对 session token 存储方式的要求。

**Why:** 法务在 2026-02 标记了不符合新合规要求的存储方式。
**How to apply:** 范围决策上优先满足合规，而非开发体验。相关 PR 见 [[user-role]]。
```

- frontmatter 三字段 **全部为 `Option`**（`types.rs:89-95`）。这是刻意的宽容设计：手工编辑或历史遗留文件缺字段时，系统降级而不报错。
- `description` 的用途在提示词中被明确定义为"**未来会话中用于判断相关性**"，因此要求具体而非笼统。

### 3.3 索引文件（MEMORY.md）

```markdown
- [用户角色](user_role.md) — 资深 Rust 工程师，负责 agent 内核
- [测试约定](feedback_testing.md) — 集成测试必须打真实数据库
```

- **索引不是记忆**，是目录。每行一条，约 150 字符以内，无 frontmatter。
- MEMORY.md **始终**被注入 system prompt，因此它的体积直接等于每轮对话的固定 token 成本——这是索引存在硬上限的根本原因。

### 3.4 内存表示

```rust
MemoryEntry   { frontmatter: MemoryFrontmatter, content: String }   // 全量，read/write 用
MemoryHeader  { filename, file_path, mtime, description, memory_type } // 轻量，扫描用
IndexTruncation { content, line_count, byte_count, was_truncated }     // 截断结果 + 诊断
```

`MemoryHeader` 与 `MemoryEntry` 的分离是为了让目录扫描**只读文件前 30 行**即可产出清单，避免为了生成一份索引而把所有记忆正文读进内存。

---

## 4. 存储布局与路径策略

### 4.1 目录结构

```
<base>/projects/<sanitized-project-root>/memory/
├── MEMORY.md                 # 索引，自动注入 prompt，上限 200 行 / 25KB
├── user_role.md
├── feedback_testing.md
└── project_auth_rewrite.md
```

### 4.2 base 解析顺序（`paths.rs:34`）

1. 环境变量 `AGENTRS_MEMORY_DIR`（非空时优先，用于测试隔离与自定义部署）
2. `agentrs_config::config::app_config_dir()` → `dirs::config_dir()/agentrs`

两者都不可用时返回 `None`，上层据此**整体关闭记忆功能**（`context.rs:284` 的 `if let Some(dir)`），而不是回退到某个可能错误的路径。

### 4.3 项目路径清洗

`sanitize_path`（`paths.rs:148`）把项目绝对路径转成一个安全的目录名：非 ASCII 字母数字字符 → `-`，超过 200 字符时截断并追加哈希后缀。

> **已知缺陷（见 §9）**：短于 200 字符时不追加哈希，导致 `/home/张三/proj` 与 `/home/李四/proj` 清洗后同名，两个项目会静默共享同一份记忆。修复方案：无条件追加原始路径哈希。

### 4.4 安全校验

`validate_memory_path`（`paths.rs:114`）四重检查：绝对路径、`Component::Normal` 深度 ≥ 2（跨平台一致，避免用字节长度判断）、无空字节、无 `..` 段。

`is_memory_path`（`paths.rs:80`）判断某路径是否落在记忆目录内，两侧都先 canonicalize 以防符号链接与 `..` 绕过；路径不存在时退化为词法归一化，但**含 `..` 时直接判失败**——因为解析父级引用必须依赖真实文件系统状态。

---

## 5. 核心算法

### 5.1 Frontmatter 解析（`store.rs:148`）

手写状态机而非通用 Markdown 解析器，原因是只需识别一种固定结构且要严格限流：

1. 内容必须以 `---` 开头，否则整个文件作为 body 返回（默认 frontmatter）。
2. 在**最多 30 行**内寻找闭合 `---`；超限即判定为无 frontmatter，全文作 body。
3. 中间段交给 `serde_yaml` 反序列化；失败则 `tracing::warn!` 并返回默认值。

30 行上限同时是安全阀（防止畸形文件导致全文扫描）和性能保证（`read_header` 只需 `BufReader` 读前 30 行）。

**降级原则**：任何一步失败都不抛错，只损失元数据。记忆是增强性上下文，不应中断会话。

### 5.2 索引截断（`index.rs:49`）

```
trim → 若行数 ≤ 200 且字节 ≤ 25000 → 原样返回
     → 否则：先按行截断到 200 行
             再若仍 > 25000 字节：
                 floor_char_boundary(25000)   // 防 CJK 多字节边界 panic
                 回退到该位置之前的最后一个 '\n'  // 防截半行
             追加诊断告警，说明触发了哪个上限及如何修正
```

三个关键细节：

- **字节判定用原始长度**，不用行截断后的长度。因为"少量超长行"正是字节上限要防的失败模式，用行截后的长度会低估问题。
- **`floor_char_boundary`** 处理中文场景：25000 字节的切点极可能落在多字节字符中间，直接切片或 `String::truncate` 会 panic。
- **告警文案面向模型**，不只是记录事实，还给出行动指令："把索引条目控制在一行 200 字符内，细节移进主题文件"。

### 5.3 提示词双档策略（`prompt.rs`）

| 档位 | 函数 | 体积 | 内容 |
|---|---|---|---|
| 精简 | `build_memory_prompt_minimal` | ~150 token | 路径 + `MINIMAL_RULES` + MEMORY.md 内容 |
| 完整 | `build_memory_prompt` | ~2650 token | 上述 + 四类分类学（含 8 组 few-shot）+ 禁存清单 + 两步保存流程 + 访问时机 + 陈旧性校验 + 与 Plan/Task 的边界 |

system prompt 每轮都要重发，因此固定成本必须压到最低。设计意图是：**默认注入精简档，当模型首次真正写入记忆目录时再升级到完整档**——读取记忆只需要知道系统存在，而写入记忆才需要完整的分类学与格式规范。

> **实现状态**：升级触发点当前尚未接线，见 §9.1。

另外 `DIR_EXISTS_GUIDANCE`（`prompt.rs:24`）显式告诉模型"目录已存在，直接用 Write 写，不要 mkdir 或检查存在性"，避免每次保存都浪费一到两轮工具调用。

---

## 6. 运行时集成

### 6.1 装配链路

```
AgentBootstrap::resolve_environment          bootstrap.rs:214
    └─ auto_memory_dir(workspace_path) → Option<PathBuf>

AgentBootstrap::configure_system_prompt      bootstrap.rs:~296
    └─ build_system_prompt_with_shell_and_tool_policy(..., memory_dir, ...)
           └─ context.rs:284  cache.sections["memory"]
                  = build_memory_prompt_minimal(dir)
                        └─ read_index() + truncate_index()
    └─ PromptUsage::from_sections(..., memory_prompt, memory_files, ...)
    └─ self.config.system_prompt = Some(system_prompt)
```

### 6.2 System prompt 中的位置

`context.rs:132-146` 定义的装配顺序：

1. 基础介绍（角色、模型、工作目录、日期、OS、shell）
2. 工具使用指引
3. 用户自定义 prompt
4. AGENTS.md（项目指令）
5. **记忆（行为指令 + MEMORY.md 内容）**
6. TOON 格式说明（可选）
7. Plan 模式指令（激活时）
8. Skills 清单

记忆排在项目指令之后、动态内容之前——它是稳定的会话级上下文，但优先级低于项目显式规则。

### 6.3 缓存机制

`SystemPromptCache`（`context.rs:19`）按 section 分区缓存，`joined` 字段缓存最终拼接结果，任一 section 变化即失效。memory section 属于"可事件失效"类，设计上支持 `cache.invalidate("memory")` 单独重建。

### 6.4 用量归因

`PromptUsage`（`context_usage.rs:119`）单独统计 `memory_tokens` 与 `memory_files`，使记忆对上下文窗口的占用可被用户观测。`system_prompt_tokens` 用总量减去 memory 与 skills，避免重复计数。

---

## 7. 生命周期

### 7.1 读取（每次会话启动）

```
bootstrap → auto_memory_dir → read_index(MEMORY.md) → truncate_index
         → 拼入 system prompt → 模型在需要时用 Read 工具展开具体主题文件
```

索引提供"有哪些记忆、各自讲什么"的地图，正文按需加载。这是一种**两级惰性加载**：索引常驻，正文按相关性触发。

### 7.2 写入（模型驱动，两步）

```
Step 1: Write 工具 → <type>_<name>.md（含 frontmatter）
Step 2: Edit/Write  → MEMORY.md 追加一行 `- [Title](file.md) — hook`
```

提示词明确要求：先检查是否已有可更新的记忆再新建；发现记忆过时要更新或删除；按主题而非时间组织。

### 7.3 删除

删除主题文件 + 从索引中移除对应行。`index::remove_index_entry` 提供了幂等的宿主侧实现（文件不存在时静默成功）。

---

## 8. 测试策略

测试代码约为生产代码的 2.7 倍，两层组织（符合 AGENTS.md）：

| 层 | 位置 | 覆盖 |
|---|---|---|
| 单元 | `src/*_test.rs`（同目录挂载） | 各模块内部逻辑与边界：截断边界、frontmatter 畸形输入、路径清洗、文件名生成 |
| 集成 | `tests/*_integration.rs` | 面向公开 API 与规格：`store` / `index` / `paths` / `prompt` / `e2e` |
| 验收 | `agentrs-agent/tests/acceptance/memory_test.rs` | TC-A1-01 记忆注入 system prompt；TC-A1-02 全生命周期（建→索引→验证→删→清索引→验证消失），无需 LLM 调用 |

依赖 `tempfile`（隔离文件系统）、`rstest`（参数化）、`serial_test`（环境变量测试串行化）。

**覆盖缺口**：并发写入、符号链接环、超大目录扫描、非 ASCII 项目路径碰撞。

---

## 9. 已知缺陷与技术债

本节记录设计意图与当前实现之间的差距，按优先级排列。

### 9.1 P0 — 未接线的能力（设计与实现两层皮）

经全仓库检索确认，以下公开 API **在生产代码中零调用，仅测试引用**：

- `store::{read_memory, write_memory, delete_memory, scan_memory_files, format_memory_manifest}`
- `paths::{is_memory_path, validate_memory_path, ensure_memory_dir}`
- `prompt::{build_memory_prompt, build_memory_instructions, memory_type_descriptions}`
- `MemoryError::FrontmatterParse`（定义但从未构造，解析失败走 warn + 默认值）

生产链路实际只剩：读一个文件 → 截断 → 拼进 prompt。派生出三个具体问题：

| # | 问题 | 影响 | 证据 |
|---|---|---|---|
| 9.1.1 | 完整提示词的惰性升级未实现 | 模型永远只见到 150 token 精简规则；四类语义、禁存清单、两步保存流程、正文结构要求全部不可达，写入质量退化为依赖模型先验 | `context.rs:282` 注释声称按需注入，但无监听写入的代码，`is_memory_path` 无调用点 |
| 9.1.2 | 会话内索引不刷新 | 本次会话新写入的记忆直到进程重启才进入上下文；提示词"保存后会出现在这里"当轮落空，可能诱发重复写入 | `bootstrap.rs` 一次性写死 `config.system_prompt`；`invalidate("memory")` 仅出现在 `context_test.rs:944` |
| 9.1.3 | 索引与正文无一致性保障 | 文件名与索引行均由模型手工产出，无对账、无孤儿检测、无 GC | `generate_filename`（`store.rs:233`）未被调用 |

### 9.2 P1 — 正确性风险

- **项目路径碰撞**（`paths.rs:148`）：短路径不加哈希后缀，非 ASCII 路径大面积塌缩为同名目录，导致跨项目记忆污染。
- **索引写入非原子**（`index.rs:131`）：read-modify-write 无锁、无 temp+rename，多会话并发写会丢条目。`write_memory` 同理。
- **`remove_index_entry` 子串误匹配**（`index.rs:165`）：以 `(filename)` 做 `contains` 判断，`(a.md)` 会误删含 `(xa.md)` 的行。

### 9.3 P2 — 健壮性与一致性

- **扫描无防护**（`store.rs:303`）：递归无深度限制、无符号链接检测（symlink 环会无限递归）；200 文件上限在**全量遍历并读完每个文件前 30 行之后**才生效，大目录开销不受控。
- **截断告警文案不一致**（`index.rs:103`）：行数与字节同时超限的分支丢失了 limit 说明。
- **MSRV 隐性约束**：`floor_char_boundary` 需要较新工具链（当前基线 rustc 1.96.1 可用），应在文档中标注。

### 9.4 架构层面的天花板

召回策略是"索引全量注入"，规模上限即 200 行 / 25KB。超出后模型只能自行 Read 目录查找。`scan_memory_files` + `format_memory_manifest` 本可作为二级索引（mtime 排序的文件清单）缓解，但未接入。若未来记忆规模需要突破这一量级，需引入检索层——这是当前架构的显式边界，而非缺陷。

---

## 10. 演进路线

按投入产出比排序：

| 优先级 | 事项 | 说明 |
|---|---|---|
| P0 | **接上会话内失效** | Write/Edit 落盘后若 `is_memory_path` 命中，调 `cache.invalidate("memory")` 并重建 prompt。一处改动同时解决 9.1.1 与 9.1.2——首次命中即可把 `build_memory_instructions()` 完整档升级进去 |
| P0 | **`sanitize_path` 无条件追加哈希** | 消除跨项目记忆污染 |
| P1 | **索引写入原子化** | temp file + rename，或文件锁 |
| P1 | **`remove_index_entry` 精确匹配** | 改用 Markdown 链接解析或行首锚定 |
| P2 | **决定 `store.rs` 的去留** | 要么由宿主暴露 Memory 工具走它（换取命名规范、frontmatter 校验、200 文件上限真正生效），要么按 AGENTS.md 可见性规范收窄为 `pub(crate)` 或删除，避免死代码伪装成能力 |
| P2 | **补齐 `FrontmatterParse` 构造点或删除该变体** | 消除未使用的错误分支 |
| P2 | **扫描增加深度上限与 symlink 检测** | |
| P3 | **二级索引** | 记忆规模超出 MEMORY.md 上限时，用 `scan_memory_files` + `format_memory_manifest` 提供按 mtime 排序的文件清单 |

---

## 11. 附录：关键常量

| 常量 | 值 | 位置 | 含义 |
|---|---|---|---|
| `ENTRYPOINT_NAME` | `MEMORY.md` | `paths.rs:14` | 索引文件名 |
| `MEMORY_DIR_ENV` | `AGENTRS_MEMORY_DIR` | `paths.rs:20` | base 目录覆盖变量 |
| `MAX_SANITIZED_LENGTH` | 200 | `paths.rs:17` | 目录名截断阈值 |
| `FRONTMATTER_MAX_LINES` | 30 | `store.rs:18` | frontmatter 搜索上限 |
| `MAX_MEMORY_FILES` | 200 | `store.rs:21` | 单次扫描返回上限 |
| `MAX_INDEX_LINES` | 200 | `index.rs:17` | 索引行数上限 |
| `MAX_INDEX_BYTES` | 25000 | `index.rs:20` | 索引字节上限（~25KB） |

# Agent 记忆方案横向技术对比报告

> 版本：v1.0 · 日期：2026-09-03
> 对比对象：`agentrs-memory` · `claude-code` · `pi` · `opencode` · `deepseek-harness`
> 代码基线：`agentrs/crates/agentrs-memory/`、`opensource/{claude-code-main,pi,opencode,deepseek-harness}`
> 配套文档：[agentrs-memory 技术架构设计文档](./agentrs-memory-技术架构设计文档.md)

---

## 摘要

四个开源项目对"Agent 如何跨会话记住东西"给出了**三种截然不同的哲学**：

- **Agent 自写文件**（claude-code、agentrs）——agent 主动积累对用户的理解，写成结构化 Markdown。
- **人写项目文件**（pi、opencode）——不做自动记忆，跨会话上下文全部来自人类维护的 AGENTS.md。
- **外包给 MCP**（deepseek-harness）——记忆是可插拔外设，产品本体不承载任何记忆逻辑。

`agentrs-memory` 选择了三条路线中**最重的一条**，代码结构直接对标 claude-code 的 `src/memdir/`，但只实现了该范式的第一段（启动时读取索引）。它既缺少让该范式跑通的闭环（召回、抽取、新鲜度、权限），也没有 pi/opencode 那种"不做就不做"的克制，更没有 DSH 的可插拔性。**它是五者中唯一"有记忆框架而无记忆闭环"的方案。**

| 方案 | 范式 | 谁写记忆 | 召回方式 | 记忆相关代码量 |
|---|---|---|---|---|
| **agentrs-memory** | Agent 自写文件 | 主模型（提示词约束） | MEMORY.md 索引全量注入 | 1130 行（生产） |
| **claude-code** | Agent 自写文件 | 主模型 **+ 后台抽取 agent** | 索引常驻 **+ Sonnet 相关性召回** | ~5720 行 |
| **pi** | 无长期记忆 | 人（AGENTS.md） | 启动时加载项目上下文文件 | — |
| **opencode** | 无长期记忆 | 人（AGENTS.md + 远程指令） | 启动加载 + glob + HTTP 拉取 | 237 行 |
| **deepseek-harness** | 外包给 MCP | 第三方记忆服务 | provider 自有 search/recall 工具 | 0 行（3 个配置样例） |

---

## 1. 对比维度总表

| 维度 | agentrs | claude-code | pi | opencode | deepseek-harness |
|---|---|---|---|---|---|
| 长期记忆 | ✅ 文件 | ✅ 文件 | ❌ | ❌ | ⭕ MCP 外接 |
| 记忆分类学 | ✅ 4 类 | ✅ 4 类 | — | — | provider 定义 |
| 索引文件 | ✅ MEMORY.md | ✅ MEMORY.md | — | — | — |
| 索引上限 | 200 行 / 25KB | 200 行 / 25KB | — | — | — |
| **相关性召回** | ❌ 全量注入 | ✅ Sonnet 选 Top-5 | — | — | provider 提供 |
| **自动抽取** | ❌ | ✅ forked agent | — | — | ❌ |
| **逐条新鲜度标注** | ❌ | ✅ "N days ago" + 告警 | — | — | ❌ |
| **权限边界** | ❌ 死代码 | ✅ 读写 carve-out | — | — | MCP 传输层擦除凭据 |
| 会话内记忆 | ❌ | ✅ SessionMemory | ⭕ 会话存储抽象 | ⭕ 会话持久化 | — |
| 子 Agent 记忆 | ❌ | ✅ agentMemory | — | — | — |
| 团队共享 | ❌ | ✅ 服务端同步 | — | ⭕ 远程 URL 指令 | provider 负责 |
| 上传前密钥扫描 | ❌ | ✅ gitleaks 规则子集 | — | — | — |
| 用户界面 | ❌ | ✅ `/memory` + 3 个组件 | — | — | — |
| 灰度开关 | ❌ | ✅ GrowthBook flag | — | — | ✅ default-off overlay |
| 遥测 | ⭕ token 计数 | ✅ 召回形状遥测 | — | — | — |
| 人写上下文文件 | ✅ AGENTS.md | ✅ CLAUDE.md | ✅ AGENTS.md 链 | ✅ 全局+项目+远程 | ✅ .agents/ |

图例：✅ 有 · ⭕ 部分/间接 · ❌ 无 · — 不适用

---

## 2. 与 claude-code 的对比

### 2.1 血缘关系

`agentrs-memory` 是 claude-code `src/memdir/` 的**部分移植**，对应关系一一可查：

| claude-code | agentrs | 内容 |
|---|---|---|
| `memdir.ts:34-38` | `paths.rs:14` / `index.rs:17-20` | `ENTRYPOINT_NAME`、`MAX_ENTRYPOINT_LINES=200`、`MAX_ENTRYPOINT_BYTES=25_000` |
| `memdir.ts:57` `truncateEntrypointContent` | `index.rs:49` `truncate_index` | 双上限截断 + 诊断告警 |
| `memdir.ts:199` `buildMemoryLines` | `prompt.rs:295` `build_memory_instructions` | 四类分类学 + 两步保存 + 访问时机 |
| `memdir.ts:116` `DIR_EXISTS_GUIDANCE` | `prompt.rs:24` 同名常量 | 目录已存在，别 mkdir |
| `memoryScan.ts:21-22` | `store.rs:18-21` | `MAX_MEMORY_FILES=200`、`FRONTMATTER_MAX_LINES=30` |
| `memoryScan.ts:35` `scanMemoryFiles` | `store.rs:86` `scan_memory_files` | 目录扫描 → 头部列表 |
| `memoryScan.ts:84` `formatMemoryManifest` | `store.rs:117` `format_memory_manifest` | `- [type] file (ts): desc` 清单 |

**差距不在组件，而在闭环。** 下面五个子节逐一说明。

### 2.2 召回层：claude-code 有两阶段召回，agentrs 空缺

`findRelevantMemories.ts`（141 行）实现了完整的两阶段召回：

1. `scanMemoryFiles` 扫描目录 → `formatMemoryManifest` 生成清单；
2. 用 **Sonnet 侧查询**（`sideQuery`，`output_format` 强制 JSON schema）从清单中挑选**最多 5 条**确实相关的记忆，作为 `relevant_memories` attachment 注入本轮上下文（消费点 `utils/attachments.ts:2217`、`utils/messages.ts:3708`）。

两个非显然的工程细节：

**(a) `alreadySurfaced` 前置过滤**（:44）
前几轮已注入过的文件在调用 Sonnet **之前**就剔除，让选择器把 5 条预算花在新候选上，而不是选完再由调用方丢弃。

**(b) `recentTools` 抑制误召回**（:87-92）
注释写得很清楚：当 agent 正在使用某个工具（如 `mcp__X__spawn`）时，召回该工具的参考文档是噪声——对话里已经有可用示例了。选择器本来会靠关键词重合命中（query 里有 "spawn" + 记忆描述里有 "spawn" → 假阳性）。但提示词同时强调：**关于这些工具的 warning / gotcha / 已知问题仍然要召回——正在用它，恰恰是这些最要紧的时候。**

**agentrs 的状况**：`scan_memory_files` 和 `format_memory_manifest` 被原样移植，**但它们唯一的消费者没有被移植**。两个函数在生产代码中零调用（`store.rs:86`、`store.rs:117`）。召回因此退化为"把 MEMORY.md 全量塞进 system prompt"，规模天花板锁死在 200 行 / 25KB，超出后没有降级路径，模型只能自己 `Read` 目录去翻。

### 2.3 写入层：claude-code 有后台抽取 agent，agentrs 全靠主模型自觉

`extractMemories.ts`（615 行）在每个 query loop 结束时（模型产出无工具调用的终态回复，经 `handleStopHooks`）**fork 一个 agent** 从本轮 transcript 抽取值得留存的记忆并写盘。它用 `runForkedAgent` 做"perfect fork"——共享父对话的 prompt cache，因此增量成本很低。

三个配套机制：

| 机制 | 位置 | 作用 |
|---|---|---|
| **互斥保护** | `hasMemoryWritesSince` :121 | 主 agent 本轮已写过记忆目录 → 跳过 fork 并推进游标。主 agent 的 prompt 有完整保存指令，此时后台抽取冗余。二者每轮互斥 |
| **受限工具策略** | `createAutoMemCanUseTool` :171 | 抽取 agent 只能 Read/Grep/Glob + 只读 Bash；Edit/Write **仅限记忆目录内**。写权限收窄到最小面 |
| **优雅收尾** | `drainPendingExtraction` :611 | 在 5 秒强杀失效前 await 在途抽取，避免记忆写一半被打断 |

**agentrs 的状况**：完全没有自动抽取。记忆是否产生，取决于主模型在漫长 system prompt 里是否还记得那 150 token 的精简规则——而由于 §2.5 的问题，它连完整规则都看不到。实际后果是：**用户不主动说"记住这个"，记忆目录大概率永远是空的。**

### 2.4 新鲜度：claude-code 把陈旧性做成一等公民

`memoryAge.ts`（53 行）体量很小，但记录了两条来自真实事故的洞察：

> **模型不擅长日期算术——原始 ISO 时间戳不会像 "47 days ago" 那样触发陈旧性推理。**（:11-14）

> 动因是用户报告：陈旧的代码状态记忆（指向已变更代码的 `file:line` 引用）被当作事实断言——**引用的存在让陈旧的论断听起来更权威，而不是更可疑。**（:29-31）

实现：
- `memoryAge()` → `today` / `yesterday` / `N days ago`
- `memoryFreshnessText()` → 超过 1 天才产出告警（新记忆加告警是噪声）
- `memoryFreshnessNote()` → 包 `<system-reminder>` 的变体，供不自带 wrapper 的调用方（如 FileReadTool 输出）使用

**agentrs 的状况**：`format_memory_manifest`（`store.rs:122`）用的正是 ISO 时间戳，而且这个 manifest 根本没被使用。提示词里只有 `BEFORE_RECOMMENDING` 一段泛化告诫（"memory 说 X 存在 ≠ X 现在存在"），**没有任何逐条记忆的、带具体天数的陈旧性标注**。这是"知道要防"和"真的防住"之间的差距。

### 2.5 提示词装配：claude-code 多路径分派，agentrs 完整档不可达

claude-code 的 `buildMemoryLines`（`memdir.ts:199`）支持：
- `skipIndex` 变体（daily-log 模式下不需要索引指令）
- `extraGuidelines` 注入（team 模式追加）
- 由 `loadMemoryPrompt`（:419）按 feature flag 分派：auto+team 合并 / auto-only / daily-log 模式

另有 `buildSearchingPastContextSection`（:375）**显式教模型如何检索过去的上下文**：
1. 先 grep 记忆目录的 `*.md`
2. 实在不行再 grep 项目目录的 `*.jsonl` transcript（注明"最后手段——文件大、慢"）
3. 并根据是否有内嵌搜索工具，在 Grep 工具形式与 shell `grep -rn` 形式之间切换
4. 提示"用窄搜索词（错误信息、文件路径、函数名），别用宽泛关键词"

**agentrs 的状况**：有对应的 `build_memory_prompt`（完整档 ~2650 token）与 `build_memory_prompt_minimal`（精简档 ~150 token），但**只有精简档被调用**（`context.rs:288`），完整档零生产引用。`context.rs:282` 的注释声称"模型首次写入记忆目录时按需升级为完整档"——**该触发点从未实现**。因此四类语义定义、8 组 few-shot、禁存清单、两步保存流程、`Why:/How to apply:` 结构要求，模型一次也看不到。也没有"如何检索过去上下文"的任何指引。

### 2.6 权限边界

claude-code 把记忆目录做成权限系统的显式 carve-out：`isAutoMemPath` 在 `utils/permissions/filesystem.ts:1572`（写）和 `:1716`（读）作为前置放行点。注释还说明了一个精细边界：

> 默认路径在 `~/.claude/` 下、属于 `DANGEROUS_DIRECTORIES`，所以需要这个 carve-out；但**用户自定义的 override 路径不给特殊待遇**——那是调用方指定的任意目录，没有这个冲突，走正常权限流程询问用户。SDK 调用方想要静默写入，应自行为 override 路径加 allow 规则。

**agentrs 的状况**：`is_memory_path` / `validate_memory_path` / `ensure_memory_dir` **全部零生产调用**。记忆目录对 Write 工具而言就是普通目录：既没有免打扰的放行，也没有任何写入边界或审计。更矛盾的是，提示词（`prompt.rs:24`）断言"This directory already exists"，而 `ensure_memory_dir` 从未被调用——**提示词与事实不符**。对比 claude-code 的处理：`buildMemoryPrompt` 注释专门写明"目录创建是调用方（`loadMemoryPrompt`）的责任，builder 只读不 mkdir"，职责边界是显式声明的。

### 2.7 claude-code 独有能力清单

| 能力 | 实现 | 关键设计 |
|---|---|---|
| **团队记忆同步** | `teamMemorySync/index.ts` 1256 行 | 按 git remote hash 分 scope；ETag + per-key checksum 增量上传；server-wins 拉取；**删除不传播**（本地删文件不会删服务端，下次 pull 会恢复）——语义在文件头注释中明确声明 |
| **上传前密钥扫描** | `secretScanner.ts` 324 行 | 移植 gitleaks 高置信度规则子集（**只取有独特前缀、近零误报的规则**，泛化的关键词上下文规则一律不要）；Go 正则的 `(?i)` 内联模式不可移植到 JS，逐条改写为显式字符类；**密钥不出本机** |
| **文件变更监听** | `watcher.ts` 387 行 | 本地改动触发同步，带 suppression 防回环 |
| **路径注入防护** | `teamMemPaths.ts` 292 行 | `sanitizePathKey` 拒绝空字节、**URL 编码穿越**（`%2e%2e%2f`）、畸形百分号编码；`PathTraversalError` 专用异常；`lstat`/`realpath` 校验 |
| **会话内记忆** | `SessionMemory/` ~700 行 | 周期性 fork 子 agent 维护当前会话笔记，与长期记忆分层；有初始化阈值、更新阈值、token 计数 |
| **子 Agent 记忆** | `AgentTool/agentMemory.ts` + `agentMemorySnapshot.ts` | 子 agent 独立记忆空间与快照 |
| **灰度与遥测** | `tengu_*` GrowthBook flag、`memoryShapeTelemetry` | **空选择也上报**——"选择率需要分母，`-1` 年龄区分'跑了但没选中'和'根本没跑'"（`findRelevantMemories.ts:64-65`） |
| **用户界面** | `/memory` 命令、`MemoryUpdateNotification`、`MemoryFileSelector`、`MemoryUsageIndicator`、`useMemorySurvey` | 记忆可见、可审、可改，且收集用户反馈 |

**agentrs 的状况**：以上全部没有。只有 `PromptUsage` 里的 `memory_tokens` / `memory_files` 统计（`context_usage.rs:119`），无命令、无 UI、无遥测、无灰度开关——连 `enabled` 配置项都没有，只能靠 `AGENTRS_MEMORY_DIR` 环境变量间接影响。

---

## 3. 与 pi 的对比：pi 根本没有长期记忆

### 3.1 事实澄清

`pi/packages/agent/src/harness/session/memory.ts`（192 行）名字里有 memory，但它是 **`InMemorySessionStorage`**——`SessionStorage` 接口的内存实现，提供 lane / entry / record / fork 抽象（`appendEntry`、`appendRecord`、`findEntriesOnBranch`、`getLog`…）。这是会话持久化基础设施，与跨会话记忆无关。

全仓库检索 `long-term memory` / `longTermMemory` / `memoryDir` / `MEMORY.md`：**零命中**。

### 3.2 pi 的跨会话上下文机制

唯一路径在 `packages/coding-agent/src/core/resource-loader.ts:71`：

```ts
const candidates = ["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"]
```

配套有 `findShadowedContextFile`（:100，检测被遮蔽的上下文文件并诊断）和 `loadProjectContextFiles`（:118）。

### 3.3 哲学差异

pi 认为跨会话应该持久化的是**人写的项目约定**和**可分叉的会话日志**，而不是 agent 自己对用户的推断。

- **规避的问题**：一致性、陈旧性、隐私、误记、记忆污染——全部不存在。
- **付出的代价**：agent 永远不会主动积累对用户的理解；每个新用户/新项目的适配成本由人承担。
- **对 agentrs 的启示**：如果记忆闭环无法在短期内补齐，"暂时下线记忆、只保留 AGENTS.md"是一个诚实且有先例的选项，比留一个半成品更好。

---

## 4. 与 opencode 的对比：把"记忆"重新定义为可版本化的指令

### 4.1 机制

`packages/opencode/src/session/instruction.ts`（237 行）同样没有自动记忆，但指令加载比 pi 完整：

| 层 | 内容 | 位置 |
|---|---|---|
| 全局 | `~/.config/opencode/AGENTS.md` | :60-63 |
| 项目 | `AGENTS.md` 等，**第一个匹配的项目级文件胜出** | :64-69, :122 |
| 配置 | `config.instructions`：glob 模式 + `~/` 展开 + **HTTP URL 远程拉取** | :138-162 |

两个设计细节：

**(a) 刻意不做祖先叠加**（:122 注释）
> "The first project-level match wins so we don't stack AGENTS.md/CLAUDE.md from every ancestor."

对比 agentrs 的 `agents_md::collect_agents_md`（层级收集）——两者是有意识的相反取舍：opencode 优先避免上下文膨胀，agentrs 优先保证不遗漏上层约定。

**(b) 远程指令带降级**（:95-103）
HTTP 拉取有 transient retry，失败时降级为空 ArrayBuffer 而非中断——与 agentrs `read_index` 失败返回空串是同一种容错思路。

### 4.2 哲学差异

opencode 把"团队共享的 agent 知识"做成**远程 URL 指令**：一个团队维护一份 markdown，所有人的 agent 拉同一份。

这与 claude-code 的 `teamMemorySync`（agent 自写 + 服务端同步 + 密钥扫描 + 冲突语义 + watcher，共 2100+ 行）解决**同一个问题**，但方案轻了两个数量级，而且：

- 内容是人写的 → 天然可 review、可版本化、可 PR 讨论
- 不需要密钥扫描 → 人不会把 AWS key 写进团队规范文件（写了也在 code review 拦住）
- 不需要冲突解决 → git 已经解决了

**对 agentrs 的启示**：如果未来要做团队记忆，"远程指令 URL"比"服务端同步 agent 自写内容"的成本低一个数量级，且风险面小得多。

---

## 5. 与 deepseek-harness 的对比：明确拒绝把记忆做进产品

### 5.1 立场

DSH 的决策记录在 `.agents/notes/implemented/feature/2026-07-31-third-party-memory-mcp-examples.md`，是五者中态度最鲜明的：

> 直接的厂商集成会把某一家的 API、配置、健康检查行为和工具语义变成 DSH 的一部分。对于一个**已经可以通过 MCP 表达的能力**，这是过多的产品表面，而且每接一个记忆系统就要重复一遍适配。

落地方式：三个 **default-off** 的 Cordis overlay 样例（`examples/mcp-memory/`），每个文件只插入一行通用 `@deepseek-ai/dsh-mcp-client`：

- Memorix（npm 1.3.0，锁 commit）
- MCP Reference Memory（npm 2026.7.4，锁 commit）
- Engram（tag v1.20.0，锁 commit）

免责声明写得很重：**不构成背书、推荐、合作或持续支持**；没有记忆预设注册表、没有厂商专用插件、没有通用记忆服务、没有安装 UI、没有迁移层、没有健康检查器、没有重连控制器。

### 5.2 责任边界（原文表格）

| 关注点 | DSH 负责 | 上游 provider / 用户负责 |
|---|---|---|
| 解析选定的 overlay | ✅ | 选择一个文件 |
| 启动 stdio 命令并在插件释放时停止 | ✅ | 安装锁定版本的可执行文件 |
| 连接 Streamable HTTP 并发现工具 | ✅ | 运行和监管 HTTP 服务 |
| 注册工具为 `mcp__<serverName>__<rawName>` | ✅ | 定义工具 schema 与行为 |
| 账号、认证、模型、embedding、存储初始化 | ❌ | ✅ |
| 厂商数据迁移、重试、崩溃恢复 | ❌ | ✅ |

存储和项目身份也归 provider：Memorix 用 `~/.memorix/data`，Engram 用 `~/.engram`；Reference Memory 样例特意把路径设为 `$HOME/.dsh-mcp-reference-memory.jsonl`，**避免写进已安装的 npm 包目录**。

### 5.3 三个值得直接借鉴的做法

**(a) 不 patch system prompt**
理由很具体：config patch 会**整体替换**一行的 config，可能抹掉用户已有的 persona。因此只在 README 里给一句可选的**附加**指令：

> 当用户要求记住某事时，调用记忆写入工具。当历史信息可能相关时，搜索记忆并使用相关结果。

provider 自己的工具描述才是权威。

**(b) 验收标准是端到端行为，不是"socket 连上了"**
合并前必须为每个 provider 提供人工证据：
1. DSH session A 调用写入工具，为一个唯一值收到成功响应；
2. **全新的 session B**（同一 provider 存储 scope，**不带 A 的 transcript**）调用 search/recall 返回该值；
3. session B 在后续回答中**用上**这个值。

三步缺一不可。这是本报告中唯一一份把"记忆真的起作用了"写成可执行验收条款的文档。

**(c) CI 永不接触第三方服务**
无密钥测试套件解析三个 overlay、检查通用桥接与密钥边界、用包内自带的 MCP fixture server 替换上游端点、启动真实 Cordis Loader、验证工具发现。通用 stdio 传输层主动**擦除凭据形状的环境变量和 `DSH_*`**，只继承其余 ambient 变量。

### 5.4 哲学差异

DSH 认为记忆是**可插拔外设**，不是产品表面。

- **收益**：换任何记忆后端只改一个 yml；维护负担趋近于零；不承担任何记忆质量责任。
- **代价**：没有开箱即用的记忆体验；记忆质量完全取决于用户选的 provider；缺少与主循环深度集成的机会（如 claude-code 的 forked-agent 抽取）。

---

## 6. agentrs-memory 的技术不足与缺陷

按严重程度分四层。

### 层一：范式层面的结构性缺失（最根本）

agentrs 选了最重的路线，却只实现了"启动时读一个索引文件"。生产链路的全部内容是：

```
bootstrap.rs:214  auto_memory_dir(workspace)
context.rs:288    build_memory_prompt_minimal(dir)
                      └─ read_index() → truncate_index() → 拼进 system prompt
```

| 缺失能力 | 后果 | 对标 |
|---|---|---|
| **无相关性召回** | 规模上限锁死 200 行 / 25KB，超出无降级路径 | `findRelevantMemories.ts` |
| **无自动抽取** | 记忆产生完全靠模型自觉；用户不主动说"记住"，目录大概率永远空 | `extractMemories.ts` |
| **无逐条新鲜度标注** | 陈旧 `file:line` 断言被当作事实——claude-code 用注释记录的真实事故 | `memoryAge.ts` |
| **无权限边界** | 记忆目录无放行也无保护，写入无审计 | `filesystem.ts:1572/1716` |
| **无检索指引** | 索引装不下时，模型不知道该怎么找 | `buildSearchingPastContextSection` |
| **无用户界面** | 记忆对用户是黑盒，不可见、不可审、不可改 | `/memory` + 3 个组件 |
| **无开关与灰度** | 出问题无法快速关闭（无 `enabled` 配置项） | `isAutoMemoryEnabled` + GrowthBook |
| **无遥测** | 无法回答"记忆到底有没有被用到"这个最基本的问题 | `memoryShapeTelemetry` |

### 层二：已实现但未接线（P0 — 改动量最小、收益最大）

全仓库检索确认，以下 API **仅测试引用，生产零调用**：

```
store::{read_memory, write_memory, delete_memory, scan_memory_files, format_memory_manifest}
paths::{is_memory_path, validate_memory_path, ensure_memory_dir}
prompt::{build_memory_prompt, build_memory_instructions, memory_type_descriptions}
MemoryError::FrontmatterParse            // 定义但从未构造
```

派生的三个具体问题：

| # | 问题 | 影响 | 证据 |
|---|---|---|---|
| 1 | **完整提示词永远不可达** | 模型只有 150 token 精简规则，连"文件名怎么起"都没说；四类语义、few-shot、禁存清单、正文结构要求全部缺席 | `context.rs:282` 注释与实现不符，无 `is_memory_path` 调用点 |
| 2 | **会话内索引不刷新** | 本轮保存的记忆当轮不可见，提示词"保存后会出现在这里"当场落空，反而诱发重复写入 | `bootstrap.rs` 一次性写死 `config.system_prompt`；`invalidate("memory")` 仅见于 `context_test.rs:944` |
| 3 | **索引与正文无对账** | 文件名与索引行均由模型手工产出，无孤儿检测、无 GC | `generate_filename`（`store.rs:233`）从未被调用 |

### 层三：正确性风险（P1）

- **项目路径碰撞**（`paths.rs:148`）
  `sanitize_path` 把非 ASCII 全替换为 `-`，**只有超过 200 字符才追加哈希**。`/home/张三/proj` 与 `/home/李四/proj` 清洗后完全同名，两个项目静默共享一份记忆。中文环境下这是**必然触发**而非理论风险。修复只需无条件追加原始路径哈希。
  对比：claude-code 的 `teamMemPaths.ts` 有专门的 `sanitizePathKey` + `PathTraversalError`，还防 URL 编码穿越。

- **索引写入非原子**（`index.rs:131`）
  read-modify-write，无锁、无 temp+rename。多会话并发写索引会丢条目。
  对比：teamMemorySync 用 per-key checksum + upsert 语义规避整文件覆写。

- **`remove_index_entry` 子串误匹配**（`index.rs:165`）
  以 `(filename)` 做 `contains` 判断，`(a.md)` 会误删含 `(xa.md)` 的行。

### 层四：健壮性与一致性（P2）

- **目录扫描无防护**（`store.rs:303`）：递归无深度限制、无符号链接检测（symlink 环会无限递归）；200 文件上限在**全量遍历并读完每个文件前 30 行之后**才生效，大目录开销不受控。
  对比：`memoryScan.ts:45` 用 `Promise.allSettled` 让单文件失败不影响整体，且注释解释了"读后排序"而非"stat-排序-读"的 syscall 权衡（`readFileInRange` 内部已 stat 并返回 mtimeMs，常见情况下减半 syscall）。
- **截断告警文案不一致**（`index.rs:103`）：行数与字节同时超限的分支丢失了 limit 说明。
- **`FrontmatterParse` 死变体**：解析失败实际走 `warn` + 默认值，该错误分支永不构造。
- **MSRV 隐性约束**：`floor_char_boundary` 需要较新工具链（当前基线 rustc 1.96.1 可用），未在文档标注。

---

## 7. 路线建议

### 路线 A：补齐闭环（沿当前范式）

按投入产出比排序：

| 优先级 | 事项 | 说明 |
|---|---|---|
| P0 | **接上会话内失效** | Write/Edit 落盘后若 `is_memory_path` 命中 → `cache.invalidate("memory")`，并在首次命中时升级到 `build_memory_instructions()` 完整档。**一处改动同时解决层二的问题 1 和 2**，顺带激活死代码 `is_memory_path` |
| P0 | **`sanitize_path` 无条件加哈希** | 消除跨项目记忆污染，中文环境必修 |
| P1 | **接上召回层** | `scan_memory_files` + `format_memory_manifest` 已现成，补一个轻量侧查询（可用本地 27B 端点，成本可忽略）选 3–5 条注入。**这是解除 25KB 天花板的唯一路径** |
| P1 | **移植 `memoryAge`** | ISO 时间戳 → "N 天前" + 陈旧性告警。约 30 行代码，直接解决最容易造成实际损害的失败模式 |
| P1 | **索引写入原子化** | temp file + rename，或文件锁 |
| P1 | **`remove_index_entry` 精确匹配** | Markdown 链接解析或行首锚定 |
| P2 | **自动抽取** | 借鉴 `extractMemories` 的三件套：stop-hook 触发、主/后台互斥保护、受限工具策略 |
| P2 | **决定 `store.rs` 去留** | 要么宿主暴露 Memory 工具走它（换取命名规范、frontmatter 校验、200 文件上限真正生效），要么按 AGENTS.md 可见性规范收窄，避免死代码伪装成能力 |
| P2 | **加 `enabled` 开关 + 基础遥测** | 出问题能关；能回答"记忆有没有被用到" |

### 路线 B：收缩（借鉴 pi / opencode）

暂时下线自动记忆，只保留 AGENTS.md 层级加载 + 可选的远程指令 URL。诚实且有先例，比留一个半成品更好。代价是放弃已投入的 4200 行代码（含测试）。

### 路线 C：外包（借鉴 deepseek-harness）

删掉 crate，改为提供一个记忆 MCP 的配置样例，并采用 DSH 的三条做法：不 patch system prompt、验收标准写成"session A 写 / 全新 session B 召回并使用"、CI 用 fixture server 不碰第三方。维护负担趋近于零。

### 推荐

**路线 A 的 P0 两项（约半天工作量）应无条件先做**——它们把当前这个"读一半"的系统变成一个至少自洽的系统，且不预设后续路线。

之后再基于"记忆是否真的被用到"的遥测数据，在 A 的 P1/P2 与 B/C 之间做产品决策。在没有遥测的情况下讨论要不要投入自动抽取，是没有依据的。

---

## 附录：证据索引

| 结论 | 证据位置 |
|---|---|
| agentrs 召回层缺失 | `store.rs:86`、`store.rs:117` 零生产调用；对照 `claude-code/src/memdir/findRelevantMemories.ts:39` |
| agentrs 完整提示词不可达 | `prompt.rs:266/295/335` 零生产调用；`context.rs:282` 注释与实现不符 |
| agentrs 会话内不刷新 | `bootstrap.rs` 写死 `config.system_prompt`；`invalidate("memory")` 仅 `context_test.rs:944` |
| agentrs 权限校验是死代码 | `paths.rs:80/114/96` 零生产调用；对照 `claude-code/src/utils/permissions/filesystem.ts:1572,1716` |
| 路径碰撞风险 | `paths.rs:148-160`，`MAX_SANITIZED_LENGTH=200` 且短路径不加哈希 |
| 索引写入非原子 | `index.rs:131-147` |
| claude-code 后台抽取 | `services/extractMemories/extractMemories.ts:121,171,611` |
| claude-code 新鲜度设计动因 | `memdir/memoryAge.ts:11-14,29-31` |
| claude-code 团队同步语义 | `services/teamMemorySync/index.ts:1-25` 文件头注释 |
| claude-code 密钥扫描来源 | `services/teamMemorySync/secretScanner.ts:1-20` |
| pi 无长期记忆 | `packages/agent/src/harness/session/memory.ts` 为 `InMemorySessionStorage`；`resource-loader.ts:71` |
| opencode 指令加载 | `packages/opencode/src/session/instruction.ts:60-69,122,138-162` |
| DSH 记忆外包决策 | `.agents/notes/implemented/feature/2026-07-31-third-party-memory-mcp-examples.md` |

# AgentRS 框架重构执行方案

> 诊断基线：工作树当前状态（HEAD = `c52cd71`），463 个 `.rs`，19 个 crate。
> 结论先行：**当前 workspace 连 `cargo metadata` 都无法解析**，仓库处于一次
> 未完成的"上游全量并入"中途。本方案的目标不是"优化"，而是**先把仓库拉回可编译，
> 再把两套并存的架构收敛成一套**。

---

## 一、现状诊断（已核实的事实）

### 1.1 workspace 当前是破的

```
$ cargo metadata --no-deps
error: failed to load manifest for workspace member `crates/agentrs-adapter`
Caused by: failed to read `crates/agentrs-testkit/Cargo.toml`: No such file or directory
```

`crates/*` 中被引用但**目录不存在**的 crate 有四个：

| 被引用的 crate | 引用者 | HEAD 中是否存在 |
|---|---|---|
| `agentrs-testkit` | `agentrs-context`、`agentrs-runtime`、`agentrs-adapter` | 存在（工作树删了 19 个文件） |
| `agentrs-prompts` | `agentrs-runtime`、`agentrs-subagents` | 存在（工作树删了 5 个文件） |
| `agentrs-provider` | `agentrs-subagents`、`agentrs-tui` | 存在（工作树删了 14 个文件） |
| `agentrs-dev-adapter` | `agentrs-tui` | 已改名为 `agentrs-adapter`，引用未跟着改 |

### 1.2 两套完整的 Agent 架构并存

HEAD 是一套「内核架构」（contracts 驱动、无执行权、纯函数可测）；工作树把上游
`iOfficeAI/agentrs @ f711174` 的 13 个 crate **原样并入**，形成第二套「传统架构」
（直接碰 fs/进程/网络、全局 Config、tokio 到处渗透）。两套在同一 workspace 里各跑各的：

| 关注点 | 内核栈（HEAD 血统） | 并入栈（上游血统） |
|---|---|---|
| 契约/类型 | `agentrs-contracts` | `agentrs-types`、`agentrs-protocol` |
| 状态机 | `agentrs-runtime`（engine 2259 / host 1641 / toolround 1376） | `agentrs-agent`（engine 1735 / turn / stream / orchestration 593） |
| 上下文 | `agentrs-context`（assembler / budget / cache / compaction） | `agentrs-agent`（context / context_usage / cache_diagnostics） |
| 压缩 | `agentrs-context::compact` + `::strategy` | `agentrs-compact` + `agentrs-agent::compact` |
| Provider | `agentrs-provider`（已被删） | `agentrs-providers` |
| 工具 | `agentrs-tools`（kernel 版，`src/builtin/`） | `agentrs-tools`（上游版，扁平 `edit.rs`/`read.rs`…） |
| 技能 | `agentrs-skills`（`src/pack/`） | `agentrs-skills`（扁平） |
| 执行宿主 | `agentrs-adapter` | 无（上游直接在工具里执行） |
| 交互宿主 | `agentrs-tui`（原 dev-tui） | 上游 `agentrs-tui`（**已被同名覆盖丢失**） |
| 配置 | 由 `RunSpec` 注入 | `agentrs-config`（1159 行全局 Config） |

### 1.3 crate 名撞车造成的三处实质破坏

内核 crate 与上游 crate **共用 `agentrs-` 前缀**，并入时按名字覆盖：

1. **`agentrs-tui` 被覆盖**：`agentrs-cli/src/run.rs:13` 用
   `agentrs_tui::{TuiMetadata, TuiOutcome, TuiRuntime, TuiSession}` ——
   这四个符号在现在的 `agentrs-tui` 里**一个都不存在**。即便补齐 1.1 的四个缺失
   crate，`agentrs-cli` 也编不过。
2. **`agentrs-types` / `agentrs-tools` / `agentrs-skills` 被混合**：同一 crate 里
   一半是内核血统、一半是上游血统（`agentrs-tools` 同时有 `src/builtin/mod.rs` 与
   扁平的 `edit.rs`/`read.rs`；`agentrs-skills` 同时有 `src/pack/` 与扁平模块）。
3. **两条架构门禁全部失效**——这是最严重的一条：

   ```
   scripts/check-port-attribution.sh:  find crates -name '*.rs' -not -path 'crates/agentrs-*'
   → 扫描到 0 个文件（应为 463 个中的内核部分）
   scripts/check-no-env.sh:            grep -v '^crates/agentrs-'
   → 排除了全部 19 个 crate
   ```

   两个脚本都写着「豁免上游并入的 `crates/agentrs-*`」，但**本仓库自己的 crate 也叫
   `agentrs-*`**，于是豁免规则吃掉了全部输入。两个脚本现在都**必然返回"检查通过"**，
   而它们守的正是"内核不碰环境"和"移植必须标注归属（Apache-2.0 §4b）"这两条底线。
   `THIRD-PARTY-NOTICES.md` 里"`agentrs-*` 那 15 个 crate 的判据一个字也没放松"这句话，
   在当前脚本下是不成立的。

### 1.4 重复实现清单（合并的具体对象）

**A. 上下文与压缩 —— 同一件事有 3 套实现、4 套 token 估算**

| 职责 | 实现 1 | 实现 2 | 实现 3 |
|---|---|---|---|
| 工具输出压缩（进上下文前） | `agentrs-context/src/compact/{fold,json,level,sanitize,toon}.rs` | `agentrs-compact/src/{同名}.rs` | — |
| 对话历史压缩策略 | `agentrs-context/src/strategy/{auto,micro,emergency,state,prompt,estimate,usage}.rs` | `agentrs-agent/src/compact/{auto,micro,emergency,state,prompt,estimate}.rs` + `context_usage.rs` | — |
| 压缩"该压哪一段"的规划 | `agentrs-context/src/compaction.rs`（纯函数、四段模型） | `agentrs-agent/src/engine.rs` 内联判断 | — |
| 摘要生成 | `agentrs-subagents/src/compact.rs`（fail-closed、校验必填项） | `agentrs-agent/src/compact/auto.rs`（直接调 `LlmProvider`） | — |
| 缓存断裂归因 | `agentrs-context/src/cache_diagnostics.rs`（381 行） | `agentrs-agent/src/cache_diagnostics.rs`（164 行） | — |
| 压缩配置 | `agentrs-context/src/strategy/config.rs` | `agentrs-config/src/compact.rs` | — |
| token 估算 | `agentrs-context/src/budget.rs`（`ConservativeEstimator`） | `agentrs-context/src/strategy/estimate.rs` | `agentrs-agent/src/context_usage.rs` + `agentrs-contracts::ports::TokenCounter` |

`agentrs-context/src/compact/` 与 `strategy/` 的文件头已写明
`Ported from agentrs … Source: crates/agentrs-compact/… @ f711174`，
**说明合并已经开始、只是没有删除源端**。`THIRD-PARTY-NOTICES.md` 的移植清单里
这几行也已登记为 ✅。所以这一块不是"要不要合"，而是"把已完成的一半收尾"。

**B. Agent Runtime / Runner —— 两套 Turn/Step 循环**

| 职责 | `agentrs-runtime`（保留） | `agentrs-agent`（淘汰） |
|---|---|---|
| Turn/Step 状态机 | `engine.rs` — Turn/Step 精确定义、四态 `StepOutcome`、零 Step Turn 也落 durable | `engine.rs` + `turn.rs` — 循环内联在 `AgentEngine::run` |
| 工具回合 | `toolround.rs` — 五段固定管线 `StepIntent→schema→guard→Hook→Policy→Sandbox→StepResult` | `orchestration.rs` — `execute_tool_calls_*` 三个重载 |
| 审批 | `toolround.rs` 有界等待 + `Suspended{token}` 挂起 | `confirm.rs` + `agentrs-protocol::ToolApprovalManager`，无有界等待 |
| 权限模式 | `permission.rs`（542 行）+ `plan_state.rs` | `tool_policy.rs`（35 行）+ `plan/state.rs`（17 行） |
| 并发/顺序 | `schedule.rs` ordering barrier | 无 |
| 恢复 | `recovery.rs` 六个 durable 边界 | 无 |
| 分叉 | `fork.rs` | 无 |
| 会话 | 由 `host.rs` + `inbox.rs` 承担 | `session.rs`（588 行，直接读写磁盘） |
| 子 Agent | `agentrs-subagents`（函数式、MemberRun） | `spawner.rs` + `spawn_tool.rs` |
| 换代 | `generation.rs`（component generation 原子换代） | 无 |

`agentrs-runtime` 在恢复、审批边界、不变式、换代上是**严格超集**；
`agentrs-agent` 的价值在于**已经跑通的实战细节**（空 final 重试、工具调用失败指纹与
熔断、`ProviderCompat`、slash 命令、输出 sink），这些是要被**吸收**而不是被丢弃的。

**C. 其余重复**

- 策略/权限：`contracts/policy.rs`(238) / `runtime/permission.rs`(542) /
  `adapter/policy.rs`(184) / `agent/tool_policy.rs`(35) —— 四处各自定义"允不允许"。
- MCP 线格式：`agentrs-mcp/src/protocol.rs` 与 `agentrs-tools/src/mcp/wire.rs`。
- Glob：`agentrs-types/src/glob.rs` 与 `glob` crate（`agentrs-skills` 扁平版用后者）。
- 进程执行：`agentrs-process/src/runner.rs` 与 `agentrs-adapter/src/exec.rs`。

---

## 二、框架设计上不合理的地方

按"改动成本 × 危害"排序。前四条是**必须改**，后四条是**建议改**。

### ① 门禁被自身命名规则关掉（致命）

见 §1.3.3。一个架构如果靠"内核不碰环境"立身，而这条判据的执行器恒真，那么
架构约束实际已经不存在。**并且这是静默的**——脚本照常打印"检查通过"。

**根因是命名**：用 `crates/agentrs-*` 这种**路径前缀**来表达"这是外来代码"，
而本仓库自己的 crate 也叫这个前缀。前缀不是分类，目录才是。

> 修法：外来代码进 `vendor/`（或 crate 名加 `upstream-` 前缀），
> 豁免规则改为按目录而非按名字前缀；并给两个脚本加上**自检**——
> 扫描到 0 个文件必须报错退出，而不是打印通过。

### ② "全量并入 13 个 crate"作为集成手段本身不成立

`THIRD-PARTY-NOTICES.md` 写"原样并入、未做任何改写"，但并入的对象与内核 crate
**同名**，于是三个 crate 被就地混血、一个（`agentrs-tui`）被整体覆盖丢失。
"原样并入"这个前提在执行的第一步就已经破了，而 notices 仍按它描述现状。

更深的问题是**动机与手段错配**：真正需要的是"把上游若干模块移植进内核"
（notices 的"移植清单"正是这么做的，做得很好——逐文件标注、逐项写清改了什么、
甚至标出了修掉的上游缺陷）。而"全量并入"把一个**参考源**变成了**第二个生产实现**，
于是每个模块都有两个可编译、可运行、语义不同的版本。

> 修法：上游代码降级为**只读参考源**，不参与 workspace 编译。

### ③ 上下文压缩的分层被反向依赖破坏

内核栈的分层是对的：`context` 只做**纯计划**（"该遮蔽哪一段、压了有没有用"），
摘要生成这种要调模型的活交给 `agentrs-subagents`，用 `Summarizer` trait 注入
（notices 第 94 行明确写了这个决定的理由）。这是这套架构里最好的一个设计。

但并入栈的 `agentrs-agent/src/compact/auto.rs` 直接持有 `LlmProvider`，
`agentrs-config` 又被 `agentrs-tools`/`agentrs-skills`/`agentrs-mcp`/`agentrs-providers`
四个 crate 依赖——**一个全局 Config 横穿所有层**。两套一起编译时，
"context 不依赖 provider"这条只在半个 workspace 里成立。

### ④ `agentrs-types` 与 `agentrs-contracts` 的边界没有定义

`agentrs-runtime/src/engine.rs` 同时 `use agentrs_contracts::{event,ids,ports}` 和
`use agentrs_types::{LlmEvent, LlmRequest, StopReason, TokenUsage}`，
而 `agentrs-contracts` 的文档说自己"不依赖任何业务 crate"，`agentrs-types` 的文档说
"No dependencies on other agentrs-* crates"。两个都声称是底座，**互不依赖，谁也管不着谁**，
于是同一个概念在两边各有一份：`contracts::surface::SurfaceNode` vs
`types::message::Message`，`contracts::ports::TokenCounter` vs
`types::message::TokenUsage`。

> 修法：定一个方向。建议 `contracts → types`（契约层引用线上数据模型），
> `types` 保持零依赖。`Message`/`LlmRequest`/`LlmEvent` 只留一份，在 `types`。

### ⑤ 四个地方各自定义"允不允许"

`contracts/policy.rs` 定义 `PolicyDecision`/`SandboxGrant`（契约，正确）；
`runtime/permission.rs` 542 行实现 PermissionMode + Plan Mode；
`adapter/policy.rs` 又实现一遍宿主侧判定；`agent/tool_policy.rs` 再来一个 35 行的。
`toolround.rs` 的文档说"★Policy 不可绕过、middleware 不得改变授权结论"——
但当有四个 Policy 时，这句话保护不了任何东西。

> 修法：判定逻辑**只在 `runtime/permission.rs` 一处**；`contracts` 只留类型；
> `adapter` 只实现 `Policy` port 的**宿主特化部分**（哪些路径可写），
> 不重复实现模式语义。

### ⑥ `agentrs-context` 内部已经有三个重叠的压缩模块

`compact/`（工具输出）、`compaction.rs`（历史遮蔽规划）、`strategy/`（auto/micro/emergency
三档守卫）。前两个的文档互相解释了"我们不是一回事"，写得很清楚；但 `strategy/`
是后并进来的第三份，它的 `auto`/`micro`/`emergency` 与 `compaction.rs` 的
"四段模型"**是同一件事的两种切法**（`micro` ≙ 第 2 段，`auto` ≙ 第 3 段，
`emergency` ≙ 溢出触发）。同一个 crate 里两套词汇描述同一个决策，
调用方无从判断该用哪个。

> 修法：`strategy/` 的三档降级为 `compaction.rs` 四段模型的**触发条件**，
> 不再是并列的第二套 API。对外只暴露一个入口：
> `plan_compaction(history, budget, trigger) -> CompactionPlan`。

### ⑦ `agentrs-context/src/strategy/mod.rs` 顶上有 `#![allow(missing_docs)]`

```rust
#![allow(missing_docs, reason = "agentrs 逐字移植，文档待二次优化补齐")]
```

workspace 设了 `missing_docs = "warn"`，这一行把整个模块（9 个文件）豁免掉了。
"待二次优化"的临时豁免没有到期机制，而这个模块恰好是全仓库语义最微妙的一块。

> 修法：这一行的存在期就是本次重构；收尾时必须删掉，且不允许新增同类豁免。

### ⑧ 门禁脚本的豁免名单已经指向不存在的 crate

`check-no-env.sh` 豁免 `agentrs-dev-adapter/`、`agentrs-dev-tui/src/host_io.rs`、
`agentrs-provider/`——这三个路径在工作树里**都不存在了**（分别改名为
`agentrs-adapter`、`agentrs-tui`、已删除）。即使修好 ①，豁免也是错的。

> 修法：豁免名单改为在脚本里**校验路径存在**，路径消失即报错。

---

## 三、目标架构

### 3.1 一句话

**保留内核栈作为唯一生产架构；上游并入栈降级为只读参考源；
逐模块把上游已跑通的实战细节移植进内核，每移植一条就在 notices 里登记一条。**

理由：内核栈在恢复、审批边界、epoch 围栏、不变式、可测试性（无网络/无 fs/无真实时钟）
上是严格超集，且这些性质**是加不回去的**——一个直接 `std::fs` 的实现无法事后变成
可 replay 的。反过来，上游栈的优势（provider 兼容矩阵、TUI、工具实现细节）
**都是可以逐条搬的**。

### 3.2 目标 crate 与依赖方向（单向，无环）

```
L0  agentrs-types          纯数据：Message / LlmRequest / LlmEvent / ToolDef / schema / glob
                           零依赖
L0  agentrs-contracts      契约：ids / event / surface / manifest / content / policy /
                           sandbox / ports / spec / component / version
                           → agentrs-types
L1  agentrs-prompts        提示词注册表（版本号 + snapshot + 安全 lint）
                           → contracts
L1  agentrs-context        唯一的上下文层：assembler / budget / cache / cache_diagnostics /
                           compaction（四段规划，含触发条件）/ compact（工具输出）
                           → contracts, types            ← 不依赖 provider
L2  agentrs-provider       LLM seam（唯一允许网络）
                           → contracts, types
L2  agentrs-tools          工具语义（builtin + mcp wire + tool_policy 的 schema 部分）
                           → contracts, types
L2  agentrs-skills         技能解析（pack/，只解析不发现）
                           → contracts, types
L2  agentrs-memory         检索（纯逻辑，索引读写归宿主）
                           → contracts
L3  agentrs-runtime        内核状态机 + 唯一的权限判定
                           → contracts, types, context, prompts
L3  agentrs-subagents      函数式子 Agent / MemberRun / compact 摘要
                           → contracts, types, context, provider, prompts
L4  agentrs-observability  读模型投影 / 脱敏 / OTel
                           → contracts
L5  agentrs-adapter        宿主参考实现（唯一允许 fs/进程）
                           → contracts, types, tools, runtime
L5  agentrs-tui            交互测试宿主
                           → contracts, types, runtime, provider, adapter
L5  agentrs-cli            RunSpec 翻译层（唯一允许读环境变量）
                           → 全部
T   agentrs-testkit        测试假件（dev-dependency only）
```

**删除的 crate**：`agentrs-agent`、`agentrs-compact`、`agentrs-config`、
`agentrs-protocol`、`agentrs-process`、`agentrs-providers`、`agentrs-mcp`
（七个，全部来自并入栈；其内容按 §四逐条移植或丢弃）。

**新增约束**（写进 `clippy.toml` / 门禁）：
- `agentrs-context` 的 `Cargo.toml` 中出现 `agentrs-provider` 即为错误（分层反转）；
- 除 `agentrs-provider` 外任何 crate 出现 `reqwest` 即为错误；
- 除 `agentrs-adapter` / `agentrs-cli` 外任何 crate 出现 `std::fs` / `Command` 即为错误。

---

## 四、执行方案

七个阶段。**每个阶段结束时 workspace 必须可编译、全测试通过**——
不允许"下一阶段修好"的中间态，因为当前这个破状态正是这么来的。

---

### P0 — 止血：恢复到可编译（0.5 天）

目标：`cargo metadata` 与 `cargo build --workspace` 成功。**不做任何架构改动。**

1. 从 HEAD 恢复被误删的四个 crate：
   ```sh
   git checkout HEAD -- crates/agentrs-testkit crates/agentrs-prompts \
                        crates/agentrs-provider crates/agentrs-dev-adapter
   ```
2. 决定 `agentrs-adapter` vs `agentrs-dev-adapter` 改名去留：
   保留改名后的 `agentrs-adapter`，把 `agentrs-tui/Cargo.toml` 中
   `agentrs-dev-adapter = { path = "../agentrs-dev-adapter" }` 改为
   `agentrs-adapter = { path = "../agentrs-adapter" }`，并全局替换
   `agentrs_dev_adapter::` → `agentrs_adapter::`。删除重复的 `agentrs-dev-adapter/`。
3. 把并入栈的 13 个 crate **整体移出 workspace**：
   ```
   vendor/agentrs-upstream/{types,protocol,compact,process,config,providers,
                            tools,mcp,skills,memory,agent,tui,cli}
   ```
   `Cargo.toml` 的 `members` 从 `["crates/*"]` 改为显式列出内核 crate，
   `vendor/` 不进 members。**这一步之后并入栈不再编译**，它只是源码参考。
4. 冲突 crate（`agentrs-types` / `agentrs-tools` / `agentrs-skills`）的混血内容按血统拆开：
   - `agentrs-tools`：保留 `src/builtin/`、`src/mcp/`、`src/lint.rs`、`src/tool_policy.rs`；
     扁平的 `edit.rs`/`read.rs`/`write.rs`/`glob.rs`/`grep.rs`/`registry.rs`/
     `exec_command.rs`/`file_cache.rs`/`view_image.rs`/`tool_search.rs` 移入 vendor。
   - `agentrs-skills`：保留 `src/pack/`；扁平模块移入 vendor。
   - `agentrs-types`：保留 `glob.rs` / `schema.rs` / `compact.rs` / `file_state.rs` /
     `skill_types.rs` / `message.rs` / `llm.rs` / `tool.rs`；`spawner.rs` 移入 vendor
     （由 `agentrs-subagents` 承担）。
5. **修门禁**（这一步不能推迟到后面，否则 P1–P6 全程无保护）：
   - 两个脚本的豁免规则从 `crates/agentrs-*` 改为 `vendor/`；
   - 豁免名单里的每个路径加存在性校验，路径不存在则 `exit 1`；
   - 加自检：扫描文件数为 0 则 `exit 1`。

**验收**：`cargo build --workspace`、`cargo test --workspace`、
`cargo clippy --workspace --all-targets -- -D warnings`、两个 `scripts/*.sh` 全绿，
且 `check-port-attribution.sh` 报告的扫描文件数 > 0。

---

### P1 — 底座定向：`contracts` / `types` 边界（1 天）

对应问题 ④。

1. `agentrs-contracts/Cargo.toml` 加 `agentrs-types` 依赖，方向定为 `contracts → types`。
2. `agentrs-types` 保持零内部依赖（`lib.rs` 顶部那句注释改成机器可检的门禁）。
3. 消除同概念双份：
   - `TokenUsage` 只留 `types::message::TokenUsage`，`contracts` 引用它；
   - `contracts::surface::SurfaceNode` 明确文档：它是 ① durable 事实的节点，
     **不是** `types::message::Message`；两者的转换只允许在
     `runtime/surface.rs::derive_messages` 一处发生（已经是这样，补测试锁住）。
4. 加门禁：`grep 'agentrs-' crates/agentrs-types/Cargo.toml` 必须为空。

**验收**：全绿；新增一条测试断言 `derive_messages` 是 `SurfaceNode → Message` 的唯一出口。

---

### P2 — 上下文与压缩收敛（3 天）★ 核心

对应问题 ③⑥⑦，以及 §1.4-A。**`agentrs-context` 成为唯一的上下文层。**

#### P2.1 工具输出压缩（已完成 90%，只需收尾）

`agentrs-context/src/compact/` 已从 `agentrs-compact` 移植完毕并登记在案
（notices 第 67–72 行，含一处已修的中文相似度缺陷）。剩余动作：

- 确认 `agentrs-compact` 已在 P0 移入 vendor，删除 workspace 依赖项；
- 把 `agentrs-compact/src/*_test.rs` 的测试用例迁进 `context/src/compact/`
  （移植时只搬了实现，测试没搬；`fold.rs` 修掉的那个中文缺陷需要一条回归测试）。

#### P2.2 压缩策略：三档 → 四段（合并 `strategy/` 与 `compaction.rs`）

这是本阶段的实质工作。目标形状：

```rust
// agentrs-context/src/compaction/mod.rs —— 唯一入口
pub enum Trigger { Pressure, Overflow }

pub struct CompactionPlan { /* 遮蔽哪一段、为什么、makes_progress() */ }

/// 纯函数。输入历史 + 预算 + 触发源，输出计划。不调模型、不读时钟。
pub fn plan_compaction(
    history: &[SurfaceNode],
    budget: &Budget,
    trigger: Trigger,
    now: Timestamp,          // 由 Clock port 传入，不读真实时钟
) -> CompactionPlan;
```

映射关系（`strategy/` 的三档变成 `plan_compaction` 内部的**阶梯**，不再是并列 API）：

| 原 `strategy/` | 归入 | 说明 |
|---|---|---|
| `micro.rs::microcompact` / `should_microcompact` | 四段模型第 2 段 | 剪早期已消费工具全文 |
| `auto.rs::autocompact` / `should_autocompact` | 四段模型第 3 段 | 阈值触发的 LLM 摘要 |
| `auto.rs::Summarizer` trait | **移出 context** → `agentrs-subagents` | 摘要要调模型，属 live 资源 |
| `emergency.rs::is_at_emergency_limit` | `Trigger::Overflow` 的判据 | 不再是第三档 API |
| `state.rs::CompactState`（断路器） | `compaction/state.rs` | 与 `makes_progress()` 合并，防重试循环只留一条判据 |
| `estimate.rs` | **删除**，并入 `budget.rs::ConservativeEstimator` | 见 P2.3 |
| `usage.rs::ContextState/ContextStatus` | `context/usage.rs` | 呈现用，与决策分开 |
| `prompt.rs` | **移出 context** → `agentrs-prompts` | 提示词集中一处才能上版本号与 lint |
| `config.rs::CompactConfig` | `compaction/config.rs` | 由 `RunSpec` 注入，不再有全局 Config |

#### P2.3 token 估算收敛为一份

四份估算合并为：`agentrs-context/src/budget.rs` 里的 `ConservativeEstimator`
实现 `contracts::ports::TokenCounter`。`strategy/estimate.rs` 的
工具结果/图像估算逻辑（含 data URI base64 段估算，notices 第 92 行）
作为 `ConservativeEstimator` 的方法并入。`agent/context_usage.rs` 丢弃。

**判据**：全仓库 `grep -rn 'len() / 4\|chars().count() / 4'` 命中数 = 1。

#### P2.4 缓存归因去重

`agentrs-agent/src/cache_diagnostics.rs`（164 行）丢弃——
`agentrs-context/src/cache_diagnostics.rs`（381 行）已是它的移植超集
（notices 第 66 行："归因改走 cache.rs 分段；新增 Unsupported 判定"）。

#### P2.5 删掉 `#![allow(missing_docs)]`

`strategy/mod.rs` 的模块级豁免随 `strategy/` 目录一起消失；
合并后的 `compaction/` 每个公开项补文档。

**验收**：
- `agentrs-context` 对外只暴露一个压缩入口 `plan_compaction`（rustdoc 与 `pub use` 双向确认）；
- `agentrs-context/Cargo.toml` 不含 `agentrs-provider`、不含 `tokio`；
- 全仓库无第二份 token 估算；
- `#![allow(missing_docs)]` 计数为 0；
- `cargo test -p agentrs-context` 覆盖：四段各自的触发、`makes_progress()` 的
  防循环判据、摘要缺失时**不遮蔽**（fail-closed）。

---

### P3 — Runtime / Runner 收敛（4 天）★ 核心

对应 §1.4-B。**`agentrs-runtime` 是唯一的执行循环；`agentrs-agent` 的实战细节逐条移植。**

#### P3.1 先把 `agentrs-agent` 的价值清单固定下来

移植前先写清单（进 notices 移植表），逐条 review。已识别的必移植项：

| 上游能力 | 源文件 | 移入 | 为什么值得移 |
|---|---|---|---|
| 空 final 重试（模型把答案埋在 reasoning 里） | `agent/engine.rs` `EMPTY_FINAL_RETRY_PROMPT` | `runtime/engine.rs` `StepOutcome::EmptyFinal` 分支 | 真实端点上高频，内核目前只标记不恢复 |
| 工具调用失败/畸形指纹与熔断 | `agent/tool_call.rs`（400 行） | `runtime/toolround.rs` | 防模型在同一个错误上死循环 |
| Provider 兼容矩阵 | `config/compat.rs`（675 行） | `agentrs-provider/compat.rs`（已部分移植，notices 第 65 行） | 端点差异是现实 |
| 工具结果合并 `merge_tool_results` | `agent/tool_call.rs` | `runtime/toolround.rs` | 多调用同轮的正确聚合 |
| slash 命令注册表 | `agent/commands/` | `agentrs-tui`（宿主关注点，不进内核） | 交互能力 |
| 输出 sink / 格式化 | `agent/output/` | `agentrs-tui` + `agentrs-observability` | 同上 |
| AGENTS.md 装载 | `agent/agents_md.rs` | `agentrs-cli`（发现属宿主）+ `context`（装配） | 边界要拆开 |
| VCR 录放 | `agent/vcr.rs` | `agentrs-testkit`（已有 `adapter/vcr.rs`，去重） | 测试设施 |

**明确丢弃**：`agent/session.rs`（直接读写磁盘，职责由 `host.rs` + 宿主持久化承担）、
`agent/spawner.rs`（由 `agentrs-subagents` 承担）、`agent/plan/`（由
`runtime/permission.rs` + `plan_state.rs` 承担）、`agent/context.rs`
（由 `context/assembler.rs` 承担）。

#### P3.2 权限判定收敛为一处

对应问题 ⑤。
- `contracts/policy.rs`：只留类型（`PolicyDecision`/`SandboxGrant`/`DenyCode`/`ToolProposal`），
  删除任何判定函数；
- `runtime/permission.rs`：唯一的 PermissionMode + Plan Mode 语义；
- `adapter/policy.rs`：只实现 `Policy` port 的宿主特化（路径白名单、网络围栏），
  **不重新定义模式**；
- `agent/tool_policy.rs` → 已作为 `agentrs-tools/src/tool_policy.rs` 移植（notices 第 90 行），
  确认它只做**工具自身的静态声明**（这个工具是不是只读），不做模式判定。

**判据**：`grep -rn "enum PermissionMode"` 命中 = 1。

#### P3.3 会话与 inbox

`runtime/host.rs` + `inbox.rs` 已覆盖 `agent/session.rs` 的运行时职责；
"会话存哪"归宿主，由 `agentrs-cli` / `agentrs-tui` 经 `RunPersistence` 实现。

**验收**：
- `agentrs-agent` 从仓库消失（在 vendor 中作为参考源）；
- P3.1 清单每一条要么在 notices 里有 ✅ 行、要么在"明确丢弃"里有理由行；
- `cargo test -p agentrs-runtime` 新增：空 final 重试、失败指纹熔断、
  多工具结果合并三组测试；
- `grep -rn "enum PermissionMode"` = 1。

---

### P4 — Provider / Tools / Skills / MCP 去重（3 天）

1. **Provider**：`agentrs-providers`（并入栈）的 anthropic/bedrock/vertex/openai_responses
   线格式已按 notices 第 61–64 行移植进 `agentrs-provider`。剩余：
   - 补齐 `agentrs-providers/src/retry.rs`、`stream_diagnostics.rs`、
     `tool_call_sanitize.rs` 的移植评估（每条给"移植"或"不移植 + 理由"）；
   - 凭据链与 SigV4 **不移植**（§1.1 归 Core），已有结论，写进 notices。
2. **Tools**：`agentrs-tools/src/builtin/` 为准。逐个工具对比上游扁平实现，
   把上游更完整的边界处理（`file_cache`、`view_image`、`tool_search`）按需移植。
   **`Read` 的逐字节一致承诺**必须有测试锁住（`CompactLevel::Full` 绝不能用在 Read 上，
   见 `context/compact/mod.rs` 文档）。
3. **Skills**：`src/pack/` 为准（已修掉上游三处缺陷：`Prefix` 前缀越界、
   替换二次扫描、`HashMap` 顺序不确定——见 notices 第 81/84/85 行）。
   扁平版丢弃；`watcher.rs`（热重载，碰 fs）归宿主。
4. **MCP**：`agentrs-tools/src/mcp/wire.rs` 为准（只留线格式）；
   `agentrs-mcp` 的进程生命周期与凭据归 Core，整 crate 丢弃。
5. **Glob**：`agentrs-types/src/glob.rs` 为唯一实现，删 `glob` crate 依赖
   （两份 glob 语义会分叉——notices 第 85 行已给出理由）。

**验收**：`cargo tree -d`（重复依赖）不含 `glob`、不含两份 `regex`；
`grep -rn 'reqwest' crates/ | grep -v agentrs-provider` 为空。

---

### P5 — 宿主层重建（2 天）

1. `agentrs-cli` 当前引用的 `agentrs_tui::{TuiRuntime, TuiSession, TuiMetadata, TuiOutcome}`
   在内核 TUI 中不存在（§1.3.1）。二选一，建议后者：
   - (a) 在 `agentrs-tui` 上补出这四个符号的门面；
   - (b) **重写 `agentrs-cli/src/run.rs`**，直接用 `RuntimeHost::start` + `agentrs-tui`
     的 `state`/`ui` 驱动。(b) 更少胶水，且让"CLI 只做 RunSpec 翻译"这条边界成立。
2. `agentrs-cli` 是唯一允许 `std::env::var` 的库外 crate（已有豁免，路径需更新）。
3. `agentrs-adapter` 的 `persistence.rs` / `vcr.rs` 与 `agentrs-testkit` 去重。

**验收**：`cargo run -p agentrs-cli -- --help` 与 README 里的 TUI 启动流程可跑通。

---

### P6 — 门禁固化与文档校正（1 天）

1. 新增门禁 `scripts/check-layering.sh`：按 §3.2 的表校验每个 crate 的
   `Cargo.toml` 依赖集是否是允许集的子集。分层不是注释，是可检查的。
2. 三个脚本统一加**自检**：扫描目标为空 → 失败。
3. `THIRD-PARTY-NOTICES.md` 校正：
   - 删除"13 个 crate 原样并入"整节（该做法已在 P0 撤销），改为
     "vendor/ 下为只读参考源，不参与编译"；
   - 移植清单补齐 P2/P3/P4 新增的每一行。
4. `README.md` 的"本地校验"补 `check-layering.sh`；
   Dev TUI 一节的 crate 名从 `agentrs-dev-tui` 改为 `agentrs-tui`。
5. `agentrs-context/src/lib.rs` 的"当前进度"清单按合并后的实际结构重写。

**验收**：四个脚本 + `cargo test --workspace` + `clippy -D warnings` 全绿；
`docs/adr/` 补一条 ADR 记录"为何放弃并入栈"。

---

## 五、排期与依赖

```
P0 止血 ────┬─> P1 底座 ──┬─> P2 上下文/压缩 ─┬─> P3 Runtime ─┬─> P5 宿主 ─> P6 门禁
   0.5d     │     1d      │        3d         │      4d       │     2d        1d
            └─────────────┴─> P4 Provider/Tools ───────────────┘
                                    3d（可与 P2/P3 并行）
```

关键路径 `P0→P1→P2→P3→P5→P6` ≈ **11.5 人日**；P4 并行。
总量约 14 人日（单人）。

**P0 必须先做且不可拆**——在它完成前，任何其他改动都无法被编译验证。

---

## 六、风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| 移植 `agentrs-agent` 实战细节时漏项 | 真实端点上退化（如模型死循环） | P3.1 的清单逐条 ✅/丢弃+理由，review 卡在清单上而不是代码上 |
| 上下文四段合并后行为变化 | 压缩压不下去 → 无限重试 | `makes_progress()` 作为唯一判据 + 一条"压缩无进展必须终止"的测试 |
| vendor 化后有人再从 vendor 里 `use` | 两套架构复活 | `check-layering.sh` 断言 `crates/` 下无 `vendor` 路径依赖 |
| Apache-2.0 §4b 合规 | 法务 | 门禁修复（P0.5）是这一条的前提；移植清单逐条维护 |
| `agentrs-cli` 重写引入回归 | CLI 不可用 | P5 选 (b) 前先补一组 CLI 端到端测试（用 `agentrs-testkit` 的 fake provider） |

---

## 七、立即可做的三件事（不需要等排期）

1. **修两个门禁脚本的豁免规则**（10 分钟）——当前它们恒真，
   在此期间任何越界都不会被发现。
2. **恢复四个被误删的 crate**（5 分钟）——`git checkout HEAD -- …`，
   让 `cargo metadata` 能跑。
3. **在 `Cargo.toml` 里把 `members` 从 `crates/*` 改为显式列表**（10 分钟）——
   通配符 members 是"删了一个 crate 但引用还在"能静默存在的原因。

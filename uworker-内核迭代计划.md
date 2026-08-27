# uworker 内核迭代计划：AgentRS + SandboxRS

> 日期：2026-08-24 ｜ 范围：**AgentRS 与 SandboxRS 两个内核**，不含 AgentCore / AgentUI
>
> 依据：[架构方案 v1.6](agentrs-技术架构设计与实施方案.md)、[任务分解 v1.6](agentrs-任务级实施分解与源码借鉴清单.md)、[规模估算与排期校准报告](uworker-规模估算与排期校准报告.md)
>
> 本计划**取代**任务分解文档 §4 的 12 周排期表。那份排期与实测规模差 4.7 倍，无论怎么调配人力都不成立。

---

## 1. 结论先行

| 项 | AgentRS | SandboxRS | 合计 |
|---|---:|---:|---:|
| 净实现 | 45,500 | 15,500 | 61,000 |
| 含测试 | 118,300 | 40,300 | 158,600 |
| 人周（1,175 行/人周） | 101 | 34 | **135** |

| 编组 | 到 M2（可用内核） | 到 M3 |
|---|---|---|
| **5 人（3 RS + 2 SBX）** | **30 周** | **34 周** |
| 3 人（混合） | 45 周 | 51 周 |

**推荐 5 人编组。**不只是因为快——SandboxRS 在 AgentRS 的关键路径上（W9 与 W11 两个门），混合编组会让两条线互相抢人，而 Sandbox 的隔离代码与 AgentRS 的状态机代码需要的技能栈也不同。

**首个可演示节点**：W2（真实 provider 跑通文本 Final）
**首个可用节点**：W10（M0：可恢复闭环 + 真实工具端到端任务）

---

## 2. 人力编组

M0 的关键路径此前全压在一个人身上（"Runtime 开发负责 T01-T04/T03A/T03B/T10"），所谓并行并不存在。重新编组为五条真正可并行的线：

| 角色 | 负责范围 | Phase A/S1 产出 |
|---|---|---|
| **R1 · 内核状态机** | contracts、事件/checkpoint、composition/owner、RuntimeHost、Engine、Surface、恢复 | 8,100 |
| **R2 · 端口与验证** | 全部 port trait、testkit、确定性时钟、崩溃注入、conformance suite、dev-adapter、CLI | 4,000 |
| **R3 · Provider 与上下文** | A 类移植、真实 provider 接通、ProviderCompat、legalization、缓存、context/manifest | 1,500 |
| **S1 · 执行与协议** | 执行协议、IPC、grant 校验、reconcile、工具执行体 | 3,800 |
| **S2 · 隔离与 overlay** | OS 隔离、ChangeSet overlay、undo、回收区、PTY | 3,200 |

耦合点只有两个，必须在第 1 周解决：

1. **R1 定义 contracts、R2 依赖它做 fake** → contracts 首版**必须在 W1 结束前冻结**，此后只增量不重构。
2. **S1/S2 依赖 `SandboxExecutor` port 与 `SandboxGrant` 载荷** → 这两个契约也在 W1 冻结（Q1 的确认因此从 W8 提前到 **W1**）。

R3 在 Phase A 只有 1,500 行是有意的——他的任务是**第 2 周就接通真实 provider**，价值不在行数而在尽早证伪。

---

## 3. 阶段总览

```text
        W1    W5    W10   W15   W20   W25   W30   W34
AgentRS ├─── Phase A ───┼── Phase B ──┼─ Phase C ─┼─ D ─┤
        │      M0(W10)  │     M1(W21) │   M2(W30) │M3   │
Sandbox  ├─── S1 ───────┼─── S2 ──────┼─── S3 ────┤
        │  最小执行器   │  真实隔离   │ PTY/强化  │
        │      ↑        │      ↑      │
        │   喂给 M0     │  喂给 M1    │
```

| 阶段 | 周次 | 实现 | 含测试 | 人周 | 里程碑 |
|---|---|---:|---:|---:|---|
| **AgentRS Phase A** | W1–W10 | 13,600 | 35,400 | 30 | **M0** 可恢复闭环 + 真实任务 |
| **SandboxRS S1** | W2–W10 | 7,000 | 18,200 | 15 | 最小执行器 + 基础围栏 |
| **AgentRS Phase B** | W11–W21 | 15,000 | 39,000 | 33 | **M1** 真实工具链路 + 缓存达标 |
| **SandboxRS S2** | W11–W20 | 5,500 | 14,300 | 12 | 真实隔离 + overlay 完善 |
| **AgentRS Phase C** | W22–W30 | 11,400 | 29,600 | 25 | **M2** 上下文可重建 + 子 Agent |
| **SandboxRS S3** | W21–W30 | 3,000 | 7,800 | 7 | PTY/终端 + 跨平台强化 |
| **AgentRS Phase D** | W31–W34 | 5,500 | 14,300 | 12 | **M3** 受控扩展（条件触发） |
| **合计** | 34 周 | 61,000 | 158,600 | 134 | |

---

## 4. 一个必须先定的设计决策：分级隔离

排期上有个硬冲突：**AgentRS 在 W11 需要 `ExecCommand` 可用，但真正的 OS 隔离（landlock/seccomp/JobObject）是 3,500 行平台相关代码，做不完。**

如果为了赶进度让 ExecCommand 裸跑，那就退回了 aionrs 的做法（`aion-process/containment.rs` 只做 kill 传播，不是隔离）——这正是本架构要否定的。

因此引入**分级隔离**，并让 conformance suite 的 H2 按级评定：

| 级别 | 内容 | 交付 | H2 评定 |
|---|---|---|---|
| **L0 基础围栏** | 进程组、rlimit（CPU/内存/文件数）、cwd 限制、env 清洗、默认断网、超时强杀 | **S1（W10）** | `H2-L0` 通过 |
| **L1 真实隔离** | Linux landlock + seccomp；macOS sandbox_init；Windows Job Object + 受限令牌 | **S2（W20）** | `H2-L1` 通过 |
| **L2 远程沙箱** | 容器/微 VM 后端 | 不在本计划 | — |

规则：

1. **L0 是真实的约束，不是占位。** 它能挡住"写到工作区之外""跑满 CPU""偷偷联网"这三类最常见的越界，只是挡不住有意的提权攻击。
2. **`ExecutionResult` 必须携带实际生效的隔离级别**，AgentRS 据此在 trajectory 中标注，用户能看到"这条命令是在 L0 下执行的"。
3. **L0 期间禁止对外宣称"已隔离"**，产品文案与安全承诺以 L1 为准。
4. M1 的验收在 L0 下可以通过，但 **L1 未达成前不进入任何对外发布**。

这条决策的价值在于：它让 W11 的门可以诚实地过，而不是靠降低标准或自欺。

---

## 5. AgentRS Phase A（W1–W10）与 SandboxRS S1（W2–W10）

### 5.1 周计划

| 周 | R1 状态机 | R2 端口与验证 | R3 Provider/上下文 | S1 执行与协议 | S2 隔离与 overlay |
|---|---|---|---|---|---|
| **W1** | **contracts 首版冻结**：ID、事件 envelope、RunEpoch、SpecVersion、ContentRef、PolicyDecision、PermissionMode | 跟随定义 10 个 port trait 签名 | workspace、CI、移植合规脚手架 | **与 R2 共同冻结 `SandboxExecutor` + `SandboxGrant` 载荷（Q1）** | overlay 技术选型 spike：CoW 目录 vs 内存差分 vs overlayfs |
| **W2** | 事件/checkpoint 协议、Surface 类型与 `derive_messages` | fake Persistence（幂等+epoch 围栏）、确定性时钟 | **A 类移植首批 + 真实 provider 跑通文本 Final** | 执行协议 + IPC 骨架 | overlay 最小实现（读己之写） |
| **W3** | composition：scope/owner/AsyncCleanup/setup 事务 | fake ContentStore、fake Policy | ProviderCompat 校准，观测非零 cache 命中 | grant 校验 + `input_hash` 复核 | overlay 快照与差分读取 |
| **W4** | RuntimeHost start/resume/cancel、Turn/Step FSM | fake Sandbox（三态 reconcile）、崩溃注入器 | **费率校准点（§8）** | reconcile 三态 | **overlay 冒烟：Edit 后 Read 读到新内容** |
| **W5** | inbox/claim + steering、0-Step Turn | interleaving scheduler、cleanup 失败注入 | context 骨架：Surface → 预算裁剪 | 执行体 Read/Write/Edit | L0 围栏：rlimit + cwd 限制 |
| **W6** | 审批有界等待 → 超时挂起 → checkpoint → resume | conformance H3/H4 首版 | manifest 首版 + 保守 token 估算器 | 执行体 Glob/Grep | L0 围栏：env 清洗 + 断网 |
| **W7** | RecoveryPlanner：五个 durable 边界 | 五边界崩溃恢复测试全绿 | 缓存分段 + `cache_prefix_digest` | 执行体 ExecCommand | L0 围栏：超时强杀 + 进程树回收 |
| **W8** | quiescent shutdown、cancel 收敛 | `agentrs-cli serve/doctor` | HistoryLegalization 首版 | 取消与 kill 传播 | **conformance H2-L0 通过** |
| **W9** | 运行时不变式（model-visible-is-logged） | dev-adapter 首版 | 单工具 ToolRound 打通 | **与 AgentRS 联调：真实 Sandbox 跑通 Read/Write** | undo 首版 |
| **W10** | **M0 收口与验收** | 崩溃/取消/挂起全场景回归 | 真实任务闭环演练 | **S1 收口**：H1/H7 通过 | ChangeSet 提交/丢弃路径 |

### 5.2 M0 出口标准（W10）

> **M0 实测结果（2026-08-25）**：八条标准中**七条达成**，252 个测试全绿，
> 真实 Qwen3.8-27B + 真实文件工具端到端跑通（1 Turn / 3 Step / Completed，
> 产出内容与源文件完全对应）。未达成的是 **H7 在真实 SandboxRS 上的验证**——
> dev-adapter 的 overlay 已通过，但真实实现尚不存在，overlay spike 仍是风险登记第一位。
>
> 过程中暴露六个契约缺口与五处设计修正，已回填架构方案 §18。**其中四个缺陷在
> 235 个单元测试全绿时仍然存活，只有端到端跑起来才发现**——这条印证了下面第 6 条
> 标准的必要性：它不是演示，是唯一能发现装配级缺陷的手段。

必须**全部**满足，缺一不算完成：

- [ ] 五个 durable 边界各注入一次崩溃，恢复后**不产生第二次未知工具调用**
- [ ] 审批超时挂起后进程内该 Run 常驻内存 ≈ 0，`resume` 能继续同一 `StepIntent`
- [ ] 取消后无残留 stream / task / listener / registration
- [ ] 同一 log 前缀始终派生同一组消息；live delta 全丢不影响 transcript 重建
- [ ] 运行时不变式能捕获人为构造的"绕过 log 直接进请求"路径
- [ ] **能干成一件事**：真实 provider + **真实 SandboxRS**（L0）完成"读三个文件、总结、写一份 markdown"，走完审批与 ChangeSet 流程
- [ ] conformance suite 的 **H1（hash 复核）、H2-L0、H3、H4、H7（overlay 一致）** 通过
- [ ] `agentrs` 单测不需要网络、文件系统、真实时钟或 OS 进程（边界判据，CI 强制）

第 6 条相比原计划有实质提升：**M0 的真实任务不再靠 dev-adapter 顶，而是走真实 SandboxRS**。这是把 SandboxRS 一起排期换来的最大收益——H1/H2/H7 这三条宿主义务在 W10 就被真实实现验证，而不是推迟到 Phase B 甚至更晚。

`agentrs-dev-adapter` 仍然要做，但定位回归本意：**开发期的便利工具与 conformance suite 的第二个被测对象**，不再承担"顶替 Sandbox"的职责。

---

## 6. Phase B / S2（W11–W21）

**AgentRS Phase B 开工前置**：S1 已收口，真实 SandboxRS 能跑 Read + ExecCommand 且 reconcile 返回真实三态。W10 的 S1 收口即满足此条——**这是联合排期消除的最大一个阻塞风险**。

### AgentRS Phase B（15,000 行）

| 内容 | 实现 | 负责 |
|---|---:|---|
| A 类移植剩余（4 厂商 + projector + sanitize + cache_diagnostics） | 3,500 | R3 |
| HistoryLegalization 完整化（跨 provider fixture、golden 序列） | 800 | R3 |
| 缓存断点放置、`CacheBreakCause` 归因、命中率回归 | 700 | R3 |
| ToolLoop 固定管线：guard、Hook、grant 一次性消费、并发调度 | 3,000 | R1 |
| ChangeSet overlay 读己之写、文件工作集版本绑定 | 800 | R1 |
| `ExternalFact` 契约与 inbox→Surface 通路 | 800 | R1 |
| Fork | 700 | R1 |
| Projection Registry + 基础轨迹 | 1,500 | R2 |
| dev-adapter 完整化 + conformance 全量 | 1,700 | R2 |
| CLI：run/resume/validate/replay/trajectory/conformance/cache-report | 1,000 | R2 |
| PermissionMode / Plan Mode | 500 | R1 |

### Phase B 不依赖 SandboxRS 的部分：已提前完成（2026-08-25）

上表中有 5 项不需要真实 Sandbox 即可交付，已在 S1 收口前先行完成，
目的是把 Phase B 开工时的剩余工作压到确实需要 Sandbox 的那几项上。

| 内容 | 交付 | 测试 |
|---|---|---|
| 缓存断点放置与归因 | `cache.rs` 分段与 `attribute()`、`cache_diagnostics.rs` 响应侧诊断、**引擎接入** | 47 |
| `ExternalFact` 契约与 inbox→Surface 通路 | `agentrs-contracts/src/external.rs` + inbox 幂等 | 8 |
| Fork | `agentrs-runtime/src/fork.rs`，六条规则逐条覆盖 | 11 |
| Projection Registry + 基础轨迹 | `agentrs-observability/src/projection{,/defs}.rs`，首批 5 个投影 | 26 |
| A 类移植剩余厂商 | `anthropic.rs`、`anthropic_wire.rs`、`openai_responses.rs`、`cache_diagnostics.rs` | 39 |
| ToolLoop 固定管线：guard、Hook、grant 一次性消费 | `toolround.rs` 两个收紧点 + grant 绑定复核 | 14 |
| 工具并发调度与 ordering barrier | `schedule.rs` + `toolround::execute_batch` | 18 |
| PermissionMode / Plan Mode | `permission.rs` 五条规则 + 引擎接入 | 30 |
| ChangeSet overlay 读己之写 | dev-adapter 补 Edit/Grep/Delete + 墓碑 | 21 |
| conformance suite（H1–H5、H7）+ `agentrs conformance` | `testkit/conformance/`，对任意 adapter 通用 | 45 |
| CLI 子命令 validate/trajectory/cache-report/replay/resume | `cli/inspect.rs`，纯读事件日志 | — |
| HistoryLegalization 完整化 | 补孤儿结果/空消息/无签名思考 + 跨 provider golden | 14 |
| 缓存前缀端到端接入 | 引擎逐请求算快照、归因、发 `CacheBreakObserved` | 8 |

工作区合计 **510 个测试**，clippy 0 warning，边界门禁与移植合规门禁均通过。

两处**移植范围收缩**，理由都是边界判据（§1.1），已记入 `THIRD-PARTY-NOTICES.md`：

- **Bedrock / Vertex 只移植线格式，不移植凭据解析。** 原实现的主体是 AWS SigV4
  凭据链（读 `AWS_*` 环境变量、读 `~/.aws/credentials`、访问 IMDS）与 GCP ADC
  （读 key 文件、访问 metadata server）。这些读环境变量、读磁盘、解析凭据，
  按 §1.1 归 Core；内核侧只保留 URL 构造、`anthropic_version` 与 body 适配，
  签名时刻由 Clock port 传入。
- **各厂商投影器里的 sanitize 逻辑不移植。** 参考实现让每个厂商适配器各做一份
  orphan tool_result 清理与 malformed tool_call 降级；本方案把它集中在
  `legalization.rs` 的固定阶段（§8.1），否则"事实与实际请求之间做了哪些修复"
  无法统一留痕。

**并发调度这一项做完后发现了一个真实缺陷**：guard、Hook、Policy、grant 指纹
四条拒绝路径全都只 `return` 了 `StepResult` 而没写 `StepResultRecorded`，
投影会把它们全部误判成"有意图无结果"、恢复时逐个去 reconcile 一件没发生的事。
是 `ToolPaths` 投影的 dangling 判定把它照出来的。已改为单一出口提交，
详见架构文档 §18.4。

Plan Mode 做成了**两道独立防线**：目录投影（模型看不见写类工具）与
`ModeGuard`（看见了也调不动）。两道都做了反向验证——去掉任意一道都有测试挂。
只做前者的话，历史里残留的旧 `tool_use` 可以直接穿过去。
实现过程中补了一个契约缺口（`ToolDef` 没有 `EffectProfile`），见架构文档 §18.5。

**读己之写已按 M1 出口标准的原文验收**（"Edit 后 Read / Grep 在同一 ChangeSet
overlay 上读到新内容"），并用本地 Qwen3.8-27B 做了端到端确认：
模型 Grep 看到 `30` → Edit → 再 Grep 看到 `60`，而磁盘上仍是 `30`（未提交）。
三条 overlay 路径（Edit 的中间读、墓碑、Grep 的可见文件集）各自做了反向验证。

实现中补了一处 overlay 语义缺口：原实现只记写入、未命中即回落磁盘，
于是"删掉再读"会命中盘上的旧内容——删除只是看起来生效了。已补墓碑。

**conformance suite 已按 M1 出口标准的原文验收**（"一个故意写坏的 adapter 能被
conformance suite 逐条捕获"）：26 个只违反一条义务的适配器，每个都被**恰好那一条**
检查捕获，其余检查照常通过——一处坏了满盘皆红的话，报告就没有定位价值。
合格实现的基线也在测（否则"能捕获"可能只是因为套件恒报失败）。

套件对任意 adapter 通用：实现方只提供"合法起点"（登记 grant、构造一份自己认识的
请求），违规场景由套件构造。已覆盖 **H1/H2/H7（Sandbox）、H3（Persistence）、
H4（ContentStore）、H5（PolicyEnforcer）**，共 26 项检查。

**H6 有意不做独立套件**：`HookOutcome` 只有 `Proceed`/`Advise`/`Block`，
第三方实现连"放宽授权"都表达不出来。由类型保证的东西再写一遍运行时检查，
只会给人一种"验过了"的错觉。真正该验的是内核有没有把 `Block` 当回事，
那属于内核不变量，已由 runtime 的 tightening_tests 覆盖。

H3 的四条尤其需要这层验证：**重复事实、序号乱序、双 writer、超前 checkpoint
都不会当场报错**，只会让日后的恢复读出一份自相矛盾的历史。套件是唯一能在
事发前发现它们的东西。

**套件在我们自己的 dev-adapter 上查出了三个真缺陷**，见架构文档 §18.7。
这是这项投入最直接的回报：写套件的人和写 adapter 的人是同一个，
仍然被查了出来——说明它验的不是"作者记得的东西"。

**Phase B 中不依赖 SandboxRS 的部分至此全部完成**（表内 11 项全部交付）。

**M1 的「缓存 · 功能验证」一条已可验收。** 此前分段与归因的机制建好了，
但引擎从不调用它——`cache-report` 恒报 0 次断裂。现在引擎每次请求都算出
稳定段摘要、与上一次比对并归因，miss 时写 `CacheBreakObserved`。
真实 Run 上验过：两次请求、一次断裂（首次请求无可比对象），
**跨越一整个工具回合前缀保持稳定**。

接入过程中改正了一处概念错误，见架构文档 §18.10。

HistoryLegalization 补齐了三类此前不处理的阻抗：**孤儿 `tool_result`**
（压缩截断或中途分叉造成，Anthropic 族对"引用了不存在的 tool_use"直接 400）、
**被掏空的空消息**（块级过滤的副产物，多数端点拒收）、
**无签名 thinking**（此前由厂商投影器静默丢弃，投影期丢弃不留痕，
于是"为什么这次请求少了一段思考"无从解释）。

跨三种 provider 的 golden 序列把整条 op 顺序钉死——逐条断言"有没有发生某次修复"
抓不住顺序错误。另加**幂等**检验：重试、fallback、压缩后重装配都会让同一段历史
被合法化不止一次，若不幂等，每过一遍就多一层合成结果，历史会越修越长、越修越假。

五条只读子命令
（`validate`/`trajectory`/`cache-report`/`replay`/`resume`）在真实 Run 上验过，
并在第一次运行时就查出两个缺陷，见架构文档 §18.8。

剩余 Phase B 项均需真实 SandboxRS：L1 隔离下的 conformance、
`resume` 的续跑部分（要向真实 Sandbox reconcile 并重建 live 资源）。
其中并发调度的**调度与屏障语义已完成并有并发性证明**（闸门式测试：
不并发就跑不完），接入真实 Sandbox 后语义不变、只是变快。

### SandboxRS S2（5,500 行）

| 内容 | 实现 |
|---|---:|
| Linux landlock + seccomp | 1,600 |
| macOS sandbox_init | 900 |
| Windows Job Object + 受限令牌 | 1,000 |
| overlay 完善：undo、回收区、大文件与二进制处理 | 1,500 |
| H1/H2-L1/H7 conformance 通过 | 500 |

### M1 出口标准（W21）

> **缓存标准拆分说明（2026-08-25 实测后修订）**：原标准是单条"真实 provider 连续 10 轮 ≥70%"。
> W2 冒烟发现本地 vLLM（0.19.0，Qwen3.8-27B）端点 `prefix_cache_queries_total` 恒为 `0`，
> `prompt_tokens_details` 为 `null`，50K token 前缀的 TTFT 冷热一致（7448ms vs 7352ms）——
> 该端点既不报告缓存字段，实测也无缓存收益。
>
> 因此把"我们的代码对不对"与"这个端点快不快"分开：前者我们能控制且必须达标，
> 后者依赖端点能力。设计本身不变——`§9.1.1` 的缓存前缀不变式服务的是
> Anthropic / OpenAI / DeepSeek 这类可靠报告缓存字段的生产端点。

- [ ] AgentRS 代码中不含任何 OS 命令执行路径
- [ ] 安全五阶段不可被 middleware 绕过（断言覆盖）
- [ ] 崩溃后不重复未知工具副作用（真实 Sandbox 下复测）
- [ ] **缓存 · 功能验证**（不依赖端点）：`cache_prefix_digest` 计算正确、`CacheBreakCause` 归因链路完整、
      Surface `Replace` 后的失效点计算准确、两种缓存字段布局均能解析。纯逻辑，用 fixture 覆盖
- [ ] **缓存 · 收益验证**（需支持端点）：在一个**报告缓存字段**的端点上，连续 10 轮对话 input token 命中率 ≥ 70%
- [ ] Edit 后 Read / Grep / 构建在同一 ChangeSet overlay 上读到新内容
- [ ] 工具往返的内核侧开销 P95 < 30 ms
- [ ] **conformance H2-L1 在三个平台各自通过**
- [ ] 团队能用两个内核完成自身的日常修改（dogfooding）
- [ ] 一个故意写坏的 adapter 能被 conformance suite 逐条捕获

---

## 7. Phase C / S3（W22–W30）与 Phase D（W31–W34）

### AgentRS Phase C（11,400 行）

| 内容 | 实现 |
|---|---:|
| context 剩余：token 账本、四段压缩、Surface Replace 表达、多模态配额 | 2,800 |
| skills（B 类移植 + 单调收窄 ContextModifier + 缓存落位） | 1,600 |
| memory port + memorySelector | 600 |
| 函数式子 Agent（Explore/Plan/compact/contextSummary）+ ChildRun | 2,200 |
| prompts 版本化 + 输出 schema + eval 集 | 1,200 |
| observability：Trajectory 查询、replay bundle、脱敏导出、OTel | 3,000 |

### Phase C 已开工部分（2026-08-26）

| 内容 | 交付 | 测试 |
|---|---|---|
| 四段压缩的规划层 | `agentrs-context/src/compaction.rs`，纯函数 | 26 |
| 压缩落到 Surface | `Engine::apply_compaction` 追加 `Replace` 节点 | 8 |
| 函数式子 Agent 骨架 + `compact` | `agentrs-subagents`，零工具、独立上下文 | 16 + 2 live |
| skills：ContextModifier 单调收窄 + 缓存落位 | `agentrs-skills` | 24 |
| memory port + memorySelector | `agentrs-memory`，只在候选里挑 | 20 + 3 live |
| 脱敏导出 + replay bundle + `agentrs export` | `observability/redact.rs` | 25 |
| Trajectory 分页性能（M2 出口标准） | `page()` 改二分定位 | 7 |
| ChildRun 派生（内核不变量 5） | `subagents/childrun.rs` + 契约补 `ChildRunSpec`/`SubagentSummary` | 18 |
| prompts 版本化 + 输入 schema + 安全 lint | `agentrs-prompts`，散在各处的提示词集中 | 21 |

此前 `SurfaceOp::Replace` 的处境很特殊：**契约里定义了、缓存失效点认它、
投影会遮蔽它，但没有任何代码会产生一个**。压缩是唯一的生产者，
所以它一天不做，`Replace` 这条路径就一天没被真正走通。

规划层刻意做成纯函数——摘要要调模型（零工具子 Agent，属 Phase C 后续），
但 §9.3 里真正难的那部分是**该压哪一段、压了到底有没有用**，
这部分可以脱离模型穷尽测试。已覆盖的判据：

- 钉住的段、承载错误的条目、未消费的工具结果、最近 N 条 —— 一律不动；
- 优先 Microcompact（代价最小），剪不动才降级到摘要；
- 取**最长的一段连续**可动条目：`Replace` 遮蔽的是区间，挑不连续的条目
  需要多个 Replace，每个都是一次缓存失效点——压一次断好几处前缀；
- 不重复压同一段（信息漂移）；
- **回收为 0 不算推进** —— 这是溢出触发的重试判据，
  允许它推进代际就等于允许无限重试。

Surface 层验的是那三条推论：模型看到的历史被遮蔽、
**人类 transcript 不受影响**、缓存失效点等于 `range.start`。
第二条是 `Replace` 这个设计的全部意义——若压缩是"改写历史"，
用户滚回去会发现自己说过的话不见了。

**压缩闭环已通。** `compact` 子 Agent 按 §11.1 做成函数式：零工具、
独立受限上下文、只返回结论。三条约束都是**结构保证**而非约定——
`build_request` 恒把 `tools` 置空，`collect` 丢弃思考增量，
返回类型里没有任何通道能把中间态带出来。

失败一律 fail-closed：空结论、缺必填项都返回 `Err`，**绝不返回"尽力而为"的
部分摘要**。理由是压缩的动作是"用摘要**遮蔽**历史"，两步绑在一起——
摘要没生成出来还照样遮蔽，等于把一段历史换成空白，模型会突然失忆
而且没人知道为什么。缺项也不自动补"未决审批：无"，那是替模型撒谎。

真实模型验过（Qwen3.8-27B）：十项必填全覆盖，保住了文件名、
ChangeSet id、错误原文与未决审批。**「未决审批」单独立了一条测试**——
它最容易被摘要吃掉，后果又最严重：漏了它，恢复后模型会以为那个操作
已经批过了，直接接着往下做。

**技能是这套设计里"只能收紧"原则的第三次出现**（前两次是单调 guard 与
Hook 的 `Proceed/Advise/Block`）。合并算子刻意不是"右侧覆盖"而是取下确界：
集合交、预算最小、模式更严、深度最小。普通的配置合并语义在这里**不安全**——
一个写错的技能清单能靠覆盖把 `tool_subset` 写成更大的集合，技能就成了提权路径。

取下确界还顺带给了一条性质：**启用顺序不影响最终能力**。
若顺序有影响，"先启 A 再启 B"和"先启 B 再启 A"会得到两个不同的 Run。

越界的要求**留痕而不是静默丢弃**——它多半是清单写错了，
静默忽略会让作者一直以为自己的配置生效了。

缓存落位与压缩接上了：中途修改 S0 会造成一次全量 miss，因此**推迟到压缩边界**——
压缩本来就要作废前缀（`Replace` 使 `range.start` 起失效），既然这一刀免不了，
就让所有要作废前缀的改动搭同一班车。实现时发现 §10 与契约注释对落位的
说法不一致，见架构文档 §18.11。

**memorySelector 的失败策略与 `compact` 刻意相反**，这一对比值得单独记：

| | 失败后果 | 策略 |
|---|---|---|
| `compact` | 摘要没了还照样遮蔽 → **历史被换成空白** | fail-closed，不压 |
| `memorySelector` | 没挑出来 → 少几条记忆 | 软降级，用 Core 的确定性排名 |

判据是"失败会不会破坏已有的东西"。压缩会，记忆不会——它只是没帮上忙。
为一个可选的增强而让 Run 停下来不划算。**降级必须留痕**，
否则"记忆怎么突然变差了"查不出来。

selector 编造候选之外的 id 时**整体退回排名**，而不是"把越界的挑出来扔掉"——
一旦它编造了一个 id，它对其余几条的判断也不再可信。

真实模型验过（Qwen3.8-27B）：与全部候选都无关的任务（改时区）产出
`{"selected": []}`，一条不选；相关任务只挑出了缩进偏好与字段名，
避开了看猫、冰岛、香菜。**这一类 `validate` 拦不住**——那些 id 确实在候选里，
只是与任务无关，唯一的防线是提示词，所以必须真跑一次看提示词够不够。

**脱敏那条约束不显然，值得单独记**：durable 事实里必然包含路径
（`StepIntent` 与 reconcile 都靠它定位），而导出要求"不含绝对路径"。
最直觉的做法是删掉或随机替换，**但两者都会破坏投影的确定性**——
删掉了，两条不同路径的事件撞成一条；随机替换了，同一份 bundle 导出两次
得到两个 snapshot。而 replay 正是导出这件事的目的。

所以脱敏是**稳定映射**：能相对化就相对化（可读、可排查、同样确定），
其余用 `opaque:` + 加盐摘要。盐记在 bundle 头部，
**同一 bundle 内可逆性为零、一致性为一**。

拒绝码保留而说明脱敏——码是稳定枚举、不含用户内容，
而它恰恰是排查时最有用的那一半。

`audit()` 是兜底：脱敏规则会漏，新增一个带路径的字段就漏一处，
而漏了没人会发现。已验证它能抓住脱敏器不覆盖的字段并拒绝导出（退出码 1）。

**M2 出口标准的"导出默认无敏感正文与绝对路径"至此可验收。**

**M2 出口标准的"百万事件下分页查询 P95 < 100 ms"至此可验收**，
一百万条实测 P95 远低于阈值。

这一条是**先把标准写成测试、跑出失败、再修**的：初版 `page()`
每次翻页都把全量 durable 事件收集并重排一遍。绝对值其实是达标的
（20 万条 4.62 ms），但扩展性不达标——**事件多十倍，翻页慢 12.5 倍**。
按这个斜率，一千万条就会撞上阈值。

所以除了绝对阈值，还立了一条**不随机器变化**的断言：
"日志长十倍，翻页耗时增长不超过四倍"。100 ms 的阈值换台机器就变，
但"翻一页的代价与日志总长无关"是实现性质。改成二分定位后比值降到 1×。

代价是给 `page()` 加了一条前置条件：**输入必须按 `seq` 升序**。
这不是"最好如此"而是存储侧契约（H3 的 seq 单调 + 按写入顺序读回），
`validate` 会检查，debug 下还有断言兜底——乱序会给出**错误结果**
而不是慢结果，那比性能问题严重得多。

ChildRun 是**"只能收紧"原则的第四次出现**（前三次：单调 guard、Hook、
技能的 `ContextModifier`）。落点各不相同，判据同一条：
**任何派生出来的东西都不该比它的来源更宽**。

两处判断值得记：

1. **上界取父的 `CapabilityView` 而不是 `AuthorityEnvelope`。**
   子能拿到的是父**此刻实际可用的**，不是父被签发时的上界——
   父自己已经收窄过的东西，子不该拿回去。
2. **深度超限拒绝而不截断。** 静默截到上限继续跑，会让一个写错的
   递归编排看起来在正常工作，而它实际上少做了最里面那几层。

`ChildRunSpec` 与 `MemberRunSpec` 在契约里彻底分开，四个维度都不同。
其中"共享父 epoch"**只对函数式子 Agent 成立**：它活不过一个 operation，
父被围栏时跟着倒是对的；协作式成员跨父的多个 turn，父 Run 结束了
它还可能在跑，让它跟着父的 epoch 倒就错了。

**Phase C 的表内六项至此全部交付。**

提示词此前散在三个 crate 里（压缩在 subagents、记忆选择在 memory、
Plan 模式在 runtime）。散着的两个问题：没人能一眼看全"我们一共对模型说了些什么"；
安全 lint 无处施加——它得能遍历全部提示词才有意义。集中后
`lint::audit_all` 成了守门人。

集中过程中真实模型逼出了一个**行为缺陷**，见架构文档 §18.13。

### SandboxRS S3（3,000 行）

| 内容 | 实现 |
|---|---:|
| PTY / 持久终端 | 2,000 |
| 跨平台矩阵、性能剖析、故障注入 | 1,000 |

**M2 出口**：主 Agent 不接收子 Agent 过程推理；任意请求可由 durable facts + content refs 重建；百万事件下分页查询 P95 < 100 ms；导出默认无敏感正文与绝对路径；PTY 在三平台可用。

### AgentRS Phase D（5,500 行，条件触发）

| 内容 | 实现 | 启动条件 |
|---|---:|---|
| generation + ComponentManifest/Inventory + profile 事务 | 2,500 | ≥2 provider / MCP / per-run component 的真实换代需求出现 |
| `BoardSnapshotRef`、团队审批与预算、`MemberRunSpec` | 1,500 | Core 编排面就绪、Q9 已确认 |
| MCP proxy | 800 | — |
| 性能、压力、故障注入、发布兼容治理 | 700 | — |

前两项**条件触发，不满足就不启动**——这是"不为尚未采用的能力预付复杂度"的排期表达。

---

## 8. 校准机制：第 4 周必须做

本计划全部数字建立在**他人项目的费率**（aionrs 1,175 行/人周）之上，不是本团队的实测。W4 强制校准：

```
本团队费率 = (实现行数 + 测试行数) ÷ (5 人 × 4 周)
本项目测试比 = 测试行数 ÷ 实现行数
```

**两条线分别统计**——AgentRS 是状态机与不变式密集的代码，SandboxRS 是系统编程，费率大概率不同，不要混算。

| 实测费率 | 应对 |
|---|---|
| > 1,400 行/人周 | 按实际压缩排期，但不要提前对外承诺 |
| 900–1,400 | 维持本计划 |
| < 900 | **立即启动范围削减（§9），不要靠加班追赶** |

同时校准测试比：方案要求 property test、interleaving scheduler、崩溃注入、conformance suite、golden fixtures，实测很可能到 1.8–2.0 而非 1.6。**每 +0.2 增加约 12,000 行（两个内核合计），即 +10 人周。**

---

## 9. 范围削减预案（按启用顺序）

| 级 | 削减 | 省 | 代价 |
|---|---|---:|---|
| **1** | AgentRS Phase D 全部推迟 | 12 人周 | 无 MCP、无 generation、无协作式团队。**契约保留**（`ExternalFact` 已在 Phase B） |
| **2** | SandboxRS S3 的 PTY 推迟 | 5 人周 | 无持久终端，交互式命令受限 |
| **3** | 隔离只做 Linux（L1），macOS/Windows 停留在 L0 | 4 人周 | 仅 Linux 可对外发布 |
| **4** | observability 降级为"事件落盘 + CLI 查看" | 8 人周 | 失去可解释性与离线重放。**事件语义必须保留**，否则事后补要改事实流 |
| **5** | 函数式子 Agent 只留 compact 与 memorySelector | 5 人周 | Agent 能力明显变弱，内核结构不变 |
| **6** | skills 整体推迟 | 5 人周 | 无技能系统 |

**不可削减项**（削了等于事后改地基，成本是留着的 5–10 倍）：contracts、事件账本与 epoch 围栏、ModelSurface、owner/生命周期、StepIntent/恢复、ContentStore 契约、`ExternalFact` 契约、缓存前缀不变式、conformance suite、**overlay 读己之写**、**grant 一次性消费与 hash 复核**。

后两项是这次新增的——它们是 SandboxRS 侧的地基，事后补要同时改 AgentRS 的 `input_hash` 与文件工作集结构。

---

## 10. 风险登记

| # | 风险 | 概率 | 影响 | 应对 |
|---|---|---|---|---|
| 1 | **overlay 技术选型失败**（CoW 目录性能不够、overlayfs 需 root、内存差分撑不住大仓库） | **高** | 直接影响读己之写，波及 M0 出口 | **W1 就做 spike**（已排入周计划），三个方案各做一个最小验证，W2 前定案 |
| 2 | 隔离在 macOS/Windows 上受限 | 高 | L1 无法在全平台达成 | 分级隔离已把这件事显式化；必要时走削减级 3 |
| 3 | 测试比高于 1.6 | 中高 | +10 人周/每 0.2 | W4 校准即可发现 |
| 4 | contracts 或 `SandboxGrant` 载荷首版设计不当 | 中 | 波及五条线 | W1 冻结前做一次五人交叉评审；此后只增量不重构 |
| 5 | A 类移植的接缝改造量被低估 | 中 | R3 进度滑坡 | W2 先移植最小集并接真实流验证，暴露改造成本 |
| 6 | 架构本身有结构性问题 | 低但影响巨大 | 估算全部作废 | M0 的真实任务闭环是最早证伪点，W10 见分晓 |
| 7 | **5 人被抽调去做 Core/UI** | 中 | 工期直接翻倍 | 本计划以"5 人专职两个内核"为前提；若不成立必须重新排期而非默默拖延 |

风险 #1 从原计划的第二梯队升到首位——**overlay 是全项目估算不确定度最高的单项（±60%），且三个参考系统里没有一个能直接借鉴**（DeepSeek Harness 的 landlock 实现是 Node 原生插件）。它同时卡住 M0 出口的"干成一件事"和 M1 的读己之写验收。W1 的 spike 不能省。

---

## 11. 对外依赖：只剩 AgentCore

SandboxRS 排进来之后，两个内核的相互依赖已在计划内闭合。剩余的外部硬依赖只有 AgentCore：

```text
W1   Core: SandboxGrant 载荷与签名确认（Q1）        ← 自 W8 提前，因 S1 W1 就要冻结
W3   Core: ContentStore 后端与 GC 策略（Q3）
W3   Core: 事件存储形态与保留期（Q5）
W6   Core: 审批令牌保管与唤醒（Q4）
W11  Core: Team 消息与任务板载荷（Q9）
W22  Core: 记忆 FTS 与技能解析可用
W31  Core: 团队编排面就绪
```

**Q1 从 W8 提前到 W1** 是本次联合排期带来的最重要变化：SandboxRS 的执行协议在第一周就要冻结，而 grant 载荷是它的核心输入。这条不满足，S1 全线无法开工。

其余四条（Q3/Q4/Q5/Q9）未确认时可用最保守假设 + fake 顶住，风险可控。

**建议**：把本节时间线同步给 Core 团队作为交付节点，每周同步会只看这一张表。

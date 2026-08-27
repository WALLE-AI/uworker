# uworker 规模估算与排期校准报告

> 日期：2026-08-24
>
> 依据：[AgentRS 技术架构设计与实施方案 v1.6](agentrs-技术架构设计与实施方案.md)、[任务级实施分解 v1.6](agentrs-任务级实施分解与源码借鉴清单.md)
>
> 目的：回答"按当前架构，uworker 大约要写多少代码、需要多久"，并据此校准现有排期。
>
> 口径：所有行数为**物理行**（含空行与注释），实现与测试**分开计**。测量方法见附录，可复现。

---

## 1. 摘要

| 问题 | 结论 |
|---|---|
| 完整产品多大 | **净实现约 157,500 行，含测试约 380,000 行** |
| 第一个可用闭环多大 | **净实现约 79,000 行，含测试约 205,000 行** |
| 现有排期（3 人 12 周）够吗 | **不够，差 3–5 倍。**同样 3 人，最小可用闭环现实工期为 **7–14 个月** |
| M0（Phase A）现实工期 | **10 周左右，不是 4 周** |
| 最大的可削减项 | **AgentUI（37,000 实现 / 67,000 含测试）**，其次是 AgentCore 的 Team 编排（12,000） |
| 建议 | 首版走 **CLI + dev-adapter**，不自建 AgentUI；多 Agent **留契约砍实现** |

一个必要的更正：此前口头引用的 `aionui-team` 为 27,166 行，那是**实现 + inline 测试**的合计。其净实现是 **13,865 行**，与 aionrs 净实现 30,493 相比约为 45%，而非"一个量级"。本报告全部采用分离口径。

---

## 2. 测量方法

三个参考系统均为本地实测，非估计。Rust 侧需要特别处理 inline `#[cfg(test)]`——AionCore 有 438/860 个文件把测试写在同一文件内，若只按文件名过滤会高估实现量 76%。

判定规则：
- 文件名含 `test` 或位于 `tests/` 目录 → 全部计入测试；
- 其余文件从第一行 `#[cfg(test)]` 起到文件末尾 → 计入测试，之前部分计入实现。

### 2.1 锚点数据

| 系统 | 语言 | 净实现 | 测试 | 测试比 | 开发周期 | 有效核心人力 |
|---|---|---:|---:|---:|---|---:|
| aionrs | Rust | 30,493 | 49,378 | **1.62** | 2026-04-01 → 08-14（19.5 周） | ~3.5 |
| AionCore | Rust | 155,794 | 255,936 | **1.64** | 2026-04-09 → 08-19（18.6 周） | ~5.5 |
| DeepSeek Harness | TS | 198,836 | — | — | 仓库为压平导入，无历史 | — |
| AionUi | TS/TSX | 156,436 | — | — | — | — |

**两个独立 Rust 项目的测试比都是 1.63。**这是本报告最可靠的单一系数，下文一律按 ×1.6 折算。

有效人力由提交分布反推（排除 bot）：aionrs 187 次人工提交中前 4 人占 161 次；AionCore 1,310 次中前 5 人占 1,162 次。故取 3.5 与 5.5。

### 2.2 生产率反推

| 系统 | 总行数 | 人周 | 行/人周 |
|---|---:|---:|---:|
| aionrs | 79,871 | 68 | **1,175** |
| AionCore | 411,730 | 102 | **4,036** |

两者相差 3.4 倍。差异来源可辨：AionCore 含大量 DTO、路由、数据库 schema 等**结构化重复代码**，而 aionrs 是密度更高的算法与协议代码。

**AgentRS 属于后者**——不变式密集、状态机为主、要求 property test 与崩溃注入，因此本报告对 AgentRS 采用 **1,175 行/人周**；对 AgentCore/AgentUI 这类偏 CRUD 与界面的部分采用 **2,500 行/人周**（取两者之间偏保守值）。

---

## 3. AgentRS 逐模块估算

| crate | 实现 | 其中移植 | 新写 | 依据 |
|---|---:|---:|---:|---|
| `agentrs-contracts` | 3,500 | 0 | 3,500 | 全新：ID/DTO/事件 envelope/RunEpoch/SpecVersion/ContentRef/PolicyDecision/PermissionMode/LegalizationOp/ExternalFact + 全部 port trait |
| `agentrs-types` | 600 | 373 | 227 | A 类移植 + `ContentRef` 变体 |
| **`agentrs-runtime`** | **9,000** | 343 | 8,657 | 最大单项。Harness 对应物（session 3,181 + agent-loop 1,668 + scope 588 = 5,437）之外，我们另有 epoch 围栏、StepIntent/RecoveryPlanner、owner/cleanup、Surface、inbox/claim、审批挂起、ExternalFact |
| `agentrs-provider` | 7,000 | 5,350 | 1,650 | A 类移植大头（5 厂商 2,113 + framing/parser/stream 1,456 + projector/sanitize 1,071 + compat 546 + cache_diag 164），新写 HistoryLegalization 与缓存断点放置 |
| `agentrs-context` | 4,300 | 1,500 | 2,800 | 移植 compact 与 token 账本；新写 Surface 投影集成、manifest 装配、缓存分段、`BoardSnapshotRef` |
| `agentrs-tools` | 3,000 | 458 | 2,542 | 主要是 ToolLoop 固定管线、单调 guard、grant 消费记账、并发调度 |
| `agentrs-skills` | 1,600 | 1,095 | 505 | B 类移植为主 |
| `agentrs-memory` | 600 | 0 | 600 | 只有 port 与 selector 协议，实现在 Core |
| `agentrs-subagents` | 2,200 | 0 | 2,200 | ChildRun + AgentProfile + SubagentSummary + `MemberRunSpec` 派生 |
| `agentrs-prompts` | 1,200 | 0 | 1,200 | 版本化提示 + 输出 schema + fixtures |
| `agentrs-observability` | 3,000 | 0 | 3,000 | Projection Registry + Trajectory + replay + 脱敏 + OTel |
| `agentrs-cli` | 1,500 | 0 | 1,500 | 对照 aionrs cli 1,323 |
| `agentrs-testkit` | 3,500 | 0 | 3,500 | fake ports + 确定性时钟 + interleaving scheduler + host conformance suite |
| `agentrs-dev-adapter` | 2,000 | 0 | 2,000 | JSONL persistence + CAS + 终端审批 + 受限 sandbox |
| `agentrs-components`（Phase D） | 2,500 | 0 | 2,500 | manifest/inventory/profile/generation 事务 |
| **合计** | **45,500** | **9,119** | **36,381** | |

**含测试约 118,000 行。**

两点交叉验证：

1. 相对 aionrs（30,493）为 1.49 倍。加法是事件账本、Surface、owner 体系、投影、testkit、conformance、contracts 层；减法是 TUI（3,774）、全局 config（2,898）、工具执行体（1,338）。比例自洽。
2. **模块移植省下约 9,100 行**，占 AgentRS 实现的 20%，且集中在最枯燥易错的部分（SSE 分帧、五个厂商 wire format、projector）。这是 Q11 决策的量化收益。

---

## 4. SandboxRS 逐模块估算

| 模块 | 实现 | 依据 |
|---|---:|---|
| 工具执行体（read/edit/write/grep/glob/exec） | 3,000 | aionrs 对应 1,338，但需加 grant 校验与 overlay 感知 |
| OS 隔离（landlock / seccomp / Job Object，跨平台） | 3,500 | **无可移植参照**。aionrs `aion-process` 仅 381 行且只做 kill 传播，不是隔离 |
| ChangeSet overlay + undo + 回收区 | 4,000 | 无现成参照，本项目原创负担 |
| grant 校验 + `input_hash` 复核 + reconcile | 1,500 | 宿主义务 H1/H2 的实现 |
| PTY / 持久终端 | 2,000 | aionui-shell 3,812 参照 |
| 执行协议 + IPC | 1,500 | |
| **合计** | **15,500** | |

**含测试约 40,000 行。**

风险提示：OS 隔离与 overlay 两项合计 7,500 行是**全新且平台相关**的代码，估算不确定度最高（可能 ±60%）。三个参考系统中只有 DeepSeek Harness 有真实隔离实现（`native/node-addon-landlock-run`），且是 Node 原生插件，无法直接借鉴到 Rust。

---

## 5. AgentCore 逐模块估算

| 模块 | 实现 | 锚点 |
|---|---:|---|
| RunPersistence + ContentStore + schema/迁移 | 9,000 | aionui-db 13,924（含更多业务表） |
| Policy / 审批 / grant 签发 / AuthorityEnvelope | 4,500 | — |
| 记忆（FTS 索引 / 检索 / 权限 / 保留） | 4,000 | aion-memory 1,338 + FTS 实现 |
| 技能（发现 / 加载 / 权限 / 参数） | 3,500 | aion-skills 3,236 |
| MCP 连接 / OAuth / 生命周期 | 4,500 | aionui-mcp 5,134 |
| **Team 编排**（实体/任务板/邮箱/调度/租约/崩溃/死锁/可见性） | **12,000** | **aionui-team 13,865** |
| 会话 / 项目 / 工作区 | 7,000 | aionui-project 7,724 |
| 账号 / 凭据 / 计费 / 模型策略 | 5,000 | |
| Hook 运行器 | 1,500 | |
| API / IPC / BFF | 5,500 | aionui-app 10,955 的子集 |
| AgentRS / SandboxRS adapter 层 | 3,000 | `manager/aionrs` 参照 |
| **合计** | **59,500** | |

**含测试约 155,000 行。**

一个必须正视的事实：**Team 编排单项 12,000 行，是 AgentRS 侧全部多 Agent 契约（约 2,000 行）的六倍。**"编排归 Core、内核只强制三个契约"这个划分是正确的（它保住了四平面模型），但它**不减少总工作量，只是把它挪到了 Core**。

---

## 6. AgentUI 逐模块估算

| 模块 | 实现 |
|---|---:|
| 对话 / transcript / 流式渲染 | 8,000 |
| Trajectory 表格 / 时间线 / 局部检查器 | 5,000 |
| 审批卡片 / 权限模式 / ChangeSet diff 审阅 | 5,000 |
| Team 视图（成员 / 任务板 / 消息） | 4,500 |
| 工作区 / 项目 / 设置 / 模型选择 | 6,000 |
| Electron 主 / 预加载 / IPC | 3,500 |
| 组件库 / 主题 / i18n | 5,000 |
| **合计** | **37,000** |

TS 测试比取 0.8 ⟹ **含测试约 67,000 行**。

参照：AionUi 156,436（成熟产品，含 mobile），Harness client 72,428。37,000 是"可用但不繁复"的首版口径。

---

## 7. 总计与交叉校准

| 组件 | 净实现 | 含测试 | 占比 |
|---|---:|---:|---:|
| AgentRS | 45,500 | 118,000 | 29% |
| SandboxRS | 15,500 | 40,000 | 10% |
| AgentCore | 59,500 | 155,000 | 38% |
| AgentUI | 37,000 | 67,000 | 18% |
| **合计** | **157,500** | **380,000** | |

**交叉校准**：AionCore（155,794）+ AionUi（156,436）= 312,230 净实现，是一个已上线的成熟桌面 Agent 产品。我们估 157,500，**约为其一半**。考虑到 uworker 不做 channel / extension / cron / office / realtime / mobile 这些产品面，这个比例是合理的。

估算不确定度：整体 **±35%**，即 102,000–213,000 净实现。不确定度最高的三项是 SandboxRS 隔离与 overlay（±60%）、AgentCore Team 编排（±40%）、AgentUI（±50%，取决于是否自建）。

---

## 8. 排期校准

### 8.1 现有排期与规模不匹配

现有排期为**3 人 12 周**到 M3。折算 36 人周，按 AgentRS 的 1,175 行/人周计，产能约 **42,000 行含测试**。

而最小可用闭环需要：

| 组件 | 实现 | 含测试 |
|---|---:|---:|
| AgentRS（Phase A–C） | 40,000 | 104,000 |
| SandboxRS 最小（Read + Exec + overlay + reconcile） | 7,000 | 18,000 |
| AgentCore 最小（persistence + content + policy + 审批，**不含 Team/MCP/记忆 FTS**） | 20,000 | 52,000 |
| AgentUI 最小（对话 + 审批 + diff 审阅） | 12,000 | 22,000 |
| **合计** | **79,000** | **196,000** |

**196,000 ÷ 42,000 ≈ 4.7 倍缺口。**

### 8.2 M0 的现实工期

Phase A（M0）的 AgentRS 部分：

| 内容 | 实现 |
|---|---:|
| contracts 全量 | 3,500 |
| types | 600 |
| runtime 核心（RuntimeHost/Engine/owner/Surface/inbox/审批挂起/恢复） | 5,000 |
| testkit 首版（fake ports + 确定性时钟 + 崩溃注入） | 2,000 |
| provider A 类移植早期（framing/parser/单厂商） | 2,000 |
| CLI 骨架 | 500 |
| **合计** | **13,600**（含测试 35,400） |

35,400 ÷ 1,175 = **30 人周**。3 人并行 ⟹ **10 周**，而非现有排期的 4 周。

### 8.3 全量与最小闭环的现实区间

| 目标 | 含测试 | 人周 | 3 人 | 5 人 |
|---|---:|---:|---|---|
| M0（AgentRS 可恢复闭环） | 35,400 | 30 | **10 周** | 6 周 |
| 最小可用闭环（M0–M2 + 最小 Core/Sandbox/UI） | 196,000 | 167 | **56 周** | 33 周 |
| 完整产品 | 380,000 | 约 240（混合费率） | 80 周 | 48 周 |

**结论：3 人做到"能用"，现实是 12–14 个月，不是 12 周。**即便按 AionCore 那种偏 CRUD 的高费率（4,036 行/人周）折算最小闭环，也需 49 人周 = 3 人 16 周，仍是现有排期的 1.4 倍——而 AgentRS 显然不适用那个费率。

---

## 9. 三条削减路径

按杠杆从大到小。

### 路径 A：首版不自建 AgentUI，走 CLI + dev-adapter（推荐）

| 削减 | 37,000 实现 / 67,000 含测试 |
|---|---|
| 影响 | 首版无桌面界面 |
| 代偿 | `agentrs-cli` 与 `agentrs-dev-adapter` 已在规划内（合计 3,500 实现），它们本就是为 dogfooding 设计的；把它们做扎实即可支撑内部使用与早期验证 |
| 风险 | 无法向非技术用户演示；Trajectory 只能以 JSON 查看 |

**这是唯一能一次性削掉 18% 总量的选项**，且不动任何架构决策。若确需界面，第二选择是适配现有 AionUi 而非新写。

### 路径 B：多 Agent 留契约、砍实现

| 削减 | AgentCore Team 编排 12,000 实现 / 31,000 含测试 |
|---|---|
| 保留 | AgentRS 侧的 `ExternalFact` 契约（约 800 行）**必须保留** |
| 理由 | `ExternalFact` 改变 inbox/claim 与 Surface 的契约，属于结构层地基。**留契约的成本是 800 行，事后补的成本是改地基。** |
| 影响 | 首版只有函数式子 Agent（Explore/Plan/compact），无协作式团队 |

### 路径 C：首版单 Provider

| 削减 | provider 移植与适配约 2,500 含测试 |
|---|---|
| 影响 | 无模型 fallback，被单一厂商限流即阻塞 |
| 评价 | **杠杆最小且代价不成比例**，不建议——五个厂商适配是 A 类直接移植，边际成本本就很低 |

### 推荐组合

**路径 A + 路径 B**，最小闭环从 196,000 降到 **约 98,000 含测试**：

| 目标 | 含测试 | 人周 | 3 人 |
|---|---:|---:|---|
| 削减后最小闭环 | 98,000 | 83 | **28 周（约 6.5 个月）** |

这仍然不是 12 周，但已是一个可以承诺的数字。

---

## 10. 风险与不确定性

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| 1 | **测试比可能高于 1.6** | 方案要求 property test、interleaving scheduler、崩溃注入、conformance suite、golden fixtures，AgentRS 实际可能到 1.8–2.0。每 +0.2 增加约 9,000 行 | M0 结束时实测本项目的真实测试比，据此重算 |
| 2 | **SandboxRS 隔离与 overlay 无参照** | 7,500 行全新平台相关代码，±60% | 尽早做技术验证（spike），不要等到 Phase B |
| 3 | **1,175 行/人周的费率取自 aionrs** | 若团队不熟悉 Rust 或架构复杂度高于 aionrs，实际会更低 | 用 M0 的真实产出反推本团队费率，第 4 周即可校准 |
| 4 | **AgentCore 被低估** | 它是最大单项（59,500），但本报告对它的分解粒度最粗 | 由 Core 团队独立做一次同口径估算 |
| 5 | **架构尚未被任何代码验证** | 2,800 行文档、0 行代码。若 M0 暴露结构性问题，估算全部作废 | M0 验收已包含"能干成一件事"的真实闭环，这是最早的证伪点 |

---

## 11. 建议

1. **把排期改成有依据的版本**：M0 = 10 周，削减后最小可用闭环 = 28 周。现有的 12 周到 M3 无论怎么调配人力都不成立。
2. **采纳路径 A + B**：首版不自建 UI，多 Agent 留契约砍实现。
3. **第 4 周做一次费率校准**：用 M0 前四周的真实产出（实现行数 / 测试行数 / 人周）反推本团队费率与测试比，重算全部估算。**本报告的所有数字都应在那时被替换。**
4. **SandboxRS 的隔离与 overlay 提前做技术验证**，不要等 Phase B。它既是最大的估算不确定项，也是"内核无执行权"这一裁决的兑现前提。
5. **AgentCore 由 Core 团队独立同口径估算**——本报告对它的分解最粗，而它是最大单项。

---

## 附录：测量方法（可复现）

Rust 项目的实现/测试分离，需处理 inline `#[cfg(test)]`：

```python
import os
def split(path):
    lines = open(path, encoding='utf-8', errors='ignore').read().split('\n')
    for i, l in enumerate(lines):
        if l.strip().startswith('#[cfg(test)]'):
            return i, len(lines) - i          # (实现, 测试)
    return len(lines), 0

impl = test = 0
for root, _, files in os.walk('.'):
    if '/target/' in root: continue
    for f in files:
        if not f.endswith('.rs'): continue
        p = os.path.join(root, f)
        i, t = split(p)
        if 'test' in f.lower() or '/tests/' in p:   # 整文件计入测试
            t += i; i = 0
        impl += i; test += t
print(f"实现 {impl} / 测试 {test} / 比 {test/impl:.2f}")
```

工期与有效人力：

```sh
git log --reverse --format="%ad" --date=short | head -1   # 起始
git log -1 --format="%ad" --date=short                    # 最近
git shortlog -sn HEAD                                     # 贡献分布（排除 bot 后取头部）
```

三个参考系统的测量结果见 §2.1。若参考仓库更新，重跑上述脚本即可刷新全部锚点。

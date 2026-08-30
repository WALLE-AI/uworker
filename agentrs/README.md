# AgentRS

uworker 的推理与工作流内核：Rust 编写、可嵌入、**无执行权**。

它把"用户目标 + 已授权上下文 + 可用能力"推进为可审计的步骤流。它**不**拥有 UI、
数据库、用户身份、策略规则、文件写权限或 OS 进程——改变世界的动作必须经 AgentCore
的裁决与 SandboxRS 的执行。

## 当前状态

AgentRS 内核已具备可运行的 provider、上下文装配、工具管线、持久化恢复、
ContentRef、memory/skills、函数式子 Agent、MemberRun 派生、轨迹/脱敏/OTel 投影，
component generation/Inventory/profile 原子换代，以及
`run` / `resume-run` / `serve --jsonl` / conformance CLI。

仍属于外部交付的部分不能由本仓库单独宣称完成：真实 SandboxRS 的 L1 隔离与 PTY、
AgentCore 的团队编排/审批预算，以及真实 MCP 进程的生命周期和凭据管理。
本仓库为这些能力提供冻结 port、MCP 目录映射和 conformance suite；接入实现必须通过
H1/H2/H3/H4/H5/H7 验收。

外部实现的逐项接入门见 [External Integration Gates](docs/external-integration-gates.md)。

## 本地校验

```sh
cargo test --workspace          # 单测。不需要网络、文件系统、真实时钟或 OS 进程
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check-no-env.sh       # 边界判据
./scripts/check-port-attribution.sh   # Apache-2.0 §4b 移植合规
```

## Dev TUI（非生产测试宿主）

`agentrs-tui` 用于在终端中端到端测试 AgentRS runtime、真实 OpenAI 兼容模型、
工具审批、ChangeSet 和 durable replay。它使用 `agentrs-dev-adapter` 的 L0 基础围栏，
不是 AgentUI，也不是生产 SandboxRS。

```sh
export AGENTRS_BASE_URL=https://api.siliconflow.cn/v1
export AGENTRS_API_KEY=...                 # 只通过进程环境注入
export AGENTRS_MODEL=Pro/deepseek-ai/DeepSeek-R1
cargo run -p agentrs-dev-tui -- --workspace . "Inspect this workspace"
```

读工具自动放行；`Write`、`Edit`、`Delete` 必须在 TUI 中逐次审批。所有写操作先进入
内存 ChangeSet，运行结束后按 `C` 提交或按 `D` 丢弃。`Ctrl+C` 请求安全取消，再按一次
强制退出。`--resume <jsonl>` 可以离线重建已提交 transcript。

支持矩阵：

| 平台 | 构建/单测 | 交互 PTY | 说明 |
|---|---:|---:|---|
| Windows 11 / PowerShell | 已验证 | 已验证 | 当前真实 LLM 与终端恢复基线 |
| Linux | CI 待接入 | 待验证 | Ratatui/Crossterm 代码路径受支持 |
| macOS | CI 待接入 | 待验证 | Ratatui/Crossterm 代码路径受支持 |

最低 Rust 版本保持 workspace 的 1.85；TUI 固定使用 Ratatui 0.29 和 Crossterm 0.28。
API key 不进入 TUI state、durable JSONL 或屏幕诊断。

## 设计入口

- [架构方案](../agentrs-技术架构设计与实施方案.md) —— 完整契约与不变式
- [架构说明（评审版）](../agentrs-架构说明-评审版.md) —— 五张图 + 关键裁决
- [内核迭代计划](../uworker-内核迭代计划.md) —— 排期与出口标准

## 许可

Apache-2.0。部分模块自 aionrs 移植，见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

# AgentRS

uworker 的推理与工作流内核：Rust 编写、可嵌入、**无执行权**。

它把"用户目标 + 已授权上下文 + 可用能力"推进为可审计的步骤流。它**不**拥有 UI、
数据库、用户身份、策略规则、文件写权限或 OS 进程——改变世界的动作必须经 AgentCore
的裁决与 SandboxRS 的执行。

## 当前状态

**W1 完成**：契约首版已冻结（见 [ADR 0001](docs/adr/0001-contracts-freeze-w1.md)）。
其余 crate 为骨架，按[内核迭代计划](../uworker-内核迭代计划.md)分阶段填充。

```
crates/agentrs-contracts   ← 已冻结，48 个测试
crates/agentrs-*           ← 骨架
```

## 本地校验

```sh
cargo test --workspace          # 单测。不需要网络、文件系统、真实时钟或 OS 进程
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check-no-env.sh       # 边界判据
./scripts/check-port-attribution.sh   # Apache-2.0 §4b 移植合规
```

## 设计入口

- [架构方案](../agentrs-技术架构设计与实施方案.md) —— 完整契约与不变式
- [架构说明（评审版）](../agentrs-架构说明-评审版.md) —— 五张图 + 关键裁决
- [内核迭代计划](../uworker-内核迭代计划.md) —— 排期与出口标准

## 许可

Apache-2.0。部分模块自 aionrs 移植，见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

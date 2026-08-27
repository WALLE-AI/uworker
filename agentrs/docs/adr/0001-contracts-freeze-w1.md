# ADR 0001：W1 契约首版冻结

- 状态：已接受
- 日期：2026-08-25
- 关联：[架构方案 v1.6](../../../agentrs-技术架构设计与实施方案.md)、[内核迭代计划](../../../uworker-内核迭代计划.md) §5.1

## 背景

迭代计划把 `agentrs-contracts` 的首版冻结定为 W1 的硬约束，理由是：R1（内核状态机）、
R2（端口与验证）、R3（Provider 与上下文）、S1（执行与协议）四条线全部依赖它。
不冻结就无法并行；冻结后重构则波及全部四条线。

## 决定

**W1 冻结以下契约，此后只增量演进，不重构。**

| 模块 | 内容 |
|---|---|
| `ids` | 全部 newtype 标识符；`EventSequence` / `LiveSequence` **两个序空间分离**；`RunEpoch` |
| `version` | `SpecVersion` 与兼容窗口；四种判定，拒绝静默降级 |
| `event` | 事件信封、三类持久性、因果字段、未知事件兜底 |
| `surface` | `SurfaceOp{Append, Replace}`、`SurfaceGeneration`、三类 Surface 事件 |
| `content` | `ContentRef` / `ContentScope` / `RetentionOwner` / 解引用降级 |
| `authority` | `AuthorityEnvelope` / `CapabilityView` / `CapabilityViewDigest` / `PermissionMode` |
| `policy` | `PolicyDecision` / `SandboxGrant` / `InputHash` / `DecisionSource` / `ApprovalOutcome` |
| `sandbox` | `IsolationLevel`（L0/L1/L2）/ `ExecutionRequest` / `ExecutionStatus` 三态 |
| `manifest` | `OperationView` / `CacheSegment` / `LegalizationOp` / `TokenAccounting` |
| `external` | `ExternalFact` / `FactOrigin` / `BoardSnapshotRef` |
| `spec` | `RunSpec` / `ConfigTree` / `ForkSpec` / `MemberRunSpec` / 预算 |
| `ports` | 10 个端口 trait |

## 几处由类型而非文档保证的不变式

设计时刻意把关键约束落到类型上，因为文档挡不住的错误，类型可以：

| 不变式 | 类型手段 |
|---|---|
| durable 与 live 序号不可混用 | `EventSequence` / `LiveSequence` 是两个 newtype，无互转 |
| 任何 agent 不得授权 | `DecisionSource` 只有 `Human` 与 `Policy` 两个变体，无 `Agent` |
| 可插拔的东西只能收紧 | `HookOutcome` 无 `Allow` 变体，放行是"不否决"的结果 |
| 跨 Run 消息不携带能力 | `ExternalFact` 结构上没有任何 authority/capability/grant 字段 |
| 执行必须绑定 ChangeSet | `ExecutionRequest::change_set_id` 非 `Option` |
| 执行器不得对隔离级别沉默 | `ExecutionResult::effective_isolation` 非 `Option` |
| 分叉点落在 durable 事实上 | `ForkSpec::boundary` 类型是 `EventSequence` 而非 `LiveSequence` |

## 边界判据的机器执行

架构 §1.1 的"内核不碰环境"从文字规则变成两道 CI 门：

- `clippy.toml` 禁用 `std::fs::File` / `std::process::Command` / `std::net::TcpStream` /
  `SystemTime::now` / `Instant::now`；
- `scripts/check-no-env.sh` 兜住 clippy 覆盖不到的写法，`agentrs-dev-adapter` 是唯一豁免。

## 后果

- 四条线可在 W2 真正并行；
- 契约变更此后走增量：加字段用 `#[serde(default)]`，加事件靠 `#[serde(other)]` 兜底；
- 破坏性变更需要新 ADR 并同步 `SpecVersion` 兼容窗口。

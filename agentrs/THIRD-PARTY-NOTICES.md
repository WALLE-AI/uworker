# 第三方来源与归属

本仓库部分模块自 **aionrs**（Apache-2.0）移植。移植是一次性的，不与上游建立 rebase 关系。

## Apache-2.0 义务落实

| 条款 | 落实方式 |
|---|---|
| §4(a) 附带许可证副本 | 仓库根目录 [`LICENSE-APACHE`](LICENSE-APACHE) |
| §4(b) 修改文件须显著标注 | 每个移植文件头部标注来源/commit/修改摘要，由 `scripts/check-port-attribution.sh` 在 CI 强制 |
| §4(c) 保留原有版权声明 | 不删除源文件任何版权头 |
| §4(d) NOTICE | aionrs 无 NOTICE 文件，无此项义务；本文件为主动披露 |
| §6 商标 | 不在 crate 名、模块名、公开 API 或文档中使用 `aion` 相关标识 |

文件头模板：

```rust
// Ported from aionrs (Apache-2.0).
//   Source: <repo-url>/crates/aion-providers/src/framing.rs @ <commit>
//   Copied: 2026-xx-xx   Modified: yes
//   Changes: 替换错误类型为 agentrs-contracts::ProviderError；移除全局 config 依赖。
```

## 移植清单

**清单在移植发生时逐条维护，不允许发布前突击补写**——事后回忆哪些文件是搬的必然会漏。

### A 类：直接复制（约 6,800 行）

源仓库 commit：`a5df989d110fb424bcd496b413e7ce7e20754414`

| 目标文件 | 源路径 | 行数 | 状态 |
|---|---|---:|---|
| `crates/agentrs-types/src/message.rs` | `crates/aion-types/src/message.rs` | 237 | ✅ W2 |
| `crates/agentrs-types/src/llm.rs` | `crates/aion-types/src/llm.rs` | 53 | ✅ W2 |
| `crates/agentrs-provider/src/framing.rs` | `crates/aion-providers/src/framing.rs` | | 待办 |
| `crates/agentrs-provider/src/parser.rs` | `crates/aion-providers/src/parser.rs` | | 待办 |
| `crates/agentrs-provider/src/anthropic.rs` | `crates/aion-providers/src/anthropic_shared.rs` @ `f711174` | 441 | ✅ Phase B（剥离 sanitize/tracing/generate_tool_id，增加 cache_control 断点） |
| `crates/agentrs-provider/src/openai.rs` | `crates/aion-providers/src/openai.rs` | | 待办 |
| `crates/agentrs-provider/src/anthropic_wire.rs` | `crates/aion-providers/src/{bedrock,vertex}.rs` @ `f711174` | 707 | ✅ Phase B（**只取线格式**；凭据链与 SigV4 签名按 §1.1 归 Core，未移植） |
| `crates/agentrs-provider/src/openai_responses.rs` | `crates/aion-providers/src/openai_responses{,_projector}.rs` @ `f711174` | 447 | ✅ Phase B（剥离 generate_call_id / orphan 清理 / 静默降级） |
| `crates/agentrs-provider/src/compat.rs` | `crates/aion-config/src/compat.rs` | | 待办 |
| `crates/agentrs-context/src/cache_diagnostics.rs` | `crates/aion-agent/src/cache_diagnostics.rs` @ `f711174` | 164 | ✅ Phase B（归因改走 cache.rs 分段；新增 Unsupported 判定） |
| `crates/agentrs-context/src/sanitize.rs` 等 | `crates/aion-compact/src/*` | | 待办 |

### B 类：复制逻辑，改造接缝（约 2,900 行）

| 目标文件 | 源路径 | commit | 改造要点 | 状态 |
|---|---|---|---|---|
| _(Phase B 起填充)_ | | | | 未开始 |

### C 类：明确不移植

`aion-tui`、`engine/session/orchestration/bootstrap/confirm`、`aion-config` 全局加载、
`aion-skills` 的 shell/executor/discovery、工具执行体、`aion-process`。

**C 类不得以"临时/调试用"为由进入仓库**——尤其 `aion-process`，它一进来"内核无执行权"就破了。

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

---

## dsh-code-agent（MIT）—— `agentrs-dev-tui` 的交互形式

`agentrs-dev-tui` 的界面与键盘模型自 **dsh-code-agent**（`packages/dsh-tui`，MIT）
逐模块移植，commit `d7cd008`。移植的是**交互形式**：字形集、语义色板、转录条目与
折叠预算、工具卡片、连续查阅合并、working line、状态行的分优先级丢弃、通知队列、
草稿编辑模型、审批选项行、按键表。原实现是 TypeScript/Ink，本仓库是 Rust/Ratatui，
因此没有一行代码是逐字复制的；但设计、判据与行为是它的，所以按 MIT 的要求署名。

**不移植**：其 `src/ink/` 分叉渲染器、`brand.ts` 的品牌美术（DeepSeek 鲸鱼与字体表，
另有其自身的第三方归属）、`harness-adapter.ts` / `plugin.ts`（对接 DeepSeek Harness，
与 AgentRS 无关）、会话持久化与 `$DSH_HOME` 布局。

| 目标文件 | 源路径（`packages/dsh-tui/src/`） | 改造要点 |
|---|---|---|
| `crates/agentrs-dev-tui/src/text.rs` | `terminal-text.ts` | 宽度改用 `unicode-width` crate，替换原手写区间表 |
| `crates/agentrs-dev-tui/src/glyphs.rs` | `glyphs.ts` | 增补树形前缀；去掉未用的 `reasoning` |
| `crates/agentrs-dev-tui/src/theme.rs` | `theme.ts` + `styling.ts` | 色调解析为 ratatui `Style`；`ansi256` 并入 `Basic` |
| `crates/agentrs-dev-tui/src/styling.rs` | `styling.ts` | 分段切分按 `char` 而非 UTF-16 码元；随美术一并去掉字面色 |
| `crates/agentrs-dev-tui/src/capabilities.rs` | `terminal-capabilities.ts` | 探测接受已读入的环境快照；终端尺寸交给 ratatui 逐帧上报 |
| `crates/agentrs-dev-tui/src/spinner.rs` | `spinner.ts` | 行为不变 |
| `crates/agentrs-dev-tui/src/markdown.rs` | `markdown.ts` | 七个固定形状手写匹配，不引入正则引擎；优先级与输出不变 |
| `crates/agentrs-dev-tui/src/diff.rs` | `diff-view.ts` | 显式建模"文件被删"；行携带色调枚举而非标记字符 |
| `crates/agentrs-dev-tui/src/tool_card.rs` | `tool-card.ts` | 卡片种类取自显式名字表（`ToolDef` 无展示层字段）；未知名字仍落 `Generic` |
| `crates/agentrs-dev-tui/src/transcript.rs` | `transcript-view.ts` | 去掉 scrollback 切分与行缓存（本宿主保留 alternate screen） |
| `crates/agentrs-dev-tui/src/collapse.rs` | `collapse.ts` | 单条规则直接应用，不做规则注册表 |
| `crates/agentrs-dev-tui/src/working_line.rs` | `working-line.ts` | 省略号取自字形集；去掉子 Agent 行 |
| `crates/agentrs-dev-tui/src/status_line.rs` | `status-line.ts` | 段位按 AgentRS 实有投影调整：无 todo/subagent，增 ChangeSet 与丢弃计数 |
| `crates/agentrs-dev-tui/src/notices.rs` | `notifications.ts` | 每条通知的 `fold` 闭包换成内建重复计数，队列保持可比较的纯值 |
| `crates/agentrs-dev-tui/src/composer.rs` | `composer.ts` | 草稿历史留在进程内；括号粘贴标记无需剥离（crossterm 有独立事件） |
| `crates/agentrs-dev-tui/src/approval.rs` | `approval-options.ts` | "放宽"一行改为切换下一次 run 的 `PermissionMode`（内核无会话内切换通路） |
| `crates/agentrs-dev-tui/src/keymap.rs` | `keymap.ts` | 和弦由 crossterm 事件构造；动作集取本宿主能兑现的子集 |
| `crates/agentrs-dev-tui/src/keybindings.rs` | `keybindings.ts` | 不支持多键序列（无动作需要，且 16 ms 帧循环里再挂一个一秒前缀计时器代价真实） |
| `crates/agentrs-dev-tui/src/overlay.rs` | `overlay.ts` + `session-browser.ts` + `session-selector.ts` + `transcript-mode.ts` | 一个窗口模型服务四个面，不是四份近似副本 |
| `crates/agentrs-dev-tui/src/surfaces.rs` | `overlay.ts`(helpRows) + `session-browser.ts` | 命令集是本宿主的；会话浏览器改为 durable JSONL 日志浏览器 |
| `crates/agentrs-dev-tui/src/completion.rs` | `draft-completion.ts` + `workspace-files.ts` | 候选由调用方提供，匹配器保持纯函数，唯一一次目录读取落在 `host_io` |
| `crates/agentrs-dev-tui/src/ui.rs` | `views/*.tsx` + `app.tsx` | React/Ink → ratatui：同样的布局与丢弃顺序，改为命令式绘制 |

### MIT 许可证副本

```
MIT License

Copyright (c) 2026 dsh-code-agent contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

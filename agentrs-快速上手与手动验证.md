# agentrs 快速上手与手动验证

> 面向本机（`/home/dataset1/gaojing/uworker`）的实操手册：怎么编译、怎么连本地模型、
> 怎么进交互界面、以及怎么手动验证子智能体与 Team 的可观测性。
> 最后一次实测：2026-09-30。

---

## 0. 先读这一节：两个已知的坑

### 0.1 保存的全局配置已失效

`~/.config/agentrs/config.toml` 里写的是：

```toml
base_url = "http://127.0.0.1:19121/starvlm/v1"
model    = "Qwen3.6-35B-A3B"
```

实测 `/starvlm/v1/models` 已返回 **404**，只有 `/v1` 可用，端口上挂的模型是 **`Qwen3.8-27B`**
（进程为 `sglang::router`）。用前先探活：

```bash
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost
curl -s http://127.0.0.1:19121/v1/models | python3 -m json.tool
```

`NO_PROXY` 是**必须**的，否则会被公司代理拦成 502。端口和路径前缀都会变，
连不上时 `ss -lntp` 列端口再逐个 `curl :PORT/v1/models` 探活——**不要动 nginx**。

本文所有命令用 `-b` / `-m` 覆盖，不改你的全局配置。

### 0.2 弱模型调不动 Spawn（真实缺口，尚未修）

本地 27B 会把 `tasks` 参数序列化成**字符串**而不是数组：

```
Spawn({"tasks":"[{\"name\": \"a\", \"prompt\": \"...\"}]"})
[Spawn ERROR] Missing or invalid 'tasks' array
```

`crates/agentrs-agent/src/spawn_tool.rs` 的 `parse_tasks` 只接受真数组
（`input["tasks"].as_array()`），字符串形式直接拒绝，模型会连试数次然后报告
"Spawn 工具坏了"。OpenAI 兼容端点上的弱模型 stringify 嵌套参数很常见。

**影响**：用这个 27B 手动验**真实**子智能体派生与 Team 消息流走不通。
渲染之外的验证请换强模型，或先给 `tasks` 补一层「字符串则先 `from_str`」的兜底。

---

## 1. 编译

```bash
cd /home/dataset1/gaojing/uworker/agentrs
cargo build --release          # 产物：target/release/agentrs（约 17s 增量）
```

开发期用 `cargo build`（debug）也可以，启动更慢但编译更快。

---

## 2. 最短路径跑起来

```bash
cd /home/dataset1/gaojing/uworker/agentrs
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost
export AGENTRS=$PWD/target/release/agentrs

# 单次执行（非交互）
$AGENTRS -b http://127.0.0.1:19121/v1 -m Qwen3.8-27B "读 Cargo.toml 说明依赖"

# 交互式 TUI（不带 prompt 即进入）
$AGENTRS -b http://127.0.0.1:19121/v1 -m Qwen3.8-27B
```

常用开关：

| 开关 | 用途 |
|---|---|
| `-b` / `-m` / `-k` | 覆盖 base_url / 模型 / API key |
| `--profile <名>` | 用配置文件里的命名 profile |
| `--project-dir <目录>` | 指定读取 `.agentrs.toml` 的目录（默认 CWD） |
| `--auto-approve` | 跳过所有工具确认（手动验证时很省事） |
| `--max-turns N` | 限制本次 run 的模型轮数，`0` 关闭 |
| `--resume <id>` / `--fork-session` | 续跑 / 派生已保存会话 |
| `--json-stream` | 宿主集成用的 JSON 流模式 |
| `--log-dir <目录> --log-level debug` | 落文件日志；也可 `info,agentrs_providers=debug` |
| `--no-color` | 关颜色 |

辅助子命令：`agentrs config init|path`、`agentrs session list`、`agentrs skills path`、`agentrs auth`。

---

## 3. 手动验证用的工作区

已经建好 `/tmp/agentrs-manual`，其中 `.agentrs.toml`：

```toml
[team]
enabled = true          # Team 默认关闭，必须显式打开
max_members = 4
inbox_capacity = 16

[subagent]
enabled = true          # 默认就是 true
max_concurrent = 4
depth = 1               # 只允许直接子代理

[tools]
auto_approve = true     # 免得每个工具都要确认
```

要重建：

```bash
mkdir -p /tmp/agentrs-manual && cd /tmp/agentrs-manual
# 写入上面的 .agentrs.toml，再放个给模型读的文件
printf '# Scratch\nThe lexer lives in src/lexer.rs and the parser in src/parser.rs.\n' > notes.md
```

然后在这个目录里启动，配置会被自动读取：

```bash
cd /tmp/agentrs-manual && $AGENTRS -b http://127.0.0.1:19121/v1 -m Qwen3.8-27B
```

---

## 4. TUI 使用

| 按键 | 行为 |
|---|---|
| Enter | 发送 |
| Shift+Enter（或 Ctrl+J） | 换行 |
| `/` | 打开命令弹窗，上下键选择 |
| 鼠标滚轮 | 翻看已定稿的对话（终端原生 scrollback） |
| Ctrl+C | 停止当前轮；空闲时清草稿 / 退出 |
| Ctrl+D | 空输入时退出 |

鼠标捕获是刻意关掉的，直接拖选 + 终端复制快捷键即可。

**Agent 侧命令**（CLI REPL 与 TUI 共用）：`/compact` `/context` `/clear`
`/memory` `/agents` `/teams` `/help` `/quit`

**TUI 自有命令**：`/status` `/model [名]` `/permissions [模式]` `/new` `/resume [id]`
`/mcp` `/skills` `/todos`

---

## 5. 逐项手动验证清单

### 5.1 基本回路

| 步 | 操作 | 预期 |
|---|---|---|
| 1 | 启动 TUI | 欢迎区 + 底部 composer；首条消息发出前不落盘 session |
| 2 | `/status` | provider、模型、session、审批模式、上下文占用、压缩次数 |
| 3 | `读 notes.md 并总结` | 连续工具调用折叠成一个 `• Tools` 块，每条一行状态与预览 |
| 4 | `/context` | 分项上下文占用（系统提示、工具、记忆、历史） |
| 5 | `/resume` 不带 id | 全屏会话选择器，可键盘/鼠标导航；选中后不退出 UI 直接换 session |

### 5.2 子智能体可观测性（本次新增）

| 步 | 操作 | 预期 |
|---|---|---|
| 1 | `/agents`（空闲时） | 提示「No sub-agents are registered right now」，并说明一次性子智能体在派生它的那一轮内就结束了，要看转录里的 `• Agents` 块 |
| 2 | 让模型派生 2 个子智能体 | 转录出现 `• Agents` 组，`├─ / └─` 各一行；每行原地更新为 `running · N turns · N out`，**不会**每个事件刷一条新行 |
| 3 | 观察运行中 | composer 上方出现实时面板 `Agents 2/2`，每行 `▸ 名字  running · …`；超过 4 个时保留 `… N more` |
| 4 | 等全部结束 | 面板自动消失（不占行），转录里的行定格为 `done / failed / cancelled` |
| 5 | 再 `/agents` | 一次性子智能体已注销，回到第 1 步的文案 |

> 第 2 步在本地 27B 上会被 §0.2 的参数问题挡住。

### 5.3 Team 可观测性（本次新增）

需要 `[team] enabled = true`，且模型能正确调用 `TeamCreate` + `persistent: true` 的 `Spawn`。

| 步 | 操作 | 预期 |
|---|---|---|
| 1 | `/teams`（无 team 时） | 「No active team…」并指向 `TeamCreate` |
| 2 | 让模型建 team `core` 并派生持久成员 `alice`、`bob` | 转录出现两条成员行（初始 `idle`），面板计入活跃数 |
| 3 | 让 leader 给 alice 发消息 | 成员行的 `↑发 ↓收` 计数变化；**不会**为每条消息新增转录行；消息正文不渲染 |
| 4 | 广播（`to: "*"`） | 除发送者外每个活跃成员 `↓` +1 |
| 5 | `/teams` | team 名与创建时间、leader 及其 `inbox N/16`、每个成员的类型 / active-stopped / `inbox N/16` |
| 6 | `/agents cancel <id前缀>` | 取消成功；命中持久成员时**额外输出强制取消警告**，指明优雅路径是 `shutdown_request` + `TeamDelete` |
| 7 | 让 leader 走 `shutdown_request` → `TeamDelete` | 成员标记 stopped、team 目录被清理 |

### 5.4 记忆系统

| 步 | 操作 | 预期 |
|---|---|---|
| 1 | `/memory` | 列出记忆（类型、相对年龄如 `47 days ago`、描述），或说明尚无记忆 |
| 2 | `/memory path` | 打印记忆目录绝对路径 |
| 3 | 让模型写入一条记忆 | **同一会话的下一轮** system prompt 就能读到新索引行；首次写入后提示词从精简档升级为完整档 |
| 4 | `/memory show <file>` / `/memory rm <file>` | 打印全文 / 删除文件并清掉 `MEMORY.md` 索引行 |

---

## 6. 自动化测试

```bash
cd /home/dataset1/gaojing/uworker/agentrs
cargo fmt --all -- --check              # 应无 diff
cargo clippy --workspace --all-targets  # 应零告警
cargo test -p agentrs-tui               # 127 项
cargo test -p agentrs-agent --lib       # 622 项
cargo test -p agentrs-memory            # 227 项
cargo test -p agentrs-tools             # 需要 NO_PROXY（wiremock 走本地回环）
```

**已知失败**：`cargo test -p agentrs-agent` 的 acceptance 目标里
`compact_test::autocompact_triggers_llm_summary` 需要真实 LLM，受限网络下被代理拦截
（`api.openai.com` TunnelUnsuccessful），与代码无关。跑 `--lib` 或按名过滤可避开；
要真跑就把它指向本地端点。

---

## 7. 相关文档

| 文档 | 内容 |
|---|---|
| `agentrs/docs/getting-started.md` | 安装、配置、键位、命令表 |
| `agentrs/docs/tools.md` | 逐个工具的参数与行为 |
| `agentrs/docs/advanced.md` | 子智能体、Team、记忆、压缩、Hook、日志、Plan 模式 |
| `agentrs/docs/providers.md` | 各 provider 与 ProviderCompat |
| `agentrs/docs/mcp.md` | MCP 客户端与动态注入 |
| `agentrs/docs/troubleshooting.md` | 含 `NO_PROXY` 那一节 |

//! `Bash`：在 L0 围栏内跑一条 shell 命令。
//!
//! 围栏本身（进程组、rlimit、cwd、env 清洗、断网、超时强杀）由
//! [`super::Shell`] 的实现方兑现；这里管的是**声明、判定与措辞**——
//! 也就是模型读得到的全部东西。

use std::time::Duration;

use agentrs_contracts::sandbox::{ExecutionOutcome, RejectReason};
use agentrs_types::ToolDef;
use serde_json::{json, Value};

use super::text;
use super::{arg, shape_guard, ToolCtx, ToolOutput};

/// 单条命令的默认超时。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// 单条命令的超时上限。再长的活该拆成几步，或者根本不该在 Agent 回合里跑。
pub const MAX_TIMEOUT_MS: u64 = 600_000;
/// 合并输出的字节上限。
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// 模型侧声明。
pub fn def() -> ToolDef {
    ToolDef::mutating(
        "Bash",
        "在工作区里执行一条 shell 命令，需人工批准。命令在基础围栏内运行：\
         独立进程组、CPU 与文件大小上限、工作区为当前目录、环境变量已清洗、\
         **默认无网络**、超时强杀。读文件请用 Read、改文件请用 Edit/Write——\
         那两类的改动会先进 ChangeSet 等人过目，命令的副作用则直接落盘。",
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的 shell 命令，原样交给 sh -c",
                    "minLength": 1
                },
                "description": {
                    "type": "string",
                    "description": "一句话说明这条命令要做什么，会显示在审批面板上供人判断"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "超时毫秒数，默认 30000，上限 600000；超时会强杀整个进程组",
                    "minimum": 1,
                    "maximum": 600000
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
    )
}

/// 把 `timeout_ms` 参数收进合法区间。
pub fn timeout_of(raw: Option<u64>) -> Duration {
    Duration::from_millis(raw.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1, MAX_TIMEOUT_MS))
}

/// 命令输出走**无损**那一级的清洗。
///
/// 不是 `Full`：折叠相似行会改变内容，而模型经常要拿命令输出的原文去做下一步
/// （`grep` 的结果直接喂给 `Edit` 的 `old`）。`Safe` 去掉的只是终端曾经怎么把它
/// 画出来——ANSI 转义、被回车反复重画的进度条。`cargo build` 的一条进度行重画
/// 几百次，不清洗就是几百行几乎相同的内容进上下文。
const 清洗强度: agentrs_context::CompactLevel = agentrs_context::CompactLevel::Safe;

/// 把 stdout 与 stderr 合成一段可回灌模型的文本。
///
/// `ExecutionResult::output` 只有一个字段，所以两路必须合流。合流就得**标注**：
/// 不标的话，一条把诊断写到 stderr 的命令看起来像是把它写进了正常输出，
/// 模型会把警告当结果解析。
pub fn merge_streams(stdout: &[u8], stderr: &[u8]) -> String {
    let 收窄 = "请让命令自己收窄输出（head/grep/wc）";
    let 洗 = |raw: &[u8]| {
        agentrs_context::compact_output(&String::from_utf8_lossy(raw), 清洗强度)
    };
    let mut out = String::new();
    // 先清洗再截断：不然截断的额度会被转义字节和重画过的旧帧占掉，
    // 真正的输出反而被截在外面。
    let o = text::truncate_bytes(&洗(stdout), MAX_OUTPUT_BYTES, 收窄);
    let e = text::truncate_bytes(&洗(stderr), MAX_OUTPUT_BYTES, 收窄);
    if !o.trim().is_empty() {
        out.push_str(o.trim_end());
    }
    if !e.trim().is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("--- stderr ---\n");
        out.push_str(e.trim_end());
    }
    out
}

/// 执行。
pub async fn run(cx: &ToolCtx<'_>, args: &Value) -> ToolOutput {
    let Some(command) = arg(args, "command") else {
        return ToolOutput::bad_args("command");
    };
    let Some(shell) = cx.shell else {
        return ToolOutput::rejected(
            RejectReason::IsolationUnavailable,
            "本宿主没有提供命令执行能力",
        );
    };

    // H2：六条围栏但凡缺一条就拒绝，**不静默降级**。对外宣称"已隔离"却没有，
    // 是最危险的一类失败。拒绝理由要说清楚缺哪几条，否则排查的人不知道是内核
    // 关了用户命名空间，还是压根没装 unshare。
    if let Err(missing) = shell.containment().await {
        return ToolOutput::rejected(
            RejectReason::IsolationUnavailable,
            format!(
                "本机无法兑现 L0 基础围栏，缺：{}。命令执行在这台机器上不可用",
                missing.join("、")
            ),
        );
    }

    let timeout = timeout_of(args.get("timeout_ms").and_then(Value::as_u64));
    match shell.run(&command, timeout).await {
        super::ShellOutcome::Spawn(why) => ToolOutput {
            outcome: ExecutionOutcome::Completed { exit_code: 127 },
            text: Some(format!("命令没能启动：{why}")),
        },
        super::ShellOutcome::Settled {
            timed_out: true,
            stdout,
            stderr,
            ..
        } => {
            let body = merge_streams(&stdout, &stderr);
            // 不重复说"超时"——`ExecutionOutcome::TimedOut` 已经说了，管线会在
            // 末尾补上稳定码。这里补的是稳定码说不出的那半句：**上限是多少**。
            // 模型据此才知道该调大 timeout_ms 还是换个做法。
            let 秒 = timeout.as_secs_f32();
            ToolOutput {
                outcome: ExecutionOutcome::TimedOut,
                text: Some(if body.is_empty() {
                    format!("命令在 {秒} 秒内没有结束，进程组已被强杀")
                } else {
                    format!("{body}\n-- 上限 {秒} 秒，进程组已被强杀")
                }),
            }
        }
        super::ShellOutcome::Settled {
            exit_code,
            stdout,
            stderr,
            ..
        } => {
            let body = merge_streams(&stdout, &stderr);
            ToolOutput {
                outcome: ExecutionOutcome::Completed { exit_code },
                // 空字符串会被模型读成"这个工具坏了"。明说一句"无输出"，
                // 它才知道命令跑成功了、只是没打印东西。
                text: Some(if body.is_empty() {
                    "（无输出）".to_string()
                } else {
                    body
                }),
            }
        }
    }
}

/// 命令形状判定的入口，供宿主的 `ToolGuard` 调用。
///
/// 判定本身在 [`shape_guard`]；这一层只负责"这是不是一次 Bash 调用"。
pub fn guard_verdict(tool_name: &str, args: &Value) -> Option<String> {
    if tool_name != "Bash" {
        return None;
    }
    // 参数不对归 schema 校验管，不归这里。
    shape_guard::judge(args.get("command")?.as_str()?)
}

#[cfg(test)]
mod tests {
    use super::super::fake::{FakeShell, FakeWorkspace};
    use super::*;
    use super::super::ShellOutcome;

    async fn 跑(shell: &FakeShell, args: Value) -> ToolOutput {
        let files = FakeWorkspace::new([]);
        let cx = ToolCtx {
            files: &files,
            shell: Some(shell),
            http: None,
            search: None,
        };
        run(&cx, &args).await
    }

    #[tokio::test]
    async fn 跑得通并把退出码带回来() {
        let out = 跑(&FakeShell::ok("hello"), json!({"command": "echo hello"})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 0 });
        assert_eq!(out.text.unwrap(), "hello");
    }

    #[tokio::test]
    async fn 失败时_stderr_也带回来() {
        let shell = FakeShell::ok("").returning(ShellOutcome::Settled {
            timed_out: false,
            exit_code: 7,
            stdout: b"out".to_vec(),
            stderr: "出错了".as_bytes().to_vec(),
        });
        let out = 跑(&shell, json!({"command": "x"})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 7 });
        let text = out.text.unwrap();
        // 两路都在，而且分得清谁是谁——不标注的话模型会把诊断当结果解析。
        assert!(text.contains("out"), "{text}");
        assert!(text.contains("--- stderr ---\n出错了"), "{text}");
    }

    #[tokio::test]
    async fn 无输出与出错是两回事() {
        // 空字符串会被模型读成"这个工具坏了"。
        let out = 跑(&FakeShell::ok(""), json!({"command": "true"})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 0 });
        assert_eq!(out.text.unwrap(), "（无输出）");
    }

    #[tokio::test]
    async fn 超时保留被杀之前那部分输出() {
        // 一条打印了很多然后卡住的构建，那些输出恰恰是"卡在哪一步"的唯一线索。
        let shell = FakeShell::ok("").returning(ShellOutcome::Settled {
            timed_out: true,
            exit_code: -1,
            stdout: "第一步完成".as_bytes().to_vec(),
            stderr: Vec::new(),
        });
        let out = 跑(&shell, json!({"command": "x", "timeout_ms": 2000})).await;
        assert_eq!(out.outcome, ExecutionOutcome::TimedOut);
        let text = out.text.unwrap();
        assert!(text.contains("第一步完成"), "{text}");
        // 说的是**上限是多少**，不是"超时了"——后者 TimedOut 这个结局本身已经
        // 说了，管线还会在末尾补一次稳定码。
        assert!(text.contains("上限 2 秒"), "{text}");
        assert!(!text.contains("超时"), "别把稳定码已经说过的话再说一遍：{text}");
    }

    #[tokio::test]
    async fn 围栏缺一条就拒绝而且说清楚缺哪条() {
        // H2。文件工具只用得到 cwd 一条，报 L0 尚可；能跑任意命令之后，
        // 同样的报告就是静默降级。
        let out = 跑(&FakeShell::missing(&["默认断网"]), json!({"command": "echo x"})).await;
        assert_eq!(
            out.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::IsolationUnavailable
            }
        );
        assert!(out.text.unwrap().contains("默认断网"));
    }

    #[tokio::test]
    async fn 宿主没给_shell_时明说而不是假装跑了() {
        let files = FakeWorkspace::new([]);
        let out = run(&ToolCtx::files_only(&files), &json!({"command": "x"})).await;
        assert_eq!(
            out.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::IsolationUnavailable
            }
        );
    }

    #[tokio::test]
    async fn 命令原样交给_shell() {
        // 不做词法分析、不改写——改写过的命令与人在审批面板上看到的就不是一条了。
        let shell = FakeShell::ok("");
        跑(&shell, json!({"command": "a && b | c > d"})).await;
        assert_eq!(shell.seen.lock().unwrap().as_slice(), ["a && b | c > d"]);
    }

    #[test]
    fn 超时参数被夹进合法区间() {
        assert_eq!(timeout_of(None), Duration::from_millis(DEFAULT_TIMEOUT_MS));
        assert_eq!(timeout_of(Some(0)), Duration::from_millis(1));
        assert_eq!(timeout_of(Some(u64::MAX)), Duration::from_millis(MAX_TIMEOUT_MS));
    }

    #[test]
    fn 两路输出合流时标注得清清楚楚() {
        assert_eq!(merge_streams(b"a", b""), "a");
        assert_eq!(merge_streams(b"", b"e"), "--- stderr ---\ne");
        assert_eq!(merge_streams(b"a\n", b"e\n"), "a\n--- stderr ---\ne");
        assert_eq!(merge_streams(b"", b""), "");
    }

    #[test]
    fn 命令输出的终端噪声被清掉() {
        // `TERM=dumb` 挡不住全部：cargo 之类仍会用回车重画进度行。
        // 不清洗的话，一次构建的几百帧进度会原样占满上下文。
        let 带噪声 = "\u{1b}[32m 10%\u{1b}[0m\r\u{1b}[32m 60%\u{1b}[0m\r\u{1b}[32m100%\u{1b}[0m\nDone";
        assert_eq!(merge_streams(带噪声.as_bytes(), b""), "100%\nDone");
    }

    #[test]
    fn 清洗是无损的_命令输出原文能直接拿去用() {
        // 模型经常把 grep 的结果直接喂给 Edit 的 old。清洗改了一个字符，
        // 那次 Edit 就会报"未找到待替换的文本"，而原因看不出来。
        let 原文 = "src/main.rs:12:    let x = compute(a, b);";
        assert_eq!(merge_streams(原文.as_bytes(), b""), 原文);
    }

    #[test]
    fn 超长输出保留真实前缀并说明截了多少() {
        let 长 = "x".repeat(MAX_OUTPUT_BYTES + 100);
        let out = merge_streams(长.as_bytes(), b"");
        assert!(out.starts_with("xxxx"));
        assert!(out.contains("已显示前"), "{}", &out[out.len() - 90..]);
        assert!(out.contains("head/grep/wc"), "要给出路");
    }

    #[test]
    fn guard_只对_bash_表态() {
        // guard 是全局装上去的，它必须对自己不懂的工具闭嘴。
        assert!(guard_verdict("Write", &json!({"content": "sudo rm -rf /"})).is_none());
        assert!(guard_verdict("Bash", &json!({"command": "ls"})).is_none());
        assert!(guard_verdict("Bash", &json!({"command": "sudo ls"})).is_some());
        // 参数畸形归 schema 校验管。
        assert!(guard_verdict("Bash", &json!({})).is_none());
    }
}

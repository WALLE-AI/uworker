//! 命令执行的 L0 基础围栏。
//!
//! **只有这里能派生进程。** 内核库一行也不行——`clippy.toml` 从类型层面禁掉
//! `std::process::Command`，`scripts/check-no-env.sh` 兜住它覆盖不到的写法，
//! 两道门禁都只对本 crate 开口。
//!
//! ## 六条，缺一不可
//!
//! 契约把 [`IsolationLevel::L0BasicContainment`] 定义为六条：
//! **进程组、rlimit、cwd 限制、env 清洗、默认断网、超时强杀**
//! （见 `agentrs_contracts::sandbox`）。文件工具只用得到 cwd 一条，报 L0 尚可；
//! 一旦能跑任意命令，同样的报告就成了 H2 明令禁止的静默降级——对外宣称"已隔离"
//! 却没有，是最危险的一类失败。
//!
//! 所以本模块**先探测再执行**：六条里但凡有一条在本机拿不到，
//! [`Containment::probe`] 返回缺失清单，调用方据此回
//! [`RejectReason::IsolationUnavailable`]。于是 macOS、Windows、以及关掉了无特权
//! 用户命名空间的 Linux 上，Bash 直接不可用——这正是要的行为。
//!
//! ## 为什么是 `unshare` + `ulimit` 而不是 `pre_exec`
//!
//! [`std::os::unix::process::CommandExt::pre_exec`] 是 `unsafe`，而 workspace
//! 声明了 `unsafe_code = "forbid"`。shell 内建的 `ulimit` 与 util-linux 的
//! `unshare` 是等价物：不引依赖、不写一行 unsafe，代价是多两层进程。
//!
//! 真实隔离（landlock / seccomp / seatbelt / Job Object）归 SandboxRS，不在这里。

// 本文件是宿主参考实现里**唯一**派生进程的地方，门禁对它单独开口。
// 收在一个模块里，crate 其余部分越界仍会被照出来。
#![allow(clippy::disallowed_types, reason = "L0 执行器按设计派生进程（架构 §3.1）")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use agentrs_tools::builtin::{Shell, ShellOutcome};
use async_trait::async_trait;

/// 收到 TERM 之后留给进程组善后的时间，之后补 KILL。
const 宽限: Duration = Duration::from_millis(300);

/// CPU 秒数上限（`ulimit -t`）。
const RLIMIT_CPU_SECONDS: u64 = 60;
/// 单文件写入上限，512 字节块（`ulimit -f`），约 512 MiB。
const RLIMIT_FILE_BLOCKS: u64 = 1_048_576;

/// 本机实际拿得到的围栏能力。
///
/// 构造它的唯一途径是 [`Self::probe`]——**探测过才存在**，
/// 于是"没探测就执行"在类型上不可能发生。
#[derive(Debug, Clone, Copy)]
pub struct Containment {
    /// 私有性字段：只为堵住外部 `Containment {}` 构造。
    _probed: (),
}

/// 六条围栏各自的名字，进入缺失清单时给人看。
const 六条: [&str; 6] = [
    "进程组",
    "rlimit",
    "cwd 限制",
    "env 清洗",
    "默认断网",
    "超时强杀",
];

impl Containment {
    /// 探测本机能否兑现全部六条。
    ///
    /// 返回 `Err` 时携带**缺哪几条**，而不是一句"不支持"——排查的人需要知道
    /// 是内核关了用户命名空间，还是压根没装 `unshare`。
    ///
    /// 真跑一条 `unshare -rn sh -c 'ulimit …; true'`：断网与 rlimit 这两条只有
    /// 试过才知道，静态判断平台会在容器里判错。
    pub async fn probe() -> Result<Self, Vec<&'static str>> {
        if !cfg!(unix) {
            // 进程组与信号语义是 Unix 的；非 Unix 上六条一条也兑现不了。
            return Err(六条.to_vec());
        }
        let script = format!(
            "ulimit -t {RLIMIT_CPU_SECONDS} 2>/dev/null && \
             ulimit -f {RLIMIT_FILE_BLOCKS} 2>/dev/null"
        );
        let 断网可用 = 试跑(&["unshare", "-r", "-n", "--", "/bin/sh", "-c", "true"]).await;
        let rlimit可用 = 试跑(&["/bin/sh", "-c", &script]).await;

        let mut missing = Vec::new();
        if !断网可用 {
            missing.push("默认断网");
        }
        if !rlimit可用 {
            missing.push("rlimit");
        }
        if missing.is_empty() {
            Ok(Self { _probed: () })
        } else {
            Err(missing)
        }
    }

    /// 在围栏内跑一条 shell 命令。
    ///
    /// `command` 原样交给 `sh -c`，**不做词法分析**——分析不全的解析器只会给人
    /// 虚假的安全感。真正的判定在 [`super::exec_guard`]（拦几种确定的形状）
    /// 与人工审批（其余全部）。
    pub async fn run(&self, cwd: &Path, command: &str, timeout: Duration) -> ShellOutcome {
        // 用户命令走 `$1` 传参，不拼进脚本文本——拼字符串就等于给自己开一个
        // 注入口，而这里本就不需要拼。
        let wrapper = format!(
            "ulimit -t {RLIMIT_CPU_SECONDS} 2>/dev/null; \
             ulimit -f {RLIMIT_FILE_BLOCKS} 2>/dev/null; \
             exec /bin/sh -c \"$1\" sh"
        );
        let mut cmd = tokio::process::Command::new("unshare");
        cmd.args(["-r", "-n", "--", "/bin/sh", "-c", &wrapper, "sh", command]);

        // ---- cwd 限制 ----
        cmd.current_dir(cwd);

        // ---- env 清洗 ----
        // 全清再补最小一套，而不是"删掉几个敏感的"：黑名单永远漏，
        // 而漏掉的那个恰好可能是 API key。值也全部写死，不从本进程继承——
        // 继承就等于把宿主的环境泄进模型能读到的地方。
        cmd.env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", cwd)
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("TZ", "UTC")
            // dumb 让工具不吐 ANSI 转义——那些字节进了模型上下文只是噪声。
            .env("TERM", "dumb");

        // ---- 进程组 ----
        // 自成一组，超时才杀得干净：子进程再派生的孙子进程也在这一组里。
        #[cfg(unix)]
        cmd.process_group(0);

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // 本任务被 drop（Run 取消）时不留孤儿。
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            // 进程压根没起来（`unshare` 不在 PATH、cwd 不存在等）。与"跑了但
            // 失败了"分开：前者没有任何副作用，后者可能已经改了盘，
            // reconcile 要靠这个区分回答 `NotStarted` 还是 `Unknown`。
            Err(e) => return ShellOutcome::Spawn(e.kind().to_string()),
        };
        let pid = child.id();
        // 先接管管道再等待：不接管的话，输出填满管道缓冲区就会把子进程堵死，
        // 于是一条打印很多的命令会"超时"，而它其实早就该结束了。
        let 收 = |pipe: Option<tokio::process::ChildStdout>| async move {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                use tokio::io::AsyncReadExt;
                let _ = p.read_to_end(&mut buf).await;
            }
            buf
        };
        let 收错 = |pipe: Option<tokio::process::ChildStderr>| async move {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                use tokio::io::AsyncReadExt;
                let _ = p.read_to_end(&mut buf).await;
            }
            buf
        };
        let out_task = tokio::spawn(收(child.stdout.take()));
        let err_task = tokio::spawn(收错(child.stderr.take()));

        // ---- 超时强杀 ----
        let mut timed_out = false;
        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => status.ok(),
            Err(_) => {
                timed_out = true;
                if let Some(pid) = pid {
                    kill_group(pid, "TERM").await;
                    tokio::time::sleep(宽限).await;
                    kill_group(pid, "KILL").await;
                }
                child.wait().await.ok()
            }
        };

        // 两路**分开**交回。合流与截断的措辞是工具的事
        // （`agentrs_tools::builtin::bash`），不是围栏的事——围栏只该报告
        // 事实：进程说了什么、退出码是几、有没有被杀。
        ShellOutcome::Settled {
            timed_out,
            exit_code: status.and_then(|s| s.code()).unwrap_or(-1),
            stdout: out_task.await.unwrap_or_default(),
            stderr: err_task.await.unwrap_or_default(),
        }
    }
}

/// 杀掉一整个进程组。
///
/// 传 `-pid` 而不是 `pid`：只杀直接子进程的话，`sh -c 'foo & bar'` 派生出来的那些
/// 会活下来，成为看不见的孤儿——超时强杀这条就没兑现。
async fn kill_group(pid: u32, signal: &str) {
    let _ = tokio::process::Command::new("kill")
        .args([&format!("-{signal}"), "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// [`Shell`] 端口的本地实现：一个工作区根 + 一次性缓存的围栏探测。
pub struct LocalShell {
    cwd: PathBuf,
    /// 围栏能力，**首次用到时探测一次**并缓存。
    ///
    /// 探测要真跑两条命令，每次执行都探一遍太贵；而在构造函数里探又会把
    /// `new()` 变成 async，波及每一个宿主与测试。
    probed: tokio::sync::OnceCell<Result<Containment, Vec<&'static str>>>,
}

impl LocalShell {
    /// 以 `cwd` 为命令的工作目录。
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            probed: tokio::sync::OnceCell::new(),
        }
    }

    async fn containment_of(&self) -> Result<Containment, Vec<&'static str>> {
        self.probed.get_or_init(Containment::probe).await.clone()
    }
}

#[async_trait]
impl Shell for LocalShell {
    async fn containment(&self) -> Result<(), Vec<String>> {
        self.containment_of()
            .await
            .map(|_| ())
            .map_err(|missing| missing.into_iter().map(str::to_string).collect())
    }

    async fn run(&self, command: &str, timeout: Duration) -> ShellOutcome {
        match self.containment_of().await {
            // 调用方（`builtin::bash`）恒先问 `containment()`，走到这里说明围栏
            // 是齐的。真到了这一步还缺，宁可报"没启动"也不能裸跑一条命令。
            Err(missing) => ShellOutcome::Spawn(format!("围栏不可用，缺：{}", missing.join("、"))),
            Ok(c) => c.run(&self.cwd, command, timeout).await,
        }
    }
}

/// 跑一条探测命令，只关心它成不成。
async fn 试跑(argv: &[&str]) -> bool {
    let Some((program, args)) = argv.split_first() else {
        return false;
    };
    matches!(
        tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await,
        Ok(status) if status.success()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 探测不到围栏的机器上，这些测试无从验证围栏——跳过而不是假装通过。
    macro_rules! 需要围栏 {
        () => {
            match Containment::probe().await {
                Ok(c) => c,
                Err(missing) => {
                    eprintln!("跳过：本机缺 {missing:?}");
                    return;
                }
            }
        };
    }

    fn 临时目录() -> tempdir::TempDir {
        tempdir::TempDir::new("agentrs-exec").unwrap()
    }

    /// 取合并后的文本，只为让断言好写。真正的合流措辞归
    /// `agentrs_tools::builtin::bash`，那边另有测试。
    fn 文本(outcome: &ShellOutcome) -> String {
        match outcome {
            ShellOutcome::Settled { stdout, stderr, .. } => {
                agentrs_tools::builtin::bash::merge_streams(stdout, stderr)
            }
            ShellOutcome::Spawn(why) => panic!("没启动：{why}"),
        }
    }

    #[tokio::test]
    async fn 探测失败时说清楚缺哪几条() {
        // 不断言探测成功——CI 上可能真的没有用户命名空间。断言的是
        // "失败时给的是清单而不是一句不支持"，那才是排查时唯一有用的东西。
        if let Err(missing) = Containment::probe().await {
            assert!(!missing.is_empty());
            for name in &missing {
                assert!(六条.contains(name), "{name} 不是六条之一");
            }
        }
    }

    #[tokio::test]
    async fn 跑得通一条普通命令() {
        let c = 需要围栏!();
        let d = 临时目录();
        let out = c.run(d.path(), "echo hello", Duration::from_secs(10)).await;
        let ShellOutcome::Settled { exit_code, timed_out, .. } = &out else {
            panic!("应当 settled");
        };
        assert_eq!(*exit_code, 0);
        assert!(!timed_out);
        assert_eq!(文本(&out), "hello");
    }

    #[tokio::test]
    async fn 退出码与两路输出都如实带回() {
        let c = 需要围栏!();
        let d = 临时目录();
        let out = c
            .run(d.path(), "echo out; echo err >&2; exit 3", Duration::from_secs(10))
            .await;
        let ShellOutcome::Settled { exit_code, stdout, stderr, .. } = &out else {
            panic!("应当 settled");
        };
        assert_eq!(*exit_code, 3);
        // 两路**分开**交回：合流与措辞是工具的事，围栏只报告事实。
        assert_eq!(String::from_utf8_lossy(stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(stderr).trim(), "err");
    }

    #[tokio::test]
    async fn cwd_就是工作区根() {
        let c = 需要围栏!();
        let d = 临时目录();
        std::fs::write(d.path().join("marker.txt"), "x").unwrap();
        let out = c.run(d.path(), "ls", Duration::from_secs(10)).await;
        assert!(文本(&out).contains("marker.txt"), "{}", 文本(&out));
    }

    #[tokio::test]
    async fn env_被清洗掉了() {
        let c = 需要围栏!();
        let d = 临时目录();
        let out = c.run(d.path(), "env", Duration::from_secs(10)).await;
        let text = 文本(&out);
        // 只断言最小一套在、其余不在：断言某个具体变量不在，等于假设测试进程
        // 里有它，那是环境依赖。真实场景里被清掉的那个叫 ANTHROPIC_API_KEY。
        assert!(text.contains("LANG=C.UTF-8"), "{text}");
        assert!(text.contains("TERM=dumb"), "{text}");
        let 变量数 = text.lines().filter(|l| l.contains('=')).count();
        assert!(变量数 <= 8, "env 没清干净，剩 {变量数} 个：{text}");
    }

    #[tokio::test]
    async fn 默认断网() {
        let c = 需要围栏!();
        let d = 临时目录();
        // 命名空间里只有 lo，而且 lo 是 down 的：连本机端口也不通。
        // 这条比"连外网不通"更强，也更好测——不依赖 CI 有没有出网。
        let out = c
            .run(
                d.path(),
                "/bin/sh -c 'exec 3<>/dev/tcp/127.0.0.1/22' 2>/dev/null",
                Duration::from_secs(10),
            )
            .await;
        let ShellOutcome::Settled { exit_code, .. } = out else {
            panic!("应当 settled");
        };
        assert_ne!(exit_code, 0, "命名空间里不该连得上任何东西");
    }

    #[tokio::test]
    async fn 超时把整个进程组都杀掉() {
        let c = 需要围栏!();
        let d = 临时目录();
        // 派生一个后台孙子进程再自己睡着。只杀直接子进程的实现会漏掉那个孙子，
        // 它会成为看不见的孤儿——"超时强杀"这一条就没兑现。
        //
        // 外层再套一个 10 秒的表：强杀没生效的话 `run` 会一直等那个 60 秒的
        // `sleep`，外层超时就会把这条测试判失败，而不是让它慢吞吞地"通过"。
        let 结果 = tokio::time::timeout(
            Duration::from_secs(10),
            c.run(d.path(), "sleep 60 & sleep 60", Duration::from_millis(300)),
        )
        .await
        .expect("超时强杀没生效：run 一直等到了外层的表");
        let ShellOutcome::Settled { timed_out, .. } = 结果 else {
            panic!("应当 settled");
        };
        assert!(timed_out);
    }

    #[tokio::test]
    async fn 端口实现对围栏的说法与探测一致() {
        // `LocalShell` 是 `builtin::bash` 唯一看得到的东西。它报"齐了"而实际
        // 没齐，就是 H2 禁止的静默降级——对外宣称"已隔离"却没有。
        let d = 临时目录();
        let shell = LocalShell::new(d.path());
        match (Containment::probe().await, shell.containment().await) {
            (Ok(_), got) => assert!(got.is_ok(), "探测说齐了，端口却说缺"),
            (Err(want), Err(got)) => assert_eq!(got.len(), want.len()),
            (Err(_), Ok(())) => panic!("探测说缺，端口却说齐了"),
        }
    }
}

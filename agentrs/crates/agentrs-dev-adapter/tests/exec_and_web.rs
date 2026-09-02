//! `Bash` 与 `WebFetch` 走完整 grant 链路的语义。
//!
//! 与 `tool_semantics.rs` 同一形制：每条测试问的都是"模型读到这段输出之后，
//! 下一步会做对还是做错"，而不是"函数返回值等于某个常量"。
//!
//! 这里**不连外网**。取网页那几条走的全是拒绝路径——它们恰好也是这个工具
//! 最要紧的部分。

use std::sync::Arc;

use agentrs_contracts::ids::{Digest, Timestamp};
use agentrs_contracts::policy::{InputHash, SandboxGrant};
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{
    ExecutionOutcome, ExecutionRequest, ExecutionStatus, IsolationLevel,
};
use agentrs_dev_adapter::LocalDevSandbox;

struct 环境 {
    sb: Arc<LocalDevSandbox>,
    _dir: tempdir::TempDir,
    n: std::cell::Cell<u32>,
}

fn 环境() -> 环境 {
    let dir = tempdir::TempDir::new("agentrs-exec-web").unwrap();
    let sb = Arc::new(LocalDevSandbox::with_search(dir.path(), None).unwrap());
    环境 {
        sb,
        _dir: dir,
        n: std::cell::Cell::new(0),
    }
}

impl 环境 {
    /// 跑一次工具调用，自带一枚合法的一次性 grant。
    async fn 调用(&self, tool: &str, args: serde_json::Value) -> (ExecutionOutcome, String) {
        let n = self.n.get() + 1;
        self.n.set(n);
        let id = format!("g{n}");
        let hash = InputHash(Digest::from_hex(format!("h{n}")));
        self.sb.issue_grant(&id, hash.clone(), Timestamp(i64::MAX));
        let r = self
            .sb
            .execute(
                SandboxGrant {
                    grant_id: id,
                    payload: serde_json::json!({}),
                },
                ExecutionRequest {
                    execution_id: format!("e{n}").into(),
                    tool_name: tool.into(),
                    arguments: args,
                    change_set_id: "cs1".into(),
                    input_hash: hash,
                    required_isolation: IsolationLevel::L0BasicContainment,
                },
            )
            .await
            .unwrap();
        (r.outcome, r.output.unwrap_or_default())
    }

    /// 本机兑现不了六条围栏时跳过——假装通过比失败更糟。
    async fn 有围栏(&self) -> bool {
        let (outcome, _) = self.调用("Bash", serde_json::json!({"command": "true"})).await;
        !matches!(
            outcome,
            ExecutionOutcome::Rejected {
                reason: agentrs_contracts::sandbox::RejectReason::IsolationUnavailable
            }
        )
    }
}

#[tokio::test]
async fn bash_跑得通并把退出码带回来() {
    let e = 环境();
    if !e.有围栏().await {
        return;
    }
    let (outcome, text) = e
        .调用("Bash", serde_json::json!({"command": "echo hello"}))
        .await;
    assert_eq!(outcome, ExecutionOutcome::Completed { exit_code: 0 });
    assert_eq!(text, "hello");

    // 失败要能被模型看出来是失败，而且看得到 stderr 里那句话。
    let (outcome, text) = e
        .调用(
            "Bash",
            serde_json::json!({"command": "echo 出错了 >&2; exit 7"}),
        )
        .await;
    assert_eq!(outcome, ExecutionOutcome::Completed { exit_code: 7 });
    assert!(text.contains("出错了"), "{text}");
}

#[tokio::test]
async fn bash_的无输出与出错是两回事() {
    let e = 环境();
    if !e.有围栏().await {
        return;
    }
    // 空字符串会被模型读成"这个工具坏了"。明说一句"无输出"，
    // 它才知道命令跑成功了、只是没打印东西。
    let (outcome, text) = e.调用("Bash", serde_json::json!({"command": "true"})).await;
    assert_eq!(outcome, ExecutionOutcome::Completed { exit_code: 0 });
    assert_eq!(text, "（无输出）");
}

#[tokio::test]
async fn bash_看得到工作区里的文件() {
    let e = 环境();
    if !e.有围栏().await {
        return;
    }
    std::fs::write(e.sb.root().join("hello.txt"), "内容").unwrap();
    let (_, text) = e.调用("Bash", serde_json::json!({"command": "ls"})).await;
    assert!(text.contains("hello.txt"), "{text}");
}

#[tokio::test]
async fn bash_看不到_change_set_里尚未提交的改动() {
    let e = 环境();
    if !e.有围栏().await {
        return;
    }
    // 这条记录的是一个**真实的、当前无解的**边界，不是一个愿望。
    // 文件工具的写只进 overlay，命令看到的却是磁盘。模型如果先 Write 再用
    // `cat` 去看，会看到旧内容——所以工具描述里写着"读文件请用 Read"。
    e.调用(
        "Write",
        serde_json::json!({"path": "staged.txt", "content": "暂存的内容"}),
    )
    .await;
    let (_, text) = e
        .调用("Bash", serde_json::json!({"command": "cat staged.txt 2>&1"}))
        .await;
    assert!(!text.contains("暂存的内容"), "如果这条挂了，说明 overlay 已经能被命令看到，是好事：{text}");
}

#[tokio::test]
async fn bash_超时被强杀且说得出是超时() {
    let e = 环境();
    if !e.有围栏().await {
        return;
    }
    let (outcome, text) = e
        .调用(
            "Bash",
            serde_json::json!({"command": "sleep 30", "timeout_ms": 300}),
        )
        .await;
    // TimedOut 与 Completed 分开：模型据此知道"没跑完"而不是"跑完了没输出"。
    assert_eq!(outcome, ExecutionOutcome::TimedOut);
    // 说的是**上限是多少**，不是"超时了"——后者 `TimedOut` 这个结局本身已经
    // 说了，管线还会在末尾补一次稳定码。模型需要的是那个数字，它据此决定
    // 该调大 timeout_ms 还是换个做法。
    assert!(text.contains("0.3"), "{text}");
    assert!(!text.contains("超时"), "别把稳定码已经说过的话再说一遍：{text}");
}

#[tokio::test]
async fn bash_默认没有网络() {
    let e = 环境();
    if !e.有围栏().await {
        return;
    }
    // 六条里最容易被悄悄漏掉的一条：漏了也没人会发现，直到某天一条命令
    // 把工作区内容发了出去。
    let (outcome, _) = e
        .调用(
            "Bash",
            serde_json::json!({
                "command": "exec 3<>/dev/tcp/127.0.0.1/22",
                "timeout_ms": 5000
            }),
        )
        .await;
    assert_ne!(outcome, ExecutionOutcome::Completed { exit_code: 0 });
}

#[tokio::test]
async fn bash_跑完之后_reconcile_答得出结果() {
    let e = 环境();
    if !e.有围栏().await {
        return;
    }
    e.调用("Bash", serde_json::json!({"command": "echo x"})).await;
    // e1 是探围栏那次，e2 是这次。
    assert!(matches!(
        e.sb.reconcile("e2".into()).await.unwrap(),
        ExecutionStatus::Finished(_)
    ));
}

#[tokio::test]
async fn webfetch_不去取本机与内网地址() {
    let e = 环境();
    // SSRF。参数由模型填，而它读过的每一篇网页都可能在教它取这些地址。
    for url in [
        "http://127.0.0.1:19121/starvlm/v1/models",
        "http://localhost:8080/admin",
        "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
        "http://10.0.0.1/",
        "http://[::1]/",
    ] {
        let (outcome, text) = e.调用("WebFetch", serde_json::json!({"url": url})).await;
        assert_eq!(
            outcome,
            ExecutionOutcome::Completed { exit_code: 1 },
            "{url} 没被拦住"
        );
        assert!(
            text.contains("本机或内网") || text.contains("解析不出来"),
            "{url}: {text}"
        );
    }
}

#[tokio::test]
async fn webfetch_只认_http_与_https() {
    let e = 环境();
    // `file://` 能把工作区之外的任何文件读进模型上下文，绕开整条 cwd 围栏。
    for url in ["file:///etc/passwd", "ftp://example.com/x", "data:text/html,x"] {
        let (outcome, text) = e.调用("WebFetch", serde_json::json!({"url": url})).await;
        assert_eq!(outcome, ExecutionOutcome::Completed { exit_code: 1 }, "{url}");
        assert!(text.contains("只支持 http") || text.contains("无法解析"), "{url}: {text}");
    }
}

#[tokio::test]
async fn webfetch_的拒绝是失败而不是执行器拒绝() {
    let e = 环境();
    // 这个区分不是形式主义。`Rejected` 是授权层的话，模型看到它会停下；
    // `Completed{1}` 带一句解释是工具自己的话，模型读得懂，会改用别的办法。
    // 一个模型选错了 URL，不该让整个 Run 看起来像撞上了权限墙。
    let (outcome, _) = e
        .调用("WebFetch", serde_json::json!({"url": "http://127.0.0.1/"}))
        .await;
    assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
}

#[tokio::test]
async fn 未配置搜索端点时_websearch_明说而不是静默失败() {
    let e = 环境();
    let (outcome, text) = e
        .调用("WebSearch", serde_json::json!({"query": "rust async"}))
        .await;
    assert_eq!(outcome, ExecutionOutcome::Completed { exit_code: 1 });
    assert!(text.contains("未配置"), "{text}");
    // 正常路径上模型根本看不到这个工具——目录里没有它。这条只是兜底。
    assert!(!agentrs_dev_adapter::env_tool_names().contains(&"WebSearch".to_string()));
}

#[tokio::test]
async fn 缺参数说的是参数问题() {
    let e = 环境();
    for (tool, arg) in [("Bash", "command"), ("WebFetch", "url"), ("WebSearch", "query")] {
        let (outcome, text) = e.调用(tool, serde_json::json!({})).await;
        assert_eq!(outcome, ExecutionOutcome::Completed { exit_code: 2 }, "{tool}");
        assert!(text.contains(arg), "{tool}: {text}");
    }
}

/// 真的去取一个公网页面。**默认不跑**——单测不许依赖网络（架构 §1.1 的边界判据）。
///
/// 手动跑：`cargo test -p agentrs-dev-adapter --test exec_and_web -- --ignored --nocapture`
#[tokio::test]
#[ignore = "需要出网"]
async fn webfetch_取得回一个真实页面() {
    let e = 环境();
    let (outcome, text) = e
        .调用("WebFetch", serde_json::json!({"url": "https://example.com/"}))
        .await;
    println!("outcome={outcome:?}\n{text}");
    assert_eq!(outcome, ExecutionOutcome::Completed { exit_code: 0 });
    assert!(text.contains("Example Domain"), "{text}");
    // 抽成了文本，不是把 HTML 原样倒给模型。
    assert!(!text.contains("<html"), "{text}");
    assert!(!text.contains("<p>"), "{text}");
}

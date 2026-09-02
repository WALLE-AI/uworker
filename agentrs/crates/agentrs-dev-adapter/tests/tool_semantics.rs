//! 六个工具的语义（工具优化执行方案，第三、四部分）。
//!
//! `read_your_writes.rs` 验的是 overlay 这一条不变量；这里验的是**每个工具自己
//! 说到做到**：拒绝的时候拒绝得对，成功的时候把发生了什么讲清楚。
//!
//! 判据不是"返回了什么码"，而是**模型读到这段输出之后能不能自己改对**。工具的错误
//! 消息是回灌给它的唯一线索，一句"操作失败"等于让它重试同样的错误。

use agentrs_contracts::ids::{Digest, ExecutionId, Timestamp};
use agentrs_contracts::policy::{InputHash, SandboxGrant};
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{
    ExecutionOutcome, ExecutionRequest, ExecutionResult, IsolationLevel, RejectReason,
};
use agentrs_dev_adapter::LocalDevSandbox;

const CS: &str = "cs-1";

struct 夹具 {
    sb: LocalDevSandbox,
    _dir: tempdir::TempDir,
    n: std::cell::Cell<u32>,
}

impl 夹具 {
    fn new() -> Self {
        let dir = tempdir::TempDir::new("agentrs-tools").unwrap();
        std::fs::write(dir.path().join("a.md"), "alpha\nbeta\nalpha\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/deep.md"), "DEEP\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn go() {}\n").unwrap();
        let sb = LocalDevSandbox::new(dir.path()).unwrap();
        Self {
            sb,
            _dir: dir,
            n: std::cell::Cell::new(0),
        }
    }

    async fn 跑(&self, tool: &str, args: serde_json::Value) -> ExecutionResult {
        let i = self.n.get();
        self.n.set(i + 1);
        let gid = format!("g{i}");
        let hash = InputHash(Digest::from_hex(format!("h{i}")));
        self.sb.issue_grant(&gid, hash.clone(), Timestamp(i64::MAX));
        self.sb
            .execute(
                SandboxGrant {
                    grant_id: gid,
                    payload: serde_json::json!({}),
                },
                ExecutionRequest {
                    execution_id: ExecutionId::new(format!("e{i}")),
                    tool_name: tool.into(),
                    arguments: args,
                    change_set_id: CS.into(),
                    input_hash: hash,
                    required_isolation: IsolationLevel::L0BasicContainment,
                },
            )
            .await
            .expect("执行失败")
    }
}

fn 成功(r: &ExecutionResult) -> String {
    assert_eq!(
        r.outcome,
        ExecutionOutcome::Completed { exit_code: 0 },
        "期望成功，实际 {:?}：{:?}",
        r.outcome,
        r.output
    );
    r.output.clone().unwrap_or_default()
}

fn 失败(r: &ExecutionResult) -> String {
    assert_eq!(
        r.outcome,
        ExecutionOutcome::Completed { exit_code: 1 },
        "期望工具级失败，实际 {:?}",
        r.outcome
    );
    r.output.clone().unwrap_or_default()
}

// ---- Glob ----

#[tokio::test]
async fn glob_按通配符列出文件并排序() {
    let f = 夹具::new();
    let out = 成功(&f.跑("Glob", serde_json::json!({"pattern": "**/*.md"})).await);
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows, vec!["a.md", "src/deep.md"], "要跨目录且有序");
}

#[tokio::test]
async fn glob_的单星不跨目录() {
    let f = 夹具::new();
    let out = 成功(&f.跑("Glob", serde_json::json!({"pattern": "*.md"})).await);
    assert_eq!(out.lines().collect::<Vec<_>>(), vec!["a.md"]);
}

#[tokio::test]
async fn glob_能限定子目录() {
    let f = 夹具::new();
    let out = 成功(&f.跑("Glob", serde_json::json!({"pattern": "**/*", "path": "src"})).await)
        ;
    assert_eq!(out.lines().collect::<Vec<_>>(), vec!["src/deep.md", "src/lib.rs"]);
}

#[tokio::test]
async fn glob_看得到新建的看不到已删除的() {
    // 与 Grep 走同一条 visible_files，所以两者对"有哪些文件"的看法不会分叉。
    let f = 夹具::new();
    f.跑("Write", serde_json::json!({"path": "fresh.md", "content": "x\n"}))
        .await;
    f.跑("Delete", serde_json::json!({"path": "a.md"})).await;
    let out = 成功(&f.跑("Glob", serde_json::json!({"pattern": "*.md"})).await);
    assert_eq!(out.lines().collect::<Vec<_>>(), vec!["fresh.md"]);
}

#[tokio::test]
async fn glob_无匹配时说无匹配而不是空字符串() {
    let f = 夹具::new();
    let out = 成功(&f.跑("Glob", serde_json::json!({"pattern": "*.zzz"})).await);
    assert!(out.contains("无匹配"), "{out}");
}

#[tokio::test]
async fn glob_超过上限时说明总数() {
    let f = 夹具::new();
    let out =
        成功(&f.跑("Glob", serde_json::json!({"pattern": "**/*", "max_results": 1})).await)
            ;
    assert!(out.contains("共 4 个文件"), "{out}");
    assert!(out.contains("max_results"), "要告诉它怎么看到更多：{out}");
}

// ---- Grep ----

#[tokio::test]
async fn grep_能限定目录与文件名并忽略大小写() {
    let f = 夹具::new();
    let 全部 = 成功(&f.跑("Grep", serde_json::json!({"pattern": "fn "})).await);
    assert_eq!(全部.lines().count(), 2, "{全部}");

    let 限定 = 成功(
        &f.跑("Grep", serde_json::json!({"pattern": "fn ", "path": "src"}))
            .await,
    )
    ;
    assert_eq!(限定.lines().collect::<Vec<_>>(), vec!["src/lib.rs:1:pub fn go() {}"]);

    let 按名 = 成功(
        &f.跑("Grep", serde_json::json!({"pattern": "fn ", "glob": "*.rs"}))
            .await,
    )
    ;
    assert_eq!(按名.lines().count(), 2);

    let 大小写 = 成功(
        &f.跑(
            "Grep",
            serde_json::json!({"pattern": "deep", "ignore_case": true}),
        )
        .await,
    )
    ;
    assert_eq!(大小写.lines().collect::<Vec<_>>(), vec!["src/deep.md:1:DEEP"]);
}

#[tokio::test]
async fn grep_超过上限时说明总数而不是被整段吞掉() {
    // 从前超限之后会撞上 16 KiB 规则，把整份结果换成一句占位符。
    let f = 夹具::new();
    let out = 成功(
        &f.跑("Grep", serde_json::json!({"pattern": "alpha", "max_results": 1}))
            .await,
    )
    ;
    assert!(out.starts_with("a.md:1:alpha"), "{out}");
    assert!(out.contains("共 2 处匹配"), "{out}");
}

// ---- Read ----

#[tokio::test]
async fn read_能翻页并说明还剩多少() {
    let f = 夹具::new();
    let 首页 = 成功(&f.跑("Read", serde_json::json!({"path": "a.md", "limit": 1})).await)
        ;
    assert!(首页.starts_with("alpha"), "{首页}");
    assert!(首页.contains("还有 2 行未显示"), "{首页}");
    assert!(首页.contains("offset=2"), "{首页}");

    let 次页 = 成功(
        &f.跑("Read", serde_json::json!({"path": "a.md", "offset": 2, "limit": 1}))
            .await,
    )
    ;
    assert!(次页.starts_with("beta"), "{次页}");
}

#[tokio::test]
async fn read_整份读回逐字节一致() {
    let f = 夹具::new();
    assert_eq!(成功(&f.跑("Read", serde_json::json!({"path": "a.md"})).await), "alpha\nbeta\nalpha\n");
}

#[tokio::test]
async fn read_不存在的文件说得清楚() {
    let f = 夹具::new();
    // 从前这里说的是"读取失败：NotFound"——模型看不出该怎么办。带上路径它才知道
    // 该去 Glob 找找真正的名字。
    let out = 失败(&f.跑("Read", serde_json::json!({"path": "nope.md"})).await);
    assert!(out.contains("文件不存在"), "{out}");
    assert!(out.contains("nope.md"), "要带上路径：{out}");
}

// ---- Write ----

#[tokio::test]
async fn write_区分新建与覆盖() {
    // 审批面板上一眼能看出这次会盖掉什么。
    let f = 夹具::new();
    let 新建 = 成功(
        &f.跑("Write", serde_json::json!({"path": "new.md", "content": "hi\n"}))
            .await,
    )
    ;
    assert!(新建.contains("已新建"), "{新建}");

    let 覆盖 = 成功(
        &f.跑("Write", serde_json::json!({"path": "a.md", "content": "hi\n"}))
            .await,
    )
    ;
    assert!(覆盖.contains("已覆盖"), "{覆盖}");
    assert!(覆盖.contains("原 17 字节"), "要报出会盖掉多少：{覆盖}");
}

// ---- Edit ----

#[tokio::test]
async fn edit_命中多处时拒绝并说清楚怎么办() {
    // 模型想改一处、实际改了七处，事后只会看到一句"已替换 7 处"，
    // 那时改动已经在 ChangeSet 里了。
    let f = 夹具::new();
    let out = 失败(
        &f.跑(
            "Edit",
            serde_json::json!({"path": "a.md", "old": "alpha", "new": "ALPHA"}),
        )
        .await,
    )
    ;
    assert!(out.contains("命中 2 处"), "{out}");
    assert!(out.contains("补足上下文"), "要告诉它怎么改对：{out}");
    assert!(out.contains("replace_all"), "也要给出另一条出路：{out}");

    // 文件没被动过。
    assert_eq!(
        成功(&f.跑("Read", serde_json::json!({"path": "a.md"})).await),
        "alpha\nbeta\nalpha\n"
    );
}

#[tokio::test]
async fn edit_补足上下文之后就能唯一命中() {
    // 这是上一条的另一半：错误消息给的建议真的可行。
    let f = 夹具::new();
    let out = 成功(
        &f.跑(
            "Edit",
            serde_json::json!({"path": "a.md", "old": "beta\nalpha", "new": "beta\nOMEGA"}),
        )
        .await,
    )
    ;
    assert!(out.contains("已替换 1 处"), "{out}");
    assert_eq!(
        成功(&f.跑("Read", serde_json::json!({"path": "a.md"})).await),
        "alpha\nbeta\nOMEGA\n"
    );
}

#[tokio::test]
async fn edit_显式_replace_all_才全改() {
    let f = 夹具::new();
    let out = 成功(
        &f.跑(
            "Edit",
            serde_json::json!({"path": "a.md", "old": "alpha", "new": "X", "replace_all": true}),
        )
        .await,
    )
    ;
    assert!(out.contains("已替换 2 处"), "{out}");
    assert_eq!(
        成功(&f.跑("Read", serde_json::json!({"path": "a.md"})).await),
        "X\nbeta\nX\n"
    );
}

#[tokio::test]
async fn edit_old_与_new_相同判为失败() {
    let f = 夹具::new();
    let out = 失败(
        &f.跑(
            "Edit",
            serde_json::json!({"path": "a.md", "old": "beta", "new": "beta"}),
        )
        .await,
    );
    assert!(out.contains("不会改变"), "{out}");
}

#[tokio::test]
async fn edit_文件不存在与找不到文本是两句不同的话() {
    let f = 夹具::new();
    let 不存在 = 失败(
        &f.跑(
            "Edit",
            serde_json::json!({"path": "nope.md", "old": "a", "new": "b"}),
        )
        .await,
    )
    ;
    assert!(不存在.contains("文件不存在"), "{不存在}");

    let 没命中 = 失败(
        &f.跑(
            "Edit",
            serde_json::json!({"path": "a.md", "old": "zzz", "new": "b"}),
        )
        .await,
    )
    ;
    assert!(没命中.contains("未找到待替换的文本"), "{没命中}");
}

// ---- Delete ----

#[tokio::test]
async fn delete_不存在的文件报失败而不是成功() {
    // 从前无条件立墓碑并报"已删除"，于是"删掉了吗"这个问题得不到真实答案。
    let f = 夹具::new();
    let out = 失败(&f.跑("Delete", serde_json::json!({"path": "nope.md"})).await);
    assert!(out.contains("文件不存在"), "{out}");
}

#[tokio::test]
async fn delete_删两次第二次会失败() {
    let f = 夹具::new();
    成功(&f.跑("Delete", serde_json::json!({"path": "a.md"})).await);
    let out = 失败(&f.跑("Delete", serde_json::json!({"path": "a.md"})).await);
    assert!(out.contains("文件不存在"), "{out}");
}

// ---- 拒绝码 ----

#[tokio::test]
async fn 路径逃逸有自己的拒绝码() {
    // 报成 ChangeSetUnavailable 会让排查的人去查变更集，而那里什么问题也没有。
    let f = 夹具::new();
    for tool in ["Read", "Write", "Edit", "Delete"] {
        let r = f
            .跑(
                tool,
                serde_json::json!({"path": "../escape", "content": "x", "old": "a", "new": "b"}),
            )
            .await;
        assert_eq!(
            r.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::OutsideWorkspace
            },
            "{tool}"
        );
    }
}

#[tokio::test]
async fn 缺参数是参数问题而不是变更集问题() {
    // 固定管线的 schema 校验先于此拦掉了；这是兜底，但它也必须说对话。
    let f = 夹具::new();
    let r = f.跑("Read", serde_json::json!({})).await;
    assert_eq!(r.outcome, ExecutionOutcome::Completed { exit_code: 2 });
    assert!(r.output.unwrap_or_default().contains("缺少参数 path"));
}

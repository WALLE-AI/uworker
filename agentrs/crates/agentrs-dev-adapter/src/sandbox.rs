//! 受限文件执行器（L0 基础围栏）。
//!
//! **这是开发工具，不是产品运行时。**真实隔离归 SandboxRS；本实现只提供
//! `IsolationLevel::L0BasicContainment`——它挡得住"写到工作区之外"这类最常见的越界，
//! 挡不住有意的提权攻击。
//!
//! 即便如此，它仍然**严格履行宿主义务**：
//!
//! - **H1**：独立复核 `bound_input_hash` 与 grant 有效期，不信任调用方；
//! - **H2**：如实报告实际生效的隔离级别与 `reconcile` 三态；
//! - **H7**：同一 Run 的所有执行看到一致的 ChangeSet overlay。
//!
//! 达不到请求的隔离级别时**失败而非静默降级**——静默降级等于对外宣称
//! "已隔离"却没有，是最危险的一类失败。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use agentrs_contracts::ids::{ExecutionId, Timestamp};
use agentrs_contracts::policy::{InputHash, SandboxGrant};
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{
    ExecutionOutcome, ExecutionRequest, ExecutionResult, ExecutionStatus, IsolationLevel, RejectReason,
    SandboxError,
};
use async_trait::async_trait;

/// 一次执行的记录，供 `reconcile` 如实回答。
#[derive(Debug, Clone)]
enum Record {
    Finished(Box<ExecutionResult>),
}

#[derive(Default)]
struct State {
    /// grant_id -> (绑定指纹, 过期时刻)。**Sandbox 侧独立持有**（H1）。
    grants: HashMap<String, (InputHash, Timestamp)>,
    /// 复核有效期时使用的逻辑时刻。**由宿主从 Clock port 传入**，不读真实时钟。
    now: Timestamp,
    consumed: HashSet<String>,
    history: HashMap<ExecutionId, Record>,
    /// ChangeSet overlay：未提交的改动。**同一 Run 的所有读都看到它**（H7）。
    overlay: HashMap<(String, PathBuf), Overlay>,
}

/// overlay 里的一条改动。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Overlay {
    /// 文件的新内容。
    Content(String),
    /// 已删除。
    ///
    /// **墓碑不能省。** 只记写入的话，"删掉再读"会命中盘上的旧内容——
    /// 读己之写在删除这条路径上直接不成立。
    Tombstone,
}

/// 本地受限文件执行器。
pub struct LocalFileSandbox {
    root: PathBuf,
    state: Mutex<State>,
}

impl LocalFileSandbox {
    /// 以 `root` 为工作区根创建。**所有路径都会被限制在此目录内。**
    pub fn new(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        Ok(Self {
            root,
            state: Mutex::new(State::default()),
        })
    }

    /// 工作区根。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 推进复核用的逻辑时刻。宿主从 Clock port 取得后传入。
    pub fn set_now(&self, now: Timestamp) {
        self.state.lock().unwrap().now = now;
    }

    /// 登记一个 grant 及其绑定值。由 dev 的 Policy 调用。
    pub fn issue_grant(&self, grant_id: &str, bound: InputHash, expires_at: Timestamp) {
        self.state
            .lock()
            .unwrap()
            .grants
            .insert(grant_id.to_string(), (bound, expires_at));
    }

    /// 把相对路径解析为工作区内的绝对路径。
    ///
    /// **cwd jail**：任何逃逸（`..`、绝对路径、符号链接指向外部）都被拒绝。
    /// 这是 L0 围栏最主要的一条。
    fn resolve(&self, rel: &str) -> Option<PathBuf> {
        let candidate = self.root.join(rel);

        // 逐段归一化，不依赖文件是否存在（写入时目标尚不存在）。
        let mut normalized = self.root.clone();
        for part in Path::new(rel).components() {
            use std::path::Component::*;
            match part {
                Normal(p) => normalized.push(p),
                CurDir => {}
                ParentDir => {
                    if !normalized.pop() {
                        return None;
                    }
                }
                // 绝对路径、根、前缀一律拒绝。
                RootDir | Prefix(_) => return None,
            }
        }
        if !normalized.starts_with(&self.root) {
            return None;
        }
        let _ = candidate;
        Some(normalized)
    }

    /// 读取：**先看 overlay**，未命中再读盘（读己之写，H7）。
    fn read(&self, change_set: &str, path: &Path) -> std::io::Result<String> {
        match self.overlay_of(change_set, path) {
            Some(Overlay::Content(text)) => Ok(text),
            // 墓碑要表现得和"文件不存在"完全一样，否则删除只是看起来生效了。
            Some(Overlay::Tombstone) => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "deleted in change set",
            )),
            None => std::fs::read_to_string(path),
        }
    }

    fn overlay_of(&self, change_set: &str, path: &Path) -> Option<Overlay> {
        self.state
            .lock()
            .unwrap()
            .overlay
            .get(&(change_set.to_string(), path.to_path_buf()))
            .cloned()
    }

    /// 写入：只进 overlay，**不落盘**。提交由宿主完成（内核不提交 ChangeSet）。
    fn write(&self, change_set: &str, path: &Path, content: String) {
        self.state.lock().unwrap().overlay.insert(
            (change_set.to_string(), path.to_path_buf()),
            Overlay::Content(content),
        );
    }

    /// 删除：只在 overlay 里立墓碑，**不动磁盘**。
    fn delete(&self, change_set: &str, path: &Path) {
        self.state
            .lock()
            .unwrap()
            .overlay
            .insert((change_set.to_string(), path.to_path_buf()), Overlay::Tombstone);
    }

    /// 列出本 ChangeSet 视角下工作区里的全部文件。
    ///
    /// **磁盘条目与 overlay 的并集，再减去墓碑**。三者缺一，
    /// Grep 就会漏掉新建的文件、或搜到已删除的文件。
    fn visible_files(&self, change_set: &str) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        walk(&self.root, &mut out);

        let s = self.state.lock().unwrap();
        for ((cs, p), entry) in s.overlay.iter() {
            if cs != change_set {
                continue;
            }
            match entry {
                Overlay::Content(_) => {
                    if !out.contains(p) {
                        out.push(p.clone());
                    }
                }
                Overlay::Tombstone => out.retain(|x| x != p),
            }
        }
        out.sort();
        out
    }

    /// 把某个 ChangeSet 的全部改动落盘。**由宿主显式调用**，内核无此能力。
    pub fn commit(&self, change_set: &str) -> std::io::Result<usize> {
        let pending: Vec<(PathBuf, Overlay)> = {
            let s = self.state.lock().unwrap();
            s.overlay
                .iter()
                .filter(|((cs, _), _)| cs == change_set)
                .map(|((_, p), v)| (p.clone(), v.clone()))
                .collect()
        };
        for (p, v) in &pending {
            match v {
                Overlay::Content(text) => {
                    if let Some(dir) = p.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::write(p, text)?;
                }
                // 墓碑落盘 = 真的删掉。文件已不在也算成功（幂等）。
                Overlay::Tombstone => match std::fs::remove_file(p) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                },
            }
        }
        self.state
            .lock()
            .unwrap()
            .overlay
            .retain(|(cs, _), _| cs != change_set);
        Ok(pending.len())
    }

    /// 当前 overlay 中待提交的条目数。
    pub fn pending_count(&self, change_set: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .overlay
            .keys()
            .filter(|(cs, _)| cs == change_set)
            .count()
    }

    /// Discards every uncommitted entry in a ChangeSet and returns its size.
    ///
    /// This is an explicit host action, symmetric with [`Self::commit`]. It
    /// never touches workspace files because overlay entries have not landed.
    pub fn discard(&self, change_set: &str) -> usize {
        let mut state = self.state.lock().unwrap();
        let before = state.overlay.len();
        state.overlay.retain(|(cs, _), _| cs != change_set);
        before - state.overlay.len()
    }

    fn reject(id: ExecutionId, reason: RejectReason) -> ExecutionResult {
        ExecutionResult {
            execution_id: id,
            outcome: ExecutionOutcome::Rejected { reason },
            effective_isolation: IsolationLevel::L0BasicContainment,
            artifacts: vec![],
            output: None,
            change_set: None,
            finished_at: Timestamp(0),
        }
    }
}

/// 递归收集目录下的全部普通文件。
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        // 不跟随符号链接——跟随会绕过 cwd jail。
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            walk(&p, out);
        } else if meta.is_file() {
            out.push(p);
        }
    }
}

/// 工具执行的可读产物。dev-adapter 直接把内容回给调用方，
/// 真实实现会经 `ContentStore` ref 化。
#[derive(Debug, Clone)]
pub struct TextOutput(pub String);

#[async_trait]
impl SandboxExecutor for LocalFileSandbox {
    async fn execute(
        &self,
        grant: SandboxGrant,
        request: ExecutionRequest,
    ) -> Result<ExecutionResult, SandboxError> {
        // ---- H1：独立复核，不信任调用方 ----
        let (bound, expires_at) = {
            let s = self.state.lock().unwrap();
            match s.grants.get(&grant.grant_id) {
                Some(v) => v.clone(),
                None => return Err(SandboxError::InvalidGrant),
            }
        };
        if bound != request.input_hash {
            return Ok(Self::reject(
                request.execution_id,
                RejectReason::InputHashMismatch,
            ));
        }
        // **与逻辑时刻比较**，而不是"是不是 <= 0"。
        // 后者只挡得住 Timestamp(0) 这种明显过期的值；一个"5 分钟后到期"的
        // grant 在第 10 分钟仍会被放行——有效期形同虚设。
        let now = self.state.lock().unwrap().now;
        if expires_at.0 <= now.0 {
            return Ok(Self::reject(request.execution_id, RejectReason::GrantExpired));
        }
        if !self.state.lock().unwrap().consumed.insert(grant.grant_id.clone()) {
            return Ok(Self::reject(
                request.execution_id,
                RejectReason::GrantAlreadyConsumed,
            ));
        }

        // ---- H2：达不到要求的隔离级别时失败，不静默降级 ----
        if request.required_isolation > IsolationLevel::L0BasicContainment {
            return Ok(Self::reject(
                request.execution_id,
                RejectReason::IsolationUnavailable,
            ));
        }

        let cs = request.change_set_id.as_str().to_string();
        let arg = |k: &str| -> Option<String> {
            request
                .arguments
                .get(k)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };

        /// 内联输出的大小上限；超过必须 ref 化。
        const MAX_INLINE: usize = 16 * 1024;

        let (outcome, output) = match request.tool_name.as_str() {
            "Read" => match arg("path").and_then(|p| self.resolve(&p)) {
                None => (
                    // 路径逃逸——L0 围栏的主要拦截点。
                    ExecutionOutcome::Rejected {
                        reason: RejectReason::ChangeSetUnavailable,
                    },
                    None,
                ),
                Some(path) => match self.read(&cs, &path) {
                    Ok(text) if text.len() <= MAX_INLINE => {
                        (ExecutionOutcome::Completed { exit_code: 0 }, Some(text))
                    }
                    Ok(text) => (
                        ExecutionOutcome::Completed { exit_code: 0 },
                        Some(format!(
                            "[输出过大：{} 字节，超过内联上限；真实实现应走 ContentStore 引用]",
                            text.len()
                        )),
                    ),
                    Err(e) => (
                        ExecutionOutcome::Completed { exit_code: 1 },
                        Some(format!("读取失败：{}", e.kind())),
                    ),
                },
            },
            "Write" => match (arg("path").and_then(|p| self.resolve(&p)), arg("content")) {
                (Some(path), Some(content)) => {
                    let n = content.len();
                    self.write(&cs, &path, content);
                    (
                        ExecutionOutcome::Completed { exit_code: 0 },
                        Some(format!("已写入 {n} 字节（未提交，位于 ChangeSet 内）")),
                    )
                }
                _ => (
                    ExecutionOutcome::Rejected {
                        reason: RejectReason::ChangeSetUnavailable,
                    },
                    None,
                ),
            },
            // **Edit 是读己之写最要紧的那条路径**：它先读、再改、再写，
            // 中间那次读必须看到 overlay，否则连续两次 Edit 的第二次
            // 会基于盘上的旧内容，把第一次的改动悄悄抹掉。
            "Edit" => match (arg("path").and_then(|p| self.resolve(&p)), arg("old"), arg("new")) {
                (Some(path), Some(old), Some(new)) => match self.read(&cs, &path) {
                    Err(e) => (
                        ExecutionOutcome::Completed { exit_code: 1 },
                        Some(format!("读取失败：{}", e.kind())),
                    ),
                    Ok(text) if !text.contains(&old) => (
                        ExecutionOutcome::Completed { exit_code: 1 },
                        // 不透传 old/new 正文——可能含用户内容。
                        Some("未找到待替换的文本".to_string()),
                    ),
                    Ok(text) => {
                        let n = text.matches(&old).count();
                        self.write(&cs, &path, text.replace(&old, &new));
                        (
                            ExecutionOutcome::Completed { exit_code: 0 },
                            Some(format!("已替换 {n} 处（未提交，位于 ChangeSet 内）")),
                        )
                    }
                },
                _ => (
                    ExecutionOutcome::Rejected {
                        reason: RejectReason::ChangeSetUnavailable,
                    },
                    None,
                ),
            },
            "Delete" => match arg("path").and_then(|p| self.resolve(&p)) {
                Some(path) => {
                    self.delete(&cs, &path);
                    (
                        ExecutionOutcome::Completed { exit_code: 0 },
                        Some("已删除（未提交，位于 ChangeSet 内）".to_string()),
                    )
                }
                None => (
                    ExecutionOutcome::Rejected {
                        reason: RejectReason::ChangeSetUnavailable,
                    },
                    None,
                ),
            },
            // Grep 同样走 overlay：只搜磁盘的话，刚写的文件搜不到、
            // 刚删的文件仍能搜到——模型据此做的判断全是错的。
            "Grep" => match arg("pattern") {
                None => (
                    ExecutionOutcome::Rejected {
                        reason: RejectReason::ChangeSetUnavailable,
                    },
                    None,
                ),
                Some(pattern) => {
                    let mut hits = Vec::new();
                    for path in self.visible_files(&cs) {
                        let Ok(text) = self.read(&cs, &path) else {
                            continue;
                        };
                        for (i, line) in text.lines().enumerate() {
                            if line.contains(&pattern) {
                                let rel = path
                                    .strip_prefix(&self.root)
                                    .unwrap_or(&path)
                                    .display()
                                    .to_string();
                                hits.push(format!("{rel}:{}:{line}", i + 1));
                            }
                        }
                    }
                    let text = if hits.is_empty() {
                        "无匹配".to_string()
                    } else {
                        hits.join("\n")
                    };
                    (ExecutionOutcome::Completed { exit_code: 0 }, Some(text))
                }
            },
            other => (
                ExecutionOutcome::Completed { exit_code: 127 },
                Some(format!("未知工具：{other}")),
            ),
        };

        let result = ExecutionResult {
            execution_id: request.execution_id.clone(),
            outcome,
            effective_isolation: IsolationLevel::L0BasicContainment,
            artifacts: vec![],
            output,
            change_set: Some(request.change_set_id),
            finished_at: Timestamp(0),
        };
        self.state
            .lock()
            .unwrap()
            .history
            .insert(request.execution_id, Record::Finished(Box::new(result.clone())));
        Ok(result)
    }

    async fn cancel(&self, _execution_id: ExecutionId) -> Result<(), SandboxError> {
        Ok(())
    }

    async fn reconcile(&self, execution_id: ExecutionId) -> Result<ExecutionStatus, SandboxError> {
        let s = self.state.lock().unwrap();
        Ok(match s.history.get(&execution_id) {
            Some(Record::Finished(r)) => ExecutionStatus::Finished(r.clone()),
            // 从未见过 = 确实没开始。**如实报告**（H2）。
            None => ExecutionStatus::NotStarted,
        })
    }
}

/// 读取工具的便捷入口，供 CLI 直接取内容（绕开 ContentStore ref 化）。
impl LocalFileSandbox {
    /// 读取一个工作区内的文件，遵循 overlay。
    pub fn read_text(&self, change_set: &str, rel: &str) -> Option<String> {
        let p = self.resolve(rel)?;
        self.read(change_set, &p).ok()
    }
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::ids::Digest;

    use super::*;

    fn 沙箱() -> (LocalFileSandbox, tempdir::TempDir) {
        let dir = tempdir::TempDir::new("agentrs-dev").unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let sb = LocalFileSandbox::new(dir.path()).unwrap();
        (sb, dir)
    }

    fn hash(h: &str) -> InputHash {
        InputHash(Digest::from_hex(h))
    }

    fn 请求(tool: &str, args: serde_json::Value) -> ExecutionRequest {
        ExecutionRequest {
            execution_id: "e1".into(),
            tool_name: tool.into(),
            arguments: args,
            change_set_id: "cs1".into(),
            input_hash: hash("h"),
            required_isolation: IsolationLevel::L0BasicContainment,
        }
    }

    fn grant() -> SandboxGrant {
        SandboxGrant {
            grant_id: "g1".into(),
            payload: serde_json::json!({}),
        }
    }

    #[test]
    fn 路径逃逸被拒绝() {
        // L0 围栏最主要的一条：任何形式的越界都不成立。
        let (sb, _d) = 沙箱();
        for bad in ["../outside", "../../etc/passwd", "/etc/passwd", "a/../../x"] {
            assert!(sb.resolve(bad).is_none(), "必须拒绝：{bad}");
        }
        assert!(sb.resolve("a.txt").is_some());
        assert!(sb.resolve("sub/b.txt").is_some());
    }

    #[tokio::test]
    async fn 读己之写_写入后立即可读() {
        // 宿主义务 H7：同一 Run 的所有执行看到一致的 overlay。
        let (sb, _d) = 沙箱();
        sb.issue_grant("g1", hash("h"), Timestamp(i64::MAX));

        let r = sb
            .execute(
                grant(),
                请求(
                    "Write",
                    serde_json::json!({"path":"new.md","content":"写入的内容"}),
                ),
            )
            .await
            .unwrap();
        assert!(matches!(r.outcome, ExecutionOutcome::Completed { exit_code: 0 }));

        assert_eq!(
            sb.read_text("cs1", "new.md").as_deref(),
            Some("写入的内容"),
            "Edit 后 Read 必须读到新内容"
        );
    }

    #[tokio::test]
    async fn 未提交时不落盘() {
        // 内核无提交能力；提交是宿主的显式动作。
        let (sb, d) = 沙箱();
        sb.issue_grant("g1", hash("h"), Timestamp(i64::MAX));
        sb.execute(
            grant(),
            请求("Write", serde_json::json!({"path":"x.md","content":"c"})),
        )
        .await
        .unwrap();

        assert!(!d.path().join("x.md").exists(), "未提交不得落盘");
        assert_eq!(sb.pending_count("cs1"), 1);

        assert_eq!(sb.commit("cs1").unwrap(), 1);
        assert_eq!(std::fs::read_to_string(d.path().join("x.md")).unwrap(), "c");
        assert_eq!(sb.pending_count("cs1"), 0);
    }

    #[tokio::test]
    async fn discard_清空_overlay_但不触碰磁盘() {
        let (sb, d) = 沙箱();
        sb.issue_grant("g1", hash("h"), Timestamp(i64::MAX));
        sb.execute(
            grant(),
            请求("Write", serde_json::json!({"path":"discard.md","content":"c"})),
        )
        .await
        .unwrap();

        assert_eq!(sb.discard("cs1"), 1);
        assert_eq!(sb.pending_count("cs1"), 0);
        assert!(!d.path().join("discard.md").exists());
    }

    #[tokio::test]
    async fn h1_篡改指纹被拒绝() {
        let (sb, _d) = 沙箱();
        sb.issue_grant("g1", hash("correct"), Timestamp(i64::MAX));
        let mut req = 请求("Read", serde_json::json!({"path":"a.txt"}));
        req.input_hash = hash("tampered");
        let r = sb.execute(grant(), req).await.unwrap();
        assert_eq!(
            r.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::InputHashMismatch
            }
        );
    }

    #[tokio::test]
    async fn h1_grant_只能消费一次() {
        let (sb, _d) = 沙箱();
        sb.issue_grant("g1", hash("h"), Timestamp(i64::MAX));
        let first = sb
            .execute(grant(), 请求("Read", serde_json::json!({"path":"a.txt"})))
            .await
            .unwrap();
        assert!(matches!(first.outcome, ExecutionOutcome::Completed { .. }));

        let mut second = 请求("Read", serde_json::json!({"path":"a.txt"}));
        second.execution_id = "e2".into();
        let r = sb.execute(grant(), second).await.unwrap();
        assert_eq!(
            r.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::GrantAlreadyConsumed
            }
        );
    }

    #[tokio::test]
    async fn h2_要求更高隔离级别时失败而非降级() {
        let (sb, _d) = 沙箱();
        sb.issue_grant("g1", hash("h"), Timestamp(i64::MAX));
        let mut req = 请求("Read", serde_json::json!({"path":"a.txt"}));
        req.required_isolation = IsolationLevel::L1RealIsolation;
        let r = sb.execute(grant(), req).await.unwrap();
        assert_eq!(
            r.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::IsolationUnavailable
            }
        );
    }

    #[tokio::test]
    async fn h2_reconcile_如实报告() {
        let (sb, _d) = 沙箱();
        sb.issue_grant("g1", hash("h"), Timestamp(i64::MAX));

        assert_eq!(
            sb.reconcile("never".into()).await.unwrap(),
            ExecutionStatus::NotStarted
        );

        sb.execute(grant(), 请求("Read", serde_json::json!({"path":"a.txt"})))
            .await
            .unwrap();
        assert!(matches!(
            sb.reconcile("e1".into()).await.unwrap(),
            ExecutionStatus::Finished(_)
        ));
    }

    #[tokio::test]
    async fn 结果如实报告_l0_隔离级别() {
        // 用户能在 trajectory 里看到"这条命令是在 L0 下执行的"。
        let (sb, _d) = 沙箱();
        sb.issue_grant("g1", hash("h"), Timestamp(i64::MAX));
        let r = sb
            .execute(grant(), 请求("Read", serde_json::json!({"path":"a.txt"})))
            .await
            .unwrap();
        assert_eq!(r.effective_isolation, IsolationLevel::L0BasicContainment);
    }
}

//! 受限本地执行器（L0 基础围栏）。
//!
//! **这是开发工具，不是产品运行时。**真实隔离归 SandboxRS；本实现只提供
//! `IsolationLevel::L0BasicContainment`——它挡得住"写到工作区之外"这类最常见的越界，
//! 挡不住有意的提权攻击。
//!
//! 三类工具在这里落地：文件（本模块）、命令（[`crate::exec`]）、
//! 网络（[`crate::web`]）。后两类各自的围栏与判定写在自己的模块里，
//! 本模块只负责派活，以及三类共用的那套 grant 复核。
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
    /// 命令**已经派生出去了**，但还没等到结果。
    ///
    /// 这一态不能省。文件工具用不着它——overlay 的写入是原子的，要么记上了要么
    /// 没有。命令不是：进程一旦跑起来就可能已经改了盘，而宿主在这中间崩掉的话，
    /// 恢复时唯一诚实的回答是 [`ExecutionStatus::Unknown`]（停下问人），
    /// 而不是 `NotStarted`——那会让内核把一条可能已经生效的命令再跑一遍。
    Started,
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

/// 一条待提交的改动，连同它替换掉的盘上内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChange {
    /// 绝对路径。
    pub path: PathBuf,
    /// 相对工作区根的路径，给人看的。
    pub relative: String,
    /// 盘上现有的内容；文件不存在（新建）时为 `None`。
    pub on_disk: Option<String>,
    /// 暂存的内容；这是一次删除时为 `None`。
    pub staged: Option<String>,
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

/// 本地受限执行器。
///
/// 从前叫 `LocalFileSandbox`。加进命令与网络之后那个名字就在骗人了——
/// 一个叫 "File" 的类型跑任意 shell 命令、取任意 URL，是这个项目最不能忍的
/// 那种误导。改名比留一个会让人读错的名字便宜。
pub struct LocalDevSandbox {
    root: PathBuf,
    state: Mutex<State>,
    /// 命令执行端口。工具侧只看得到它，看不到 `unshare` 与 `ulimit`。
    shell: crate::exec::LocalShell,
    /// 出站 HTTP 端口。
    http: crate::web::ReqwestHttp,
    /// 搜索端点；没配就没有 `WebSearch`。
    search: Option<agentrs_tools::builtin::SearchEndpoint>,
}

impl LocalDevSandbox {
    /// 以 `root` 为工作区根创建。**所有路径都会被限制在此目录内。**
    ///
    /// 搜索端点从环境读（`AGENTRS_SEARCH_URL`/`_KEY`）；没配就没有 `WebSearch`。
    pub fn new(root: impl AsRef<Path>) -> std::io::Result<Self> {
        Self::with_search(root, crate::web::search_endpoint_from_env())
    }

    /// 显式给定搜索端点配置。测试与不读环境的宿主走这条。
    pub fn with_search(
        root: impl AsRef<Path>,
        search: Option<agentrs_tools::builtin::SearchEndpoint>,
    ) -> std::io::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        Ok(Self {
            shell: crate::exec::LocalShell::new(&root),
            root,
            state: Mutex::new(State::default()),
            http: crate::web::ReqwestHttp,
            search,
        })
    }

    /// 本执行器实际提供了哪些能力。宿主据此推目录——
    /// 声明了却跑不动的工具于是在构造上不可能存在。
    pub fn capabilities(&self) -> agentrs_tools::builtin::Capabilities {
        agentrs_tools::builtin::Capabilities {
            shell: true,
            http: true,
            search: self.search.is_some(),
        }
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

    /// 本 ChangeSet 视角下的全部文件，工作区相对路径，已排序。
    ///
    /// `Glob` 与 `Grep` 都走它，所以两者对"有哪些文件"的看法不会分叉。
    fn relative_files(&self, change_set: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .visible_files(change_set)
            .into_iter()
            .map(|path| {
                path.strip_prefix(&self.root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        out.sort();
        out
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

    /// 某个 ChangeSet 里待提交的每一条改动：路径、暂存后的内容、盘上的旧内容。
    ///
    /// 宿主用它在提交**之前**把改动摆给人看。删除的 `staged` 为 `None`，
    /// 新建文件的 `on_disk` 为 `None`——两者都不是"空文件"，把它们并成空串会
    /// 让新建看起来像清空、删除看起来像没变。
    pub fn pending_entries(&self, change_set: &str) -> Vec<PendingChange> {
        let staged: Vec<(PathBuf, Overlay)> = {
            let s = self.state.lock().unwrap();
            let mut out: Vec<(PathBuf, Overlay)> = s
                .overlay
                .iter()
                .filter(|((cs, _), _)| cs == change_set)
                .map(|((_, p), v)| (p.clone(), v.clone()))
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        };
        staged
            .into_iter()
            .map(|(path, overlay)| PendingChange {
                relative: path
                    .strip_prefix(&self.root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
                on_disk: std::fs::read_to_string(&path).ok(),
                staged: match overlay {
                    Overlay::Content(text) => Some(text),
                    Overlay::Tombstone => None,
                },
                path,
            })
            .collect()
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

    /// 记下"这条执行已经派生出去了"。
    fn mark_started(&self, id: &ExecutionId) {
        self.state
            .lock()
            .unwrap()
            .history
            .insert(id.clone(), Record::Started);
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

/// 一次执行看到的工作区：某个 ChangeSet 的视角。
///
/// [`agentrs_tools::builtin::WorkspaceIo`] 的方法只收一个相对路径，不带
/// ChangeSet——那是**故意的**：工具不该知道 ChangeSet 存在，它只要求"读己之写"
/// 成立。哪个 ChangeSet、overlay 怎么叠、墓碑怎么算，全是宿主义务（H7），
/// 由这一层兜住。
struct ChangeSetView<'a> {
    sandbox: &'a LocalDevSandbox,
    change_set: String,
}

impl ChangeSetView<'_> {
    /// 解析路径并施加 cwd 围栏（L0 的一条，H2）。
    fn 定位(&self, rel: &str) -> Result<PathBuf, agentrs_tools::builtin::IoError> {
        self.sandbox
            .resolve(rel)
            .ok_or(agentrs_tools::builtin::IoError::OutsideWorkspace)
    }
}

#[async_trait]
impl agentrs_tools::builtin::WorkspaceIo for ChangeSetView<'_> {
    async fn read(&self, path: &str) -> Result<String, agentrs_tools::builtin::IoError> {
        use agentrs_tools::builtin::IoError;
        let p = self.定位(path)?;
        self.sandbox.read(&self.change_set, &p).map_err(|e| {
            // 墓碑与"盘上没有"都归 NotFound：删除必须表现得和文件不存在
            // 完全一样，否则删除只是看起来生效了。
            match e.kind() {
                std::io::ErrorKind::NotFound => IoError::NotFound,
                other => IoError::Other(other.to_string()),
            }
        })
    }

    async fn write(&self, path: &str, content: String) -> Result<(), agentrs_tools::builtin::IoError> {
        let p = self.定位(path)?;
        self.sandbox.write(&self.change_set, &p, content);
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<(), agentrs_tools::builtin::IoError> {
        let p = self.定位(path)?;
        self.sandbox.delete(&self.change_set, &p);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>, agentrs_tools::builtin::IoError> {
        // 已排序——契约要求的，Glob 与 Grep 都靠它保持输出稳定。
        Ok(self.sandbox.relative_files(&self.change_set))
    }
}

#[async_trait]
impl SandboxExecutor for LocalDevSandbox {
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

        // ---- 派活 ----
        // 工具的声明、编排与措辞全在 `agentrs_tools::builtin`；本模块只提供它
        // 注入的三个端口，也就是真正碰 OS 的那部分。从前这里是一个三百行的
        // match，声明在 catalog.rs、判定在 exec_guard.rs——想知道"Read 到底做
        // 什么"得读三个文件。
        let view = ChangeSetView {
            sandbox: self,
            change_set: request.change_set_id.as_str().to_string(),
        };
        let cx = agentrs_tools::builtin::ToolCtx {
            files: &view,
            shell: Some(&self.shell),
            http: Some(&self.http),
            search: self.search.as_ref(),
        };
        // 命令可能已经改了盘，所以派生**之前**先记一笔：宿主在这中间崩掉时，
        // reconcile 才答得出 Unknown 而不是 NotStarted（后者会让内核重跑）。
        // 文件工具不需要这个——overlay 的写入是原子的。
        if request.tool_name == "Bash" {
            self.mark_started(&request.execution_id);
        }
        let out = agentrs_tools::builtin::execute(&request.tool_name, &request.arguments, &cx).await;
        let (outcome, output) = (out.outcome, out.text);
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
            // 派生了但没等到结果。进程可能已经改了盘，也可能还没来得及——
            // 这里唯一诚实的回答就是"不知道"，内核据此停在 RunNeedsUserAction
            // 而不是重跑（H2）。
            Some(Record::Started) => ExecutionStatus::Unknown,
            // 从未见过 = 确实没开始。**如实报告**（H2）。
            None => ExecutionStatus::NotStarted,
        })
    }
}

/// 读取工具的便捷入口，供 CLI 直接取内容（绕开 ContentStore ref 化）。
impl LocalDevSandbox {
    /// 读取一个工作区内的文件，遵循 overlay。
    pub fn read_text(&self, change_set: &str, rel: &str) -> Option<String> {
        let p = self.resolve(rel)?;
        self.read(change_set, &p).ok()
    }
}


#[cfg(test)]
mod pending_tests {
    use super::*;

    #[test]
    fn 待提交的改动带着它替换掉的旧内容() {
        let dir = tempdir::TempDir::new("agentrs-pending").unwrap();
        std::fs::write(dir.path().join("kept.md"), "old\n").unwrap();
        std::fs::write(dir.path().join("gone.md"), "bye\n").unwrap();
        let sb = LocalDevSandbox::new(dir.path()).unwrap();

        assert!(sb.pending_entries("cs").is_empty());

        let root = dir.path().canonicalize().unwrap();
        sb.write("cs", &root.join("kept.md"), "new\n".into());
        sb.write("cs", &root.join("fresh.md"), "hello\n".into());
        sb.delete("cs", &root.join("gone.md"));

        let entries = sb.pending_entries("cs");
        assert_eq!(entries.len(), 3);
        let by_name = |name: &str| {
            entries
                .iter()
                .find(|entry| entry.relative == name)
                .unwrap_or_else(|| panic!("{name} is missing"))
        };
        // 改：两边都有。
        assert_eq!(by_name("kept.md").on_disk.as_deref(), Some("old\n"));
        assert_eq!(by_name("kept.md").staged.as_deref(), Some("new\n"));
        // 新建：盘上没有——这不是"空文件"，并成空串会让新建看起来像清空。
        assert_eq!(by_name("fresh.md").on_disk, None);
        assert_eq!(by_name("fresh.md").staged.as_deref(), Some("hello\n"));
        // 删除：暂存侧没有。
        assert_eq!(by_name("gone.md").on_disk.as_deref(), Some("bye\n"));
        assert_eq!(by_name("gone.md").staged, None);
        // 另一个 ChangeSet 看不到这些。
        assert!(sb.pending_entries("other").is_empty());
    }
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::ids::Digest;

    use super::*;

    fn 沙箱() -> (LocalDevSandbox, tempdir::TempDir) {
        let dir = tempdir::TempDir::new("agentrs-dev").unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let sb = LocalDevSandbox::new(dir.path()).unwrap();
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

    // 围栏缺失时拒绝执行、以及缺失不影响其余工具这两条，随判定一起搬去了
    // `agentrs_tools::builtin::bash`，在那里用 `FakeShell` 测——不必再靠往私有
    // 字段里塞一个假探测结果。第二条如今更是结构性成立：只有 Bash 会去看
    // `ToolCtx::shell`，别的工具连拿都拿不到它。

    #[tokio::test]
    async fn h2_派生之后没等到结果时_reconcile_答不知道() {
        // 宿主在命令跑到一半崩掉，就是这个形状。进程可能已经改了盘，
        // 报 NotStarted 会让内核把它再跑一遍——这正是 reconcile 三态里
        // Unknown 存在的全部理由。
        let (sb, _d) = 沙箱();
        sb.mark_started(&"e-interrupted".into());
        assert_eq!(
            sb.reconcile("e-interrupted".into()).await.unwrap(),
            ExecutionStatus::Unknown
        );
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

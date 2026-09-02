//! 测试用的内存工作区。
//!
//! 工具语义从前只能在 `dev-adapter` 里测，而那意味着每条测试先建一个临时目录、
//! 写几个文件、再删掉——41 条测试 41 个 TempDir。搬进来之后它们跑在这个 map 上，
//! 于是 `cargo test -p agentrs-tools` **一次磁盘也不碰**。
//!
//! 这个 fake 只兑现 [`WorkspaceIo`] 的契约，不模拟 overlay：overlay 是宿主义务
//! （H7），它的正确性归 `dev-adapter` 的集成测试管。这里管的是"给定一份文件内容，
//! 工具说的话对不对"。

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use super::{IoError, WorkspaceIo};

/// 一个内存工作区。
pub struct FakeWorkspace {
    files: Mutex<BTreeMap<String, String>>,
    /// 这些路径一律报越界，用来测围栏那条分支而不必真有一个围栏。
    outside: Vec<String>,
}

impl FakeWorkspace {
    /// 用若干 `(路径, 内容)` 建一个。
    pub fn new<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            files: Mutex::new(
                files
                    .into_iter()
                    .map(|(p, c)| (p.to_string(), c.to_string()))
                    .collect(),
            ),
            outside: Vec::new(),
        }
    }

    /// 声明某个路径落在工作区之外。
    pub fn with_outside(mut self, paths: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.outside = paths.into_iter().map(Into::into).collect();
        self
    }

    /// 当前内容，供断言用。
    pub fn content(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(path).cloned()
    }

    /// 现在有哪些文件。
    pub fn paths(&self) -> Vec<String> {
        self.files.lock().unwrap().keys().cloned().collect()
    }

    fn 围栏(&self, path: &str) -> Result<(), IoError> {
        if self.outside.iter().any(|p| p == path) {
            return Err(IoError::OutsideWorkspace);
        }
        Ok(())
    }
}

#[async_trait]
impl WorkspaceIo for FakeWorkspace {
    async fn read(&self, path: &str) -> Result<String, IoError> {
        self.围栏(path)?;
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or(IoError::NotFound)
    }

    async fn write(&self, path: &str, content: String) -> Result<(), IoError> {
        self.围栏(path)?;
        self.files.lock().unwrap().insert(path.to_string(), content);
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<(), IoError> {
        self.围栏(path)?;
        self.files.lock().unwrap().remove(path);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>, IoError> {
        // BTreeMap 的 keys 天然有序——契约要求已排序，fake 不该在这一点上
        // 比真实现宽松，否则依赖顺序的 bug 只会在真跑时才冒出来。
        Ok(self.files.lock().unwrap().keys().cloned().collect())
    }
}

/// 测试用的假 shell。
pub struct FakeShell {
    /// 围栏自检的答案。
    pub containment: Result<(), Vec<String>>,
    /// 每次 `run` 都回这个。
    pub outcome: super::ShellOutcome,
    /// 收到过的命令。
    pub seen: Mutex<Vec<String>>,
}

impl FakeShell {
    /// 一个围栏齐备、命令成功的 shell。
    pub fn ok(stdout: &str) -> Self {
        Self {
            containment: Ok(()),
            outcome: super::ShellOutcome::Settled {
                timed_out: false,
                exit_code: 0,
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            },
            seen: Mutex::new(Vec::new()),
        }
    }

    /// 一个围栏残缺的 shell。
    pub fn missing(items: &[&str]) -> Self {
        Self {
            containment: Err(items.iter().map(|s| s.to_string()).collect()),
            outcome: super::ShellOutcome::Spawn("unreachable".into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// 换掉它的结局。
    pub fn returning(mut self, outcome: super::ShellOutcome) -> Self {
        self.outcome = outcome;
        self
    }
}

#[async_trait]
impl super::Shell for FakeShell {
    async fn containment(&self) -> Result<(), Vec<String>> {
        self.containment.clone()
    }

    async fn run(&self, command: &str, _timeout: std::time::Duration) -> super::ShellOutcome {
        self.seen.lock().unwrap().push(command.to_string());
        self.outcome.clone()
    }
}

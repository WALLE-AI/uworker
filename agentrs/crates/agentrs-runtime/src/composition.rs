//! Live 资源平面：scope、owner、有序清理（架构 §5.1，任务 T03A）。
//!
//! 这是四平面模型里的 **③ Live 资源平面**。它的规则与 durable 平面完全不同：
//!
//! | | Live 平面（本模块） | Durable 平面 |
//! |---|---|---|
//! | 管什么 | stream / task / listener / lease / inbox | 事实、意图、结果 |
//! | 结束方式 | `cancel → drain → 反序 cleanup` | `StepIntent → reconcile → StepResult` |
//! | 崩溃后 | **不恢复**，重新创建 | 权威来源，全部保留 |
//!
//! **两者不可互相替代**：`cleanup` 只释放进程内资源，**不宣称历史未发生**。
//! 用 `Drop` 假装回滚了一次 `rm -rf` 是错的——外部副作用只能靠 `reconcile` 处理。
//!
//! ## Scope 不是安全边界
//!
//! Scope 管**可见性与生命周期**，`AuthorityEnvelope` 管**权限**，两轴正交。
//! 子 Run 的 scope 更窄不等于它权限更小——权限收窄必须显式对 Authority 做交集。

use std::sync::Arc;

use agentrs_contracts::ids::ScopeId;
use async_trait::async_trait;
use futures::future::BoxFuture;
use tokio::sync::Mutex;

/// 资源的生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    /// 已创建，尚未启动。
    Pending,
    /// 启动中。
    Starting,
    /// 正常服务。
    Active,
    /// 停止中。**此后拒绝一切新注册**，防止逃逸。
    Stopping,
    /// 已完成清理。
    Disposed,
}

/// 异步清理动作。
///
/// 消费 `Box<Self>`，因此一个 cleanup 天然只能执行一次——这是"清理不可重入"
/// 由类型保证的方式。
#[async_trait]
pub trait AsyncCleanup: Send + Sync {
    /// 执行清理。失败会被汇总但**不阻断其余清理**。
    async fn cleanup(self: Box<Self>) -> Result<(), CleanupError>;
}

/// 清理失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cleanup failed: {reason}")]
pub struct CleanupError {
    /// 已脱敏的失败原因。
    pub reason: String,
}

impl CleanupError {
    /// 构造一个清理错误。
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// 注册失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    /// owner 已进入 `Stopping` 或 `Disposed`。
    ///
    /// **这是逃逸注册的防线**：停止过程中新增的资源不会被本轮清理覆盖，
    /// 允许它注册就等于制造泄漏。
    #[error("owner is stopping or disposed")]
    NotAccepting,
}

/// 一次清理的结果记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupOutcome {
    /// 注册时给的标签，用于诊断。
    pub label: String,
    /// 结果。
    pub result: Result<(), CleanupError>,
}

/// 一次 shutdown 的结算报告。
///
/// **并发多次 shutdown 共享同一份报告**——清理只执行一次。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShutdownReport {
    /// 本 owner 自身的清理结果，**按实际执行顺序**（即注册的反序）。
    pub outcomes: Vec<CleanupOutcome>,
    /// 子 owner 的报告，按子的创建顺序。
    pub children: Vec<ShutdownReport>,
    /// 所属 scope。
    pub scope: Option<ScopeId>,
}

impl ShutdownReport {
    /// 是否完全成功（含全部后代）。
    pub fn is_clean(&self) -> bool {
        self.outcomes.iter().all(|o| o.result.is_ok()) && self.children.iter().all(ShutdownReport::is_clean)
    }

    /// 全部失败的扁平列表，供诊断。
    pub fn failures(&self) -> Vec<&CleanupOutcome> {
        let mut out: Vec<_> = self.outcomes.iter().filter(|o| o.result.is_err()).collect();
        for c in &self.children {
            out.extend(c.failures());
        }
        out
    }

    /// 已执行的清理总数（含后代）。
    pub fn total_cleanups(&self) -> usize {
        self.outcomes.len()
            + self
                .children
                .iter()
                .map(ShutdownReport::total_cleanups)
                .sum::<usize>()
    }
}

struct Registration {
    label: String,
    action: Box<dyn AsyncCleanup>,
}

#[derive(Default)]
struct OwnerState {
    lifecycle: Option<LifecycleState>,
    registrations: Vec<Registration>,
    children: Vec<Arc<ResourceOwner>>,
    settlement: Option<ShutdownReport>,
}

/// 一组 live 资源的唯一属主。
///
/// 不变式（T03A 验收）：
///
/// 1. cleanup 按注册的**反序**执行——后建立的依赖先拆；
/// 2. 停止时**先 cancel/drain 子 owner**，再清理自身；
/// 3. 单个 cleanup 失败被汇总，**不阻断其余**；
/// 4. `Stopping` 之后拒绝新注册；
/// 5. 并发 shutdown 幂等，共享同一份 settlement。
pub struct ResourceOwner {
    scope: ScopeId,
    state: Mutex<OwnerState>,
}

impl ResourceOwner {
    /// 在指定 scope 下创建一个 owner。
    pub fn new(scope: impl Into<ScopeId>) -> Arc<Self> {
        Arc::new(Self {
            scope: scope.into(),
            state: Mutex::new(OwnerState {
                lifecycle: Some(LifecycleState::Active),
                ..Default::default()
            }),
        })
    }

    /// 所属 scope。
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }

    /// 当前生命周期状态。
    pub async fn lifecycle(&self) -> LifecycleState {
        self.state
            .lock()
            .await
            .lifecycle
            .unwrap_or(LifecycleState::Disposed)
    }

    /// 登记一个清理动作。
    ///
    /// **调用点必须在资源对外可见之前**——先登记再暴露，否则中途失败会留下
    /// 无人认领的资源。
    pub async fn register(
        &self,
        label: impl Into<String>,
        action: Box<dyn AsyncCleanup>,
    ) -> Result<(), RegisterError> {
        let mut s = self.state.lock().await;
        match s.lifecycle {
            Some(LifecycleState::Stopping) | Some(LifecycleState::Disposed) | None => {
                Err(RegisterError::NotAccepting)
            }
            _ => {
                s.registrations.push(Registration {
                    label: label.into(),
                    action,
                });
                Ok(())
            }
        }
    }

    /// 派生一个子 owner。父停止时会**先**结算全部子。
    pub async fn child(self: &Arc<Self>, scope: impl Into<ScopeId>) -> Result<Arc<Self>, RegisterError> {
        let child = ResourceOwner::new(scope);
        let mut s = self.state.lock().await;
        match s.lifecycle {
            Some(LifecycleState::Stopping) | Some(LifecycleState::Disposed) | None => {
                Err(RegisterError::NotAccepting)
            }
            _ => {
                s.children.push(child.clone());
                Ok(child)
            }
        }
    }

    /// 当前登记的清理动作数（诊断用）。
    pub async fn registration_count(&self) -> usize {
        self.state.lock().await.registrations.len()
    }

    /// 停止并清理。**幂等**：并发或重复调用共享同一份结算结果。
    ///
    /// 顺序：拒绝新注册 → 结算全部子 → 反序清理自身 → 标记 Disposed。
    ///
    /// 返回 `BoxFuture` 而非 `async fn`：owner 树是递归结构，Rust 的 `async fn`
    /// 不能直接递归。调用方照常 `.await` 即可。
    pub fn shutdown(&self) -> BoxFuture<'_, ShutdownReport> {
        Box::pin(self.shutdown_inner())
    }

    async fn shutdown_inner(&self) -> ShutdownReport {
        // **整个结算过程持锁。**
        //
        // 并发调用者会在锁上等待，醒来时直接读到同一份 settlement——
        // 既保证清理只执行一次，也避免了通知机制的丢失唤醒窗口。
        //
        // 初版试过"取出待清理项后释放锁，后来者轮询/等通知"，两种都不成立：
        // 忙等会在单线程运行时下饿死正在清理的一方；`Notify::notified()`
        // 在首次 poll 前不注册，`notify_waiters()` 会漏掉尚未 await 的等待者。
        //
        // 代价是 shutdown 期间 `register` 会阻塞——但它本就该被拒绝，
        // 先阻塞后拒绝与直接拒绝在语义上等价。
        // 子 owner 各持自己的锁，父持锁调用子的 shutdown 不构成循环等待。
        let mut s = self.state.lock().await;

        if let Some(done) = &s.settlement {
            return done.clone();
        }

        s.lifecycle = Some(LifecycleState::Stopping);
        let children = std::mem::take(&mut s.children);
        let registrations = std::mem::take(&mut s.registrations);

        // 先结算子——父的资源可能是子的依赖，反过来会拆出悬空引用。
        let mut child_reports = Vec::with_capacity(children.len());
        for c in &children {
            child_reports.push(c.shutdown().await);
        }

        // 反序清理自身。单个失败汇总但不中断。
        let mut outcomes = Vec::with_capacity(registrations.len());
        for reg in registrations.into_iter().rev() {
            let result = reg.action.cleanup().await;
            outcomes.push(CleanupOutcome {
                label: reg.label,
                result,
            });
        }

        let report = ShutdownReport {
            outcomes,
            children: child_reports,
            scope: Some(self.scope.clone()),
        };

        s.lifecycle = Some(LifecycleState::Disposed);
        s.settlement = Some(report.clone());
        report
    }
}

/// 部分 setup 失败时的反序回滚（架构 §5.1）。
///
/// 典型场景：一次启动要建立 N 个资源，第 N 步失败时前 N-1 步必须**反序**拆掉。
/// 若正序拆，后建立的资源可能仍持有先建立资源的引用。
pub struct SetupTransaction {
    done: Vec<Registration>,
}

impl Default for SetupTransaction {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupTransaction {
    /// 开启一次 setup 事务。
    pub fn new() -> Self {
        Self { done: Vec::new() }
    }

    /// 记录一个已完成的步骤及其回滚动作。
    pub fn step(&mut self, label: impl Into<String>, undo: Box<dyn AsyncCleanup>) {
        self.done.push(Registration {
            label: label.into(),
            action: undo,
        });
    }

    /// 已完成的步骤数。
    pub fn completed(&self) -> usize {
        self.done.len()
    }

    /// 回滚：反序执行已完成步骤的 undo。失败汇总但不中断。
    pub async fn rollback(self) -> Vec<CleanupOutcome> {
        let mut out = Vec::with_capacity(self.done.len());
        for reg in self.done.into_iter().rev() {
            let result = reg.action.cleanup().await;
            out.push(CleanupOutcome {
                label: reg.label,
                result,
            });
        }
        out
    }

    /// 提交：把已完成步骤的清理动作移交给 owner。
    ///
    /// 提交后这些资源的生命周期归 owner，事务结束。
    pub async fn commit(self, owner: &ResourceOwner) -> Result<(), RegisterError> {
        for reg in self.done {
            owner.register(reg.label, reg.action).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// 记录执行顺序的清理动作。
    struct Recorder {
        log: Arc<Mutex<Vec<String>>>,
        name: String,
        fail: bool,
    }

    #[async_trait]
    impl AsyncCleanup for Recorder {
        async fn cleanup(self: Box<Self>) -> Result<(), CleanupError> {
            self.log.lock().await.push(self.name.clone());
            if self.fail {
                Err(CleanupError::new(format!("{} 故意失败", self.name)))
            } else {
                Ok(())
            }
        }
    }

    fn 动作(log: &Arc<Mutex<Vec<String>>>, name: &str, fail: bool) -> Box<dyn AsyncCleanup> {
        Box::new(Recorder {
            log: log.clone(),
            name: name.to_string(),
            fail,
        })
    }

    #[tokio::test]
    async fn 清理按注册的反序执行() {
        // 后建立的依赖必须先拆，否则会拆出悬空引用。
        let log = Arc::new(Mutex::new(Vec::new()));
        let owner = ResourceOwner::new("run");
        for n in ["a", "b", "c"] {
            owner.register(n, 动作(&log, n, false)).await.unwrap();
        }
        let report = owner.shutdown().await;

        assert_eq!(*log.lock().await, ["c", "b", "a"]);
        assert!(report.is_clean());
        assert_eq!(report.total_cleanups(), 3);
    }

    #[tokio::test]
    async fn 单个清理失败不阻断其余() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let owner = ResourceOwner::new("run");
        owner.register("a", 动作(&log, "a", false)).await.unwrap();
        owner.register("b", 动作(&log, "b", true)).await.unwrap();
        owner.register("c", 动作(&log, "c", false)).await.unwrap();

        let report = owner.shutdown().await;

        assert_eq!(*log.lock().await, ["c", "b", "a"], "b 失败后 a 仍须执行");
        assert!(!report.is_clean());
        assert_eq!(report.failures().len(), 1);
        assert_eq!(report.failures()[0].label, "b");
    }

    #[tokio::test]
    async fn 先结算子再清理自身() {
        // 父的资源可能是子的依赖；反过来拆会留下悬空引用。
        let log = Arc::new(Mutex::new(Vec::new()));
        let parent = ResourceOwner::new("run");
        parent
            .register("父资源", 动作(&log, "父资源", false))
            .await
            .unwrap();

        let child = parent.child("operation").await.unwrap();
        child
            .register("子资源", 动作(&log, "子资源", false))
            .await
            .unwrap();

        parent.shutdown().await;
        assert_eq!(*log.lock().await, ["子资源", "父资源"]);
    }

    #[tokio::test]
    async fn 子的失败不阻断父的清理且被汇总() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let parent = ResourceOwner::new("run");
        parent.register("父", 动作(&log, "父", false)).await.unwrap();
        let child = parent.child("op").await.unwrap();
        child.register("子", 动作(&log, "子", true)).await.unwrap();

        let report = parent.shutdown().await;
        assert_eq!(*log.lock().await, ["子", "父"]);
        assert!(!report.is_clean());
        assert_eq!(report.failures().len(), 1, "子的失败在父报告里可见");
        assert_eq!(report.total_cleanups(), 2);
    }

    #[tokio::test]
    async fn stopping_后拒绝新注册() {
        // 这是逃逸注册的防线：停止中新增的资源不会被本轮清理覆盖。
        let log = Arc::new(Mutex::new(Vec::new()));
        let owner = ResourceOwner::new("run");
        owner.register("a", 动作(&log, "a", false)).await.unwrap();
        owner.shutdown().await;

        assert_eq!(owner.lifecycle().await, LifecycleState::Disposed);
        assert_eq!(
            owner.register("late", 动作(&log, "late", false)).await,
            Err(RegisterError::NotAccepting)
        );
        assert!(owner.child("late-child").await.is_err());
        assert_eq!(*log.lock().await, ["a"], "迟到的注册不得被执行");
    }

    #[tokio::test]
    async fn 重复_shutdown_幂等且共享结算() {
        let count = Arc::new(AtomicUsize::new(0));

        struct Counting(Arc<AtomicUsize>);
        #[async_trait]
        impl AsyncCleanup for Counting {
            async fn cleanup(self: Box<Self>) -> Result<(), CleanupError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let owner = ResourceOwner::new("run");
        owner
            .register("once", Box::new(Counting(count.clone())))
            .await
            .unwrap();

        let a = owner.shutdown().await;
        let b = owner.shutdown().await;
        let c = owner.shutdown().await;

        assert_eq!(count.load(Ordering::SeqCst), 1, "清理只能执行一次");
        assert_eq!(a, b);
        assert_eq!(b, c, "重复调用共享同一份结算");
    }

    /// 并发结算：**确定性门控**，不依赖调度器的轮询顺序。
    ///
    /// 让第一个调用者停在 cleanup 内部，确认第二个调用者此时无法插进来，
    /// 放行后二者拿到同一份结算且清理只跑一次。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 并发_shutdown_共享同一份结算() {
        use tokio::sync::oneshot;

        let count = Arc::new(AtomicUsize::new(0));

        struct Gated {
            count: Arc<AtomicUsize>,
            entered: Option<oneshot::Sender<()>>,
            gate: Option<oneshot::Receiver<()>>,
        }

        #[async_trait]
        impl AsyncCleanup for Gated {
            async fn cleanup(mut self: Box<Self>) -> Result<(), CleanupError> {
                // 通知测试："我已经进入清理并且持有锁"。
                let _ = self.entered.take().unwrap().send(());
                // 停在这里，直到测试放行。
                let _ = self.gate.take().unwrap().await;
                self.count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let (entered_tx, entered_rx) = oneshot::channel();
        let (gate_tx, gate_rx) = oneshot::channel();

        let owner = ResourceOwner::new("run");
        owner
            .register(
                "gated",
                Box::new(Gated {
                    count: count.clone(),
                    entered: Some(entered_tx),
                    gate: Some(gate_rx),
                }),
            )
            .await
            .unwrap();

        let o1 = Arc::clone(&owner);
        let h1 = tokio::spawn(async move { o1.shutdown().await });

        // 等第一个调用者确实进到了清理内部。
        entered_rx.await.unwrap();

        // 此刻第二个调用者开始结算——它必须等待，不能重复清理。
        let o2 = Arc::clone(&owner);
        let h2 = tokio::spawn(async move { o2.shutdown().await });

        // 给 h2 一点时间去争锁，然后放行 h1。
        tokio::task::yield_now().await;
        gate_tx.send(()).unwrap();

        let r1 = h1.await.unwrap();
        let r2 = h2.await.unwrap();

        assert_eq!(count.load(Ordering::SeqCst), 1, "并发调用下清理仍只执行一次");
        assert_eq!(r1, r2, "二者拿到同一份结算");
    }

    #[tokio::test]
    async fn setup_第_n_步失败时反序回滚前_n_减一步() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut tx = SetupTransaction::new();
        tx.step("step1", 动作(&log, "undo1", false));
        tx.step("step2", 动作(&log, "undo2", false));
        // 第 3 步失败，不记录。
        assert_eq!(tx.completed(), 2);

        let out = tx.rollback().await;
        assert_eq!(*log.lock().await, ["undo2", "undo1"], "必须反序");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|o| o.result.is_ok()));
    }

    #[tokio::test]
    async fn setup_回滚中的失败被汇总而不中断() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut tx = SetupTransaction::new();
        tx.step("s1", 动作(&log, "undo1", false));
        tx.step("s2", 动作(&log, "undo2", true));
        tx.step("s3", 动作(&log, "undo3", false));

        let out = tx.rollback().await;
        assert_eq!(*log.lock().await, ["undo3", "undo2", "undo1"]);
        assert_eq!(out.iter().filter(|o| o.result.is_err()).count(), 1);
    }

    #[tokio::test]
    async fn setup_提交后生命周期归_owner() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let owner = ResourceOwner::new("run");
        let mut tx = SetupTransaction::new();
        tx.step("s1", 动作(&log, "undo1", false));
        tx.step("s2", 动作(&log, "undo2", false));
        tx.commit(&owner).await.unwrap();

        assert_eq!(owner.registration_count().await, 2);
        owner.shutdown().await;
        assert_eq!(*log.lock().await, ["undo2", "undo1"], "反序语义在移交后保持");
    }

    #[tokio::test]
    async fn 多层子孙按由内向外结算() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let run = ResourceOwner::new("run");
        run.register("run 级", 动作(&log, "run", false)).await.unwrap();
        let op = run.child("operation").await.unwrap();
        op.register("op 级", 动作(&log, "op", false)).await.unwrap();
        let child_run = op.child("child-run").await.unwrap();
        child_run
            .register("child 级", 动作(&log, "child", false))
            .await
            .unwrap();

        let report = run.shutdown().await;
        assert_eq!(*log.lock().await, ["child", "op", "run"]);
        assert_eq!(report.total_cleanups(), 3);
        assert!(report.is_clean());
    }

    #[tokio::test]
    async fn scope_只标识可见性不参与授权() {
        // 文档性断言：ResourceOwner 上没有任何 authority / capability / grant 字段。
        // Scope 管可见性与生命周期，Authority 管权限，两轴正交（架构 §5.1）。
        let owner = ResourceOwner::new("child-run");
        assert_eq!(owner.scope().as_str(), "child-run");
        // 更窄的 scope 不携带任何权限信息——收窄必须显式对 Authority 做交集。
    }
}

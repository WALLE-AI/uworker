//! Inbox 与 claim（架构 §6.3，任务 T04A）。
//!
//! 采用 inbox/claim 模型而不是简单队列，因为它把"输入何时变成模型可见"
//! 变成一个**有 durable 记录的显式操作**：
//!
//! ```text
//! submit()  -> inbox
//! Turn 开始 -> claim(next-step 输入 + 至多一条排队消息)
//!           -> PreStep 决策（authoritative）
//!                reject          => 被 claim 的批次保持移除，Turn 花 0 个 Step 后关闭
//!                enter(messages) => 写 UserMessage 事件，进入 Step
//! ```
//!
//! **被拒绝的 claim 也要留痕**——用户输入不得在事实流里凭空消失。

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};

use agentrs_contracts::external::ExternalFact;
use agentrs_contracts::ids::ExternalFactId;
use agentrs_types::ContentBlock;
use tokio::sync::Mutex;

/// 进入 inbox 的输入。
#[derive(Debug, Clone, PartialEq)]
pub enum UserInput {
    /// 直接的用户消息。
    Message(Vec<ContentBlock>),
    /// 来自本 Run 之外的事实（团队消息、任务板快照、宿主通知）。
    ///
    /// **只携带内容，不携带能力**——跨 Run 提权路径不存在（内核不变量 14）。
    External(Box<ExternalFact>),
}

/// `submit` 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAccepted {
    /// 已入 inbox，等待下一个安全边界被 claim。
    Queued,
    /// 同一 `fact_id` 已投递过——**幂等丢弃，不是错误**。
    ///
    /// Core 侧的投递重试会走到这里；返回错误会让它误以为需要再试。
    Duplicate,
}

/// `submit` 失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    /// Run 已进入终态。宿主应改为 `start` 一个新 Run。
    #[error("run already terminal")]
    RunAlreadyTerminal,
    /// 队列已满。避免用户狂敲导致无界增长。
    #[error("steering queue full")]
    QueueFull,
}

/// 一次 claim 的产物。
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    /// 被认领的输入，按入队顺序。
    pub inputs: Vec<UserInput>,
}

impl Claim {
    /// 是否为空 claim（inbox 中无待处理输入）。
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }
}

/// PreStep 的裁决。**返回值是权威的**——监听方包装时须保留下游消息，
/// 除非替换是有意的。
#[derive(Debug, Clone, PartialEq)]
pub enum PreStepDecision {
    /// 进入 Step，使用给定的（可能被改写的）消息。
    Enter(Vec<UserInput>),
    /// 拒绝。被 claim 的批次保持移除，Turn 花 0 个 Step 后关闭。
    Reject {
        /// 已脱敏的原因，写入事实流。
        reason: String,
    },
}

/// 输入队列。
pub struct Inbox {
    queue: Mutex<VecDeque<UserInput>>,
    /// 已见过的跨 Run 事实。**投递幂等**：Core 侧的重试不得让同一条消息
    /// 在收件方 Run 里出现两次（架构 §11.3.3 规则 4）。
    seen_facts: Mutex<HashSet<ExternalFactId>>,
    terminal: AtomicBool,
    capacity: usize,
}

impl Inbox {
    /// 新建 inbox。
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            seen_facts: Mutex::new(HashSet::new()),
            terminal: AtomicBool::new(false),
            capacity,
        }
    }

    /// 放入一条输入。**不立刻改变对话**——只在安全边界被 claim。
    pub async fn submit(&self, input: UserInput) -> Result<InputAccepted, SubmitError> {
        if self.terminal.load(Ordering::SeqCst) {
            return Err(SubmitError::RunAlreadyTerminal);
        }
        // 跨 Run 事实按 fact_id 幂等——投递重试不得产生第二条。
        if let UserInput::External(fact) = &input {
            let mut seen = self.seen_facts.lock().await;
            if !seen.insert(fact.fact_id.clone()) {
                return Ok(InputAccepted::Duplicate);
            }
        }

        let mut q = self.queue.lock().await;
        if q.len() >= self.capacity {
            return Err(SubmitError::QueueFull);
        }
        q.push_back(input);
        Ok(InputAccepted::Queued)
    }

    /// 认领至多 `max` 条输入。**这是唯一让输入变成模型可见的通道。**
    ///
    /// 被认领的输入即刻从队列移除——即便随后被 PreStep 拒绝，也不回到队列，
    /// 否则会形成"反复认领同一条被拒输入"的活锁。它的去向由 0-Step Turn 记录。
    pub async fn claim(&self, max: usize) -> Claim {
        let mut q = self.queue.lock().await;
        let n = max.min(q.len());
        Claim {
            inputs: q.drain(..n).collect(),
        }
    }

    /// 队列中待认领的数量。
    pub async fn pending(&self) -> usize {
        self.queue.lock().await.len()
    }

    /// 标记 Run 已终态。此后 `submit` 一律拒绝。
    pub fn mark_terminal(&self) {
        self.terminal.store(true, Ordering::SeqCst);
    }

    /// Run 是否已终态。
    pub fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 消息(t: &str) -> UserInput {
        UserInput::Message(vec![ContentBlock::text(t)])
    }

    #[tokio::test]
    async fn submit_不立刻改变对话只入队() {
        let ib = Inbox::new(8);
        ib.submit(消息("a")).await.unwrap();
        ib.submit(消息("b")).await.unwrap();
        assert_eq!(ib.pending().await, 2, "入队后等待安全边界被 claim");
    }

    #[tokio::test]
    async fn claim_按入队顺序取出并移除() {
        let ib = Inbox::new(8);
        for t in ["a", "b", "c"] {
            ib.submit(消息(t)).await.unwrap();
        }
        let c = ib.claim(2).await;
        assert_eq!(c.inputs, vec![消息("a"), 消息("b")]);
        assert_eq!(ib.pending().await, 1, "已认领的即刻移除");
    }

    #[tokio::test]
    async fn 被拒绝的输入不回到队列() {
        // 否则会形成"反复认领同一条被拒输入"的活锁；
        // 它的去向由 0-Step Turn 在事实流里记录。
        let ib = Inbox::new(8);
        ib.submit(消息("bad")).await.unwrap();
        let c = ib.claim(4).await;
        assert_eq!(c.inputs.len(), 1);

        let _ = PreStepDecision::Reject {
            reason: "示例".into(),
        };
        assert_eq!(ib.pending().await, 0, "拒绝不回滚 claim");
    }

    #[tokio::test]
    async fn 终态后拒绝新输入() {
        let ib = Inbox::new(8);
        ib.mark_terminal();
        assert_eq!(
            ib.submit(消息("late")).await,
            Err(SubmitError::RunAlreadyTerminal)
        );
    }

    #[tokio::test]
    async fn 队列有上限防止无界增长() {
        let ib = Inbox::new(2);
        ib.submit(消息("1")).await.unwrap();
        ib.submit(消息("2")).await.unwrap();
        assert_eq!(ib.submit(消息("3")).await, Err(SubmitError::QueueFull));
    }

    #[tokio::test]
    async fn 空_inbox_的_claim_是空的而非阻塞() {
        let ib = Inbox::new(8);
        assert!(ib.claim(4).await.is_empty());
    }

    #[tokio::test]
    async fn 外部事实与用户消息走同一条通道() {
        // 跨 Run 内容进入 Surface 的唯一通路就是这里（内核不变量 13）。
        use agentrs_contracts::external::{ExternalContent, ExternalFact, FactOrigin};
        let ib = Inbox::new(8);
        ib.submit(UserInput::External(Box::new(ExternalFact {
            fact_id: "f1".into(),
            origin: FactOrigin::TeamMessage {
                team_id: "t".into(),
                from: "m".into(),
            },
            content: ExternalContent::Inline {
                text: "看下 auth".into(),
            },
            causality: None,
        })))
        .await
        .unwrap();
        assert_eq!(ib.pending().await, 1);
    }
}

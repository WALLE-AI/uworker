use agentrs_types::subagent::SubAgentId;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Reserved for persistent Team members.
pub(crate) struct InboxMessage {
    pub(crate) from: SubAgentId,
    pub(crate) content: String,
}

#[allow(dead_code)] // Reserved for persistent Team members.
pub(crate) fn channel(capacity: usize) -> (mpsc::Sender<InboxMessage>, mpsc::Receiver<InboxMessage>) {
    mpsc::channel(capacity.max(1))
}

#[cfg(test)]
#[path = "inbox_test.rs"]
mod inbox_test;

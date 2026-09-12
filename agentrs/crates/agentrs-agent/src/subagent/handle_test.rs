use std::sync::{Arc, RwLock};

use agentrs_types::message::TokenUsage;
use agentrs_types::subagent::{SubAgentId, SubAgentResult, SubAgentStatus};
use tokio_util::sync::CancellationToken;

use super::SubAgentHandle;

fn result(id: SubAgentId) -> SubAgentResult {
    SubAgentResult {
        id,
        name: "child".to_string(),
        text: "done".to_string(),
        usage: TokenUsage::default(),
        turns: 1,
        status: SubAgentStatus::Finished,
    }
}

#[tokio::test]
async fn terminal_status_cannot_be_replaced() {
    let status = RwLock::new(SubAgentStatus::Running);
    assert!(SubAgentHandle::transition_status(&status, SubAgentStatus::Finished));
    assert!(!SubAgentHandle::transition_status(&status, SubAgentStatus::Cancelled));
    assert_eq!(*status.read().unwrap(), SubAgentStatus::Finished);
}

#[tokio::test]
async fn join_handle_can_only_be_consumed_once() {
    let id = SubAgentId::new("child-1");
    let handle = SubAgentHandle::new(
        id.clone(),
        "child".to_string(),
        Arc::new(RwLock::new(SubAgentStatus::Running)),
        CancellationToken::new(),
        tokio::spawn(async move { result(id) }),
    );
    let join = handle.join.lock().await.take().expect("first consumer");
    assert!(handle.join.lock().await.take().is_none());
    assert_eq!(join.await.unwrap().status, SubAgentStatus::Finished);
}

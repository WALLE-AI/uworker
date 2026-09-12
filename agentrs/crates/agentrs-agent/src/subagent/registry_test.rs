use std::sync::{Arc, RwLock};
use std::time::Duration;

use agentrs_types::message::TokenUsage;
use agentrs_types::subagent::{SubAgentId, SubAgentResult, SubAgentStatus};
use tokio_util::sync::CancellationToken;

use super::SubAgentRegistry;
use crate::subagent::handle::SubAgentHandle;

fn handle(index: usize) -> SubAgentHandle {
    let id = SubAgentId::new(format!("child-{index}"));
    let task_id = id.clone();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let join = tokio::spawn(async move {
        task_cancel.cancelled().await;
        SubAgentResult {
            id: task_id,
            name: format!("child-{index}"),
            text: String::new(),
            usage: TokenUsage::default(),
            turns: 0,
            status: SubAgentStatus::Cancelled,
        }
    });
    SubAgentHandle::new(
        id,
        format!("child-{index}"),
        Arc::new(RwLock::new(SubAgentStatus::Running)),
        cancel,
        join,
    )
}

#[tokio::test]
async fn concurrent_registration_and_cancellation_leaves_registry_empty() {
    let registry = Arc::new(SubAgentRegistry::new(4, None, Duration::from_secs(1)));
    let registrations = (0..16).map(|index| {
        let registry = Arc::clone(&registry);
        tokio::spawn(async move { registry.register(handle(index)) })
    });
    for registration in registrations {
        registration.await.unwrap();
    }
    assert_eq!(registry.list().len(), 16);
    registry.cancel_all().await;
    assert!(registry.list().is_empty());
}

#[tokio::test]
async fn wait_records_usage_and_drain_resets_it() {
    let registry = SubAgentRegistry::new(1, Some(10), Duration::from_secs(1));
    let id = SubAgentId::new("usage-child");
    let task_id = id.clone();
    registry.register(SubAgentHandle::new(
        id.clone(),
        "usage".to_string(),
        Arc::new(RwLock::new(SubAgentStatus::Running)),
        CancellationToken::new(),
        tokio::spawn(async move {
            SubAgentResult {
                id: task_id,
                name: "usage".to_string(),
                text: String::new(),
                usage: TokenUsage {
                    input_tokens: 3,
                    output_tokens: 10,
                    cache_creation_tokens: 1,
                    cache_read_tokens: 2,
                },
                turns: 1,
                status: SubAgentStatus::Finished,
            }
        }),
    ));
    registry.wait(&id).await.expect("result");
    assert!(!registry.budget_available());
    assert_eq!(registry.drain_turn_usage().output_tokens, 10);
    assert!(registry.budget_available());
}

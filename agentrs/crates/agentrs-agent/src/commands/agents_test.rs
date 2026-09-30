use std::sync::{Arc, RwLock};
use std::time::Duration;

use agentrs_types::message::TokenUsage;
use agentrs_types::subagent::{SubAgentId, SubAgentResult, SubAgentStatus};
use tokio_util::sync::CancellationToken;

use super::AgentsCommand;
use crate::commands::SlashCommand;
use crate::commands::test_support::Harness;
use crate::subagent::handle::SubAgentHandle;
use crate::subagent::registry::SubAgentRegistry;

/// A registered agent that stays alive until it is cancelled, mirroring
/// `subagent/registry_test.rs`.
fn register(registry: &SubAgentRegistry, id: &str, name: &str, persistent: bool) -> CancellationToken {
    let agent_id = SubAgentId::new(id);
    let task_id = agent_id.clone();
    let task_name = name.to_string();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let join = tokio::spawn(async move {
        task_cancel.cancelled().await;
        SubAgentResult {
            id: task_id,
            name: task_name,
            text: String::new(),
            usage: TokenUsage::default(),
            turns: 0,
            status: SubAgentStatus::Cancelled,
        }
    });
    let status = Arc::new(RwLock::new(SubAgentStatus::Running));
    let handle = if persistent {
        SubAgentHandle::new_persistent(agent_id, name.to_string(), status, cancel.clone(), join)
    } else {
        SubAgentHandle::new(agent_id, name.to_string(), status, cancel.clone(), join)
    };
    registry.register(handle);
    cancel
}

fn registry() -> Arc<SubAgentRegistry> {
    Arc::new(SubAgentRegistry::new(4, None, Duration::from_secs(1)))
}

#[tokio::test]
async fn disabled_subagents_are_reported_as_such() {
    let mut harness = Harness::new();
    let mut ctx = harness.context();
    AgentsCommand.execute(&mut ctx, "").await.unwrap();
    drop(ctx);
    assert!(
        harness.output.errors().contains("Sub-agents are disabled"),
        "got: {}",
        harness.output.errors()
    );
}

#[tokio::test]
async fn empty_registry_explains_where_one_shot_agents_went() {
    let mut harness = Harness::new().with_subagents(registry());
    let mut ctx = harness.context();
    AgentsCommand.execute(&mut ctx, "").await.unwrap();
    drop(ctx);
    let info = harness.output.info();
    assert!(info.contains("No sub-agents are registered"), "got: {info}");
    assert!(
        info.contains("• Agents"),
        "the listing must point at the transcript: {info}"
    );
}

#[tokio::test]
async fn listing_names_each_agent_and_marks_persistent_members() {
    let registry = registry();
    register(&registry, "child-aaaaaaaa-1", "explore", false);
    register(&registry, "child-bbbbbbbb-2", "reviewer", true);
    let mut harness = Harness::new().with_subagents(Arc::clone(&registry));
    let mut ctx = harness.context();
    AgentsCommand.execute(&mut ctx, "").await.unwrap();
    drop(ctx);

    let info = harness.output.info();
    assert!(info.contains("2 sub-agents registered"), "got: {info}");
    assert!(info.contains("explore"), "got: {info}");
    assert!(info.contains("reviewer  running"), "got: {info}");
    assert!(info.contains("persistent"), "got: {info}");
    assert!(info.contains("/agents cancel"), "got: {info}");
}

#[tokio::test]
async fn cancel_without_an_id_asks_for_one() {
    let mut harness = Harness::new().with_subagents(registry());
    let mut ctx = harness.context();
    AgentsCommand.execute(&mut ctx, "cancel").await.unwrap();
    drop(ctx);
    assert!(harness.output.errors().contains("Which sub-agent?"));
}

#[tokio::test]
async fn cancel_reports_an_unknown_id_with_what_is_registered() {
    let registry = registry();
    register(&registry, "child-aaaaaaaa-1", "explore", false);
    let mut harness = Harness::new().with_subagents(registry);
    let mut ctx = harness.context();
    AgentsCommand.execute(&mut ctx, "cancel nope").await.unwrap();
    drop(ctx);
    let errors = harness.output.errors();
    assert!(errors.contains("No sub-agent matches nope"), "got: {errors}");
    assert!(errors.contains("Registered: child-aa"), "got: {errors}");
}

#[tokio::test]
async fn cancel_accepts_a_short_id_and_stops_the_agent() {
    let registry = registry();
    let cancel = register(&registry, "child-aaaaaaaa-1", "explore", false);
    let mut harness = Harness::new().with_subagents(Arc::clone(&registry));
    let mut ctx = harness.context();
    AgentsCommand.execute(&mut ctx, "cancel child-aa").await.unwrap();
    drop(ctx);

    assert!(cancel.is_cancelled(), "the agent's token must be cancelled");
    let info = harness.output.info();
    assert!(info.contains("Cancelled child-aa (explore)"), "got: {info}");
    assert!(
        !info.contains("forced cancel"),
        "a one-shot agent needs no warning: {info}"
    );
}

#[tokio::test]
async fn cancelling_a_persistent_member_warns_about_the_graceful_path() {
    let registry = registry();
    register(&registry, "child-bbbbbbbb-2", "reviewer", true);
    let mut harness = Harness::new().with_subagents(Arc::clone(&registry));
    let mut ctx = harness.context();
    AgentsCommand.execute(&mut ctx, "cancel child-bb").await.unwrap();
    drop(ctx);

    let info = harness.output.info();
    assert!(info.contains("forced cancel"), "got: {info}");
    assert!(info.contains("shutdown_request"), "got: {info}");
    assert!(info.contains("TeamDelete"), "got: {info}");
}

#[tokio::test]
async fn an_ambiguous_prefix_asks_for_a_longer_id() {
    let registry = registry();
    register(&registry, "child-same-1", "one", false);
    register(&registry, "child-same-2", "two", false);
    let mut harness = Harness::new().with_subagents(registry);
    let mut ctx = harness.context();
    AgentsCommand.execute(&mut ctx, "cancel child-sa").await.unwrap();
    drop(ctx);
    assert!(
        harness.output.errors().contains("matches several sub-agents"),
        "got: {}",
        harness.output.errors()
    );
}

#[tokio::test]
async fn an_unknown_subcommand_shows_the_usage() {
    let mut harness = Harness::new().with_subagents(registry());
    let mut ctx = harness.context();
    AgentsCommand.execute(&mut ctx, "restart child-aa").await.unwrap();
    drop(ctx);
    assert!(harness.output.errors().contains("Usage: /agents"));
}

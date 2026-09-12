use std::sync::Arc;

use agentrs_types::subagent::SubAgentId;
use agentrs_types::team::{AgentId, AgentName, InboxMessage, Recipient, TeamError, TeamMessageKind, TeamRuntime};
use chrono::Utc;
use tokio_util::sync::CancellationToken;

use super::InProcessTeamRuntime;
use crate::output::null_sink::NullSink;

fn message() -> InboxMessage {
    let placeholder = agentrs_types::team::TeamId::new("placeholder").unwrap();
    InboxMessage {
        id: "m1".to_string(),
        from: AgentId::team_lead(&placeholder),
        message: "review this".to_string(),
        summary: "review request".to_string(),
        kind: TeamMessageKind::Text,
        sent_at: Utc::now(),
    }
}

fn runtime(temp: &tempfile::TempDir, limit: usize, capacity: usize) -> InProcessTeamRuntime {
    InProcessTeamRuntime::new(temp.path().join("teams"), limit, capacity, Arc::new(NullSink))
}

#[tokio::test]
async fn create_register_send_exit_and_delete_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = runtime(&temp, 8, 16);
    let team = runtime.create_team("Core Team", Some("ship it"), None).await.unwrap();
    let cancel = CancellationToken::new();
    let (alice, alice_id, inbox) = runtime
        .register_member("alice", Some("explore".to_string()), SubAgentId::new("sub-1"), cancel)
        .unwrap();

    let report = runtime.send(Recipient::Named(alice.clone()), message()).await.unwrap();
    assert_eq!(report.delivered, 1);
    let received = inbox.drain();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].from, team.lead_agent_id);
    assert_eq!(alice_id.to_string(), "alice@core-team");

    let error = runtime.delete_team().await.unwrap_err();
    assert!(matches!(error, TeamError::ActiveMembers { .. }));
    runtime.member_exited(&alice);
    let deleted = runtime.delete_team().await.unwrap();
    assert!(deleted.deleted);
    assert!(!team.team_file_path.exists());
}

#[tokio::test]
async fn actor_bound_runtime_stamps_sender_and_broadcast_excludes_it() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = runtime(&temp, 8, 16);
    runtime.create_team("team", None, None).await.unwrap();
    let (alice, _, alice_inbox) = runtime
        .register_member("alice", None, SubAgentId::new("a"), CancellationToken::new())
        .unwrap();
    let (_bob, _, bob_inbox) = runtime
        .register_member("bob", None, SubAgentId::new("b"), CancellationToken::new())
        .unwrap();
    let alice_runtime = runtime.for_member(&alice).unwrap();

    let report = alice_runtime.send(Recipient::Broadcast, message()).await.unwrap();
    assert_eq!(report.delivered, 2);
    assert!(alice_inbox.drain().is_empty());
    let bob_message = bob_inbox.drain().pop().unwrap();
    assert_eq!(bob_message.from.name(), AgentName::new("alice").unwrap());
    assert_eq!(runtime.leader_inbox().drain().len(), 1);
}

#[tokio::test]
async fn duplicate_unknown_full_and_member_limit_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = runtime(&temp, 1, 1);
    runtime.create_team("team", None, None).await.unwrap();
    let (alice, _, _) = runtime
        .register_member("alice", None, SubAgentId::new("a"), CancellationToken::new())
        .unwrap();
    assert!(matches!(
        runtime.register_member("alice", None, SubAgentId::new("a2"), CancellationToken::new()),
        Err(TeamError::DuplicateName { .. })
    ));
    assert!(matches!(
        runtime.register_member("bob", None, SubAgentId::new("b"), CancellationToken::new()),
        Err(TeamError::MemberLimit { limit: 1 })
    ));
    runtime.send(Recipient::Named(alice.clone()), message()).await.unwrap();
    assert!(matches!(
        runtime.send(Recipient::Named(alice), message()).await,
        Err(TeamError::InboxFull { .. })
    ));
    assert!(matches!(
        runtime
            .send(Recipient::Named(AgentName::new("missing").unwrap()), message())
            .await,
        Err(TeamError::UnknownRecipient { .. })
    ));
}

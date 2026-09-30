use std::sync::Arc;

use agentrs_types::subagent::SubAgentId;
use agentrs_types::team::{AgentId, InboxMessage, Recipient, TeamMessageKind, TeamRuntime};
use chrono::Utc;
use tokio_util::sync::CancellationToken;

use super::TeamsCommand;
use crate::commands::SlashCommand;
use crate::commands::test_support::Harness;
use crate::output::null_sink::NullSink;
use crate::team::InProcessTeamRuntime;

fn runtime(temp: &tempfile::TempDir) -> Arc<InProcessTeamRuntime> {
    Arc::new(InProcessTeamRuntime::new(
        temp.path().join("teams"),
        8,
        16,
        Arc::new(NullSink),
    ))
}

fn message(team: &agentrs_types::team::TeamId) -> InboxMessage {
    InboxMessage {
        id: "m1".to_string(),
        from: AgentId::team_lead(team),
        message: "review this".to_string(),
        summary: "review request".to_string(),
        kind: TeamMessageKind::Text,
        sent_at: Utc::now(),
    }
}

async fn run(harness: &mut Harness) -> String {
    let mut ctx = harness.context();
    TeamsCommand.execute(&mut ctx, "").await.unwrap();
    drop(ctx);
    harness.output.info()
}

#[tokio::test]
async fn disabled_team_mode_is_reported_as_such() {
    let mut harness = Harness::new();
    let mut ctx = harness.context();
    TeamsCommand.execute(&mut ctx, "").await.unwrap();
    drop(ctx);
    assert!(
        harness.output.errors().contains("Team mode is disabled"),
        "got: {}",
        harness.output.errors()
    );
}

#[tokio::test]
async fn without_a_team_the_command_says_how_one_is_created() {
    let temp = tempfile::tempdir().unwrap();
    let mut harness = Harness::new().with_team(runtime(&temp));
    let info = run(&mut harness).await;
    assert!(info.contains("No active team"), "got: {info}");
    assert!(info.contains("TeamCreate"), "got: {info}");
}

#[tokio::test]
async fn an_empty_team_shows_its_leader_inbox() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = runtime(&temp);
    runtime.create_team("Core Team", Some("ship it"), None).await.unwrap();
    let mut harness = Harness::new().with_team(Arc::clone(&runtime));

    let info = run(&mut harness).await;
    assert!(info.contains("Team core-team"), "got: {info}");
    assert!(info.contains("ship it"), "got: {info}");
    assert!(info.contains("inbox 0/16"), "got: {info}");
    assert!(info.contains("No members yet."), "got: {info}");
}

#[tokio::test]
async fn members_report_their_type_state_and_inbox_depth() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = runtime(&temp);
    let team = runtime.create_team("team", None, None).await.unwrap();
    let (alice, _, _) = runtime
        .register_member(
            "alice",
            Some("explore".to_string()),
            SubAgentId::new("sub-1"),
            CancellationToken::new(),
        )
        .unwrap();
    let (bob, _, _) = runtime
        .register_member("bob", None, SubAgentId::new("sub-2"), CancellationToken::new())
        .unwrap();
    // One queued message, so a stalled member is distinguishable from an idle one.
    runtime
        .send(Recipient::Named(alice.clone()), message(&team.id))
        .await
        .unwrap();
    runtime.member_exited(&bob);
    let mut harness = Harness::new().with_team(Arc::clone(&runtime));

    let info = run(&mut harness).await;
    assert!(info.contains("2 members"), "got: {info}");
    assert!(info.contains("alice  explore  active  inbox 1/16"), "got: {info}");
    assert!(info.contains("bob  stopped"), "got: {info}");
}

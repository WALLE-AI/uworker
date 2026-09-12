use std::sync::Arc;

use serde_json::json;

use agentrs_protocol::events::ToolCategory;
use agentrs_types::team::{AgentName, DeliveryReport, Recipient, TeamError, TeamMessageKind, TeamRuntime};

use super::SendMessageTool;
use crate::Tool;
use crate::team::test_support::MockTeamRuntime;

#[tokio::test]
async fn rejects_missing_inputs_without_calling_runtime() {
    let runtime = Arc::new(MockTeamRuntime::default());
    let tool = SendMessageTool::new(runtime.clone());
    for input in [
        json!({"message": "hello", "summary": "A useful short message for worker"}),
        json!({"to": "worker", "summary": "A useful short message for worker"}),
        json!({"to": "worker", "message": "hello"}),
    ] {
        assert!(tool.execute(input).await.is_error);
    }
    assert!(runtime.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sends_as_the_current_team_lead() {
    let runtime = Arc::new(MockTeamRuntime::default());
    let tool = SendMessageTool::new(runtime.clone());
    let result = tool
        .execute(json!({
            "to": "worker",
            "message": "Please review the parser.",
            "summary": "Request parser review from worker"
        }))
        .await;
    assert!(!result.is_error);
    let sent = runtime.sent.lock().unwrap();
    assert!(matches!(&sent[0].0, Recipient::Named(name) if name.as_str() == "worker"));
    assert_eq!(sent[0].1.from.as_str(), "team-lead@alpha");
    assert_eq!(sent[0].1.message, "Please review the parser.");
}

#[tokio::test]
async fn broadcast_uses_broadcast_recipient_and_reports_count() {
    let runtime = Arc::new(MockTeamRuntime::default());
    *runtime.send_result.lock().unwrap() = Some(Ok(DeliveryReport {
        delivered: 3,
        recipients: vec![
            AgentName::new("one").unwrap(),
            AgentName::new("two").unwrap(),
            AgentName::new("three").unwrap(),
        ],
        broadcast: true,
    }));
    let tool = SendMessageTool::new(runtime.clone());
    let result = tool
        .execute(json!({
            "to": "*",
            "message": "Status update",
            "summary": "Share current project status with everyone"
        }))
        .await;
    assert!(!result.is_error);
    assert!(result.content.contains("3 teammate(s)"));
    assert!(matches!(runtime.sent.lock().unwrap()[0].0, Recipient::Broadcast));
}

#[tokio::test]
async fn unknown_recipient_error_lists_available_members() {
    let runtime = Arc::new(MockTeamRuntime::default());
    *runtime.send_result.lock().unwrap() = Some(Err(TeamError::UnknownRecipient {
        recipient: "missing".to_string(),
        available: "worker, reviewer".to_string(),
    }));
    let tool = SendMessageTool::new(runtime);
    let result = tool
        .execute(json!({
            "to": "missing",
            "message": "Hello",
            "summary": "Send a short note to teammate"
        }))
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("worker, reviewer"));
}

#[test]
fn exposes_team_deferred_metadata() {
    let runtime: Arc<dyn TeamRuntime> = Arc::new(MockTeamRuntime::default());
    let tool = SendMessageTool::new(runtime);
    assert_eq!(tool.category(), ToolCategory::Team);
    assert!(tool.is_deferred());
    assert_eq!(tool.input_schema()["required"], json!(["to", "message", "summary"]));
}

#[tokio::test]
async fn structured_shutdown_messages_are_validated_and_preserved() {
    let runtime = Arc::new(MockTeamRuntime::default());
    let tool = SendMessageTool::new(runtime.clone());
    let invalid = tool
        .execute(json!({
            "to": "team-lead",
            "message": "no",
            "summary": "shutdown response",
            "type": "shutdown_response",
            "request_id": "request-1"
        }))
        .await;
    assert!(invalid.is_error);

    let result = tool
        .execute(json!({
            "to": "team-lead",
            "message": "approved",
            "summary": "shutdown approved",
            "type": "shutdown_response",
            "request_id": "request-1",
            "approve": true
        }))
        .await;
    assert!(!result.is_error);
    assert!(matches!(
        runtime.sent.lock().unwrap()[0].1.kind,
        TeamMessageKind::ShutdownResponse { approved: true, .. }
    ));
}

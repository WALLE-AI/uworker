use std::sync::Arc;

use agentrs_types::team::{AgentId, AgentName, InboxMessage, TeamId, TeamMessageKind};
use chrono::Utc;

use super::{InboxPushError, TeammateInbox, render_messages};

fn message(body: &str) -> InboxMessage {
    let team = TeamId::new("core").unwrap();
    InboxMessage {
        id: "message-1".to_string(),
        from: AgentId::new(&AgentName::new("alice").unwrap(), &team),
        message: body.to_string(),
        summary: "test message".to_string(),
        kind: TeamMessageKind::Text,
        sent_at: Utc::now(),
    }
}

#[tokio::test]
async fn wait_drains_messages_once_and_escapes_xml() {
    let inbox = Arc::new(TeammateInbox::new(2));
    inbox.push(message("a < b & c")).unwrap();

    let messages = inbox.wait().await;
    assert_eq!(messages.len(), 1);
    assert!(render_messages(&messages).contains("a &lt; b &amp; c"));
    assert!(inbox.drain().is_empty());
}

#[test]
fn bounded_inbox_reports_backpressure_and_close() {
    let inbox = TeammateInbox::new(1);
    inbox.push(message("first")).unwrap();
    assert!(matches!(inbox.push(message("second")), Err(InboxPushError::Full)));
    inbox.close();
    assert!(matches!(inbox.push(message("third")), Err(InboxPushError::Closed)));
}

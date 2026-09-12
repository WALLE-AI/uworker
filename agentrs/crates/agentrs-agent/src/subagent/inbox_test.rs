use agentrs_types::subagent::SubAgentId;

use super::{InboxMessage, channel};

#[tokio::test]
async fn channel_preserves_sender_identity_and_content() {
    let (sender, mut receiver) = channel(0);
    let message = InboxMessage {
        from: SubAgentId::new("child-1"),
        content: "continue with the second task".into(),
    };

    sender.send(message.clone()).await.unwrap();

    assert_eq!(receiver.recv().await, Some(message));
}

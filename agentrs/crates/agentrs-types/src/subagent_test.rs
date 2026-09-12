use super::*;

#[test]
fn terminal_statuses_are_exhaustive() {
    for status in [
        SubAgentStatus::Finished,
        SubAgentStatus::Failed,
        SubAgentStatus::Cancelled,
    ] {
        assert!(status.is_terminal());
    }
    for status in [SubAgentStatus::Pending, SubAgentStatus::Running, SubAgentStatus::Idle] {
        assert!(!status.is_terminal());
    }
}

#[test]
fn only_failure_and_cancellation_are_errors() {
    assert!(SubAgentStatus::Failed.is_error());
    assert!(SubAgentStatus::Cancelled.is_error());
    assert!(!SubAgentStatus::Finished.is_error());
}

#[test]
fn subagent_id_round_trips_through_serde() {
    let id = SubAgentId::new("child-1");
    let encoded = serde_json::to_string(&id).expect("serialize id");
    assert_eq!(
        serde_json::from_str::<SubAgentId>(&encoded).expect("deserialize id"),
        id
    );
}

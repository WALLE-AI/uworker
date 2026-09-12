use std::sync::Arc;

use serde_json::json;

use agentrs_protocol::events::ToolCategory;
use agentrs_types::team::{TeamError, TeamId, TeamRuntime};

use super::TeamCreateTool;
use crate::Tool;
use crate::team::test_support::MockTeamRuntime;

#[tokio::test]
async fn rejects_missing_and_blank_team_names() {
    let runtime: Arc<dyn TeamRuntime> = Arc::new(MockTeamRuntime::default());
    let tool = TeamCreateTool::new(runtime);
    for input in [json!({}), json!({"team_name": "  "})] {
        let result = tool.execute(input).await;
        assert!(result.is_error);
        assert!(result.content.contains("team"));
    }
}

#[tokio::test]
async fn creates_team_and_reports_stable_identifiers() {
    let runtime = Arc::new(MockTeamRuntime::default());
    let tool = TeamCreateTool::new(runtime.clone());
    let result = tool
        .execute(json!({
            "team_name": "Compiler Team",
            "description": "Implement the compiler",
            "agent_type": "general-purpose"
        }))
        .await;
    assert!(!result.is_error);
    assert!(result.content.contains("team_name: compiler-team"));
    assert!(result.content.contains("lead_agent_id: team-lead@compiler-team"));
    assert_eq!(
        runtime.created_with.lock().unwrap().as_ref().unwrap(),
        &(
            "Compiler Team".to_string(),
            Some("Implement the compiler".to_string()),
            Some("general-purpose".to_string())
        )
    );
}

#[tokio::test]
async fn surfaces_already_leading_error() {
    let runtime = Arc::new(MockTeamRuntime::default());
    *runtime.create_error.lock().unwrap() = Some(TeamError::AlreadyLeading {
        team: TeamId::new("existing").unwrap(),
    });
    let tool = TeamCreateTool::new(runtime);
    let result = tool.execute(json!({"team_name": "new"})).await;
    assert!(result.is_error);
    assert!(result.content.contains("Already leading team"));
    assert!(result.content.contains("TeamDelete"));
}

#[test]
fn exposes_team_deferred_metadata() {
    let runtime: Arc<dyn TeamRuntime> = Arc::new(MockTeamRuntime::default());
    let tool = TeamCreateTool::new(runtime);
    assert_eq!(tool.category(), ToolCategory::Team);
    assert!(tool.is_deferred());
    assert_eq!(tool.input_schema()["required"], json!(["team_name"]));
}

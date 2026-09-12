use std::sync::Arc;

use serde_json::json;

use agentrs_protocol::events::ToolCategory;
use agentrs_types::team::{TeamDeleteReport, TeamError, TeamRuntime};

use super::TeamDeleteTool;
use crate::Tool;
use crate::team::test_support::{MockTeamRuntime, team_info};

#[tokio::test]
async fn delete_is_idempotent_when_no_team_exists() {
    let runtime = Arc::new(MockTeamRuntime::default());
    *runtime.team.lock().unwrap() = None;
    let tool = TeamDeleteTool::new(runtime);
    let result = tool.execute(json!({})).await;
    assert!(!result.is_error);
    assert!(result.content.contains("nothing to clean up"));
}

#[tokio::test]
async fn refuses_delete_and_lists_active_members() {
    let runtime = Arc::new(MockTeamRuntime::default());
    *runtime.delete_result.lock().unwrap() = Some(Err(TeamError::ActiveMembers {
        members: "builder, reviewer".to_string(),
    }));
    let tool = TeamDeleteTool::new(runtime);
    let result = tool.execute(json!({})).await;
    assert!(result.is_error);
    assert!(result.content.contains("builder, reviewer"));
}

#[tokio::test]
async fn reports_deleted_team() {
    let runtime = Arc::new(MockTeamRuntime::default());
    *runtime.delete_result.lock().unwrap() = Some(Ok(TeamDeleteReport {
        deleted: true,
        team: Some(team_info("alpha").id),
        stopped_members: Vec::new(),
    }));
    let tool = TeamDeleteTool::new(runtime);
    let result = tool.execute(json!({})).await;
    assert!(!result.is_error);
    assert!(result.content.contains("Deleted team 'alpha'"));
}

#[test]
fn exposes_team_deferred_metadata() {
    let runtime: Arc<dyn TeamRuntime> = Arc::new(MockTeamRuntime::default());
    let tool = TeamDeleteTool::new(runtime);
    assert_eq!(tool.category(), ToolCategory::Team);
    assert!(tool.is_deferred());
}

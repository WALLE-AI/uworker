use agentrs_types::team::{AgentId, AgentName, TeamFile, TeamId, TeamInfo, TeamMember};
use chrono::Utc;

use super::TeamStore;

#[test]
fn team_file_round_trips_and_delete_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let store = TeamStore::new(temp.path().join("teams"));
    let id = TeamId::new("My Team!").unwrap();
    let lead = AgentName::team_lead();
    let file = TeamFile {
        team: TeamInfo {
            id: id.clone(),
            description: Some("purpose".to_string()),
            agent_type: None,
            team_file_path: store.config_path(&id),
            lead_agent_id: AgentId::team_lead(&id),
            created_at: Utc::now(),
        },
        members: vec![TeamMember {
            name: lead.clone(),
            agent_id: AgentId::new(&lead, &id),
            agent_type: None,
            color: "default".to_string(),
            is_active: true,
        }],
    };

    assert!(store.read(&id).unwrap().is_none());
    store.write(&file).unwrap();
    assert_eq!(store.read(&id).unwrap(), Some(file));
    store.delete(&id).unwrap();
    store.delete(&id).unwrap();
    assert!(store.read(&id).unwrap().is_none());
}

#[test]
fn sanitized_team_path_stays_under_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("teams");
    let store = TeamStore::new(root.clone());
    let id = TeamId::new("../../outside").unwrap();
    assert!(store.config_path(&id).starts_with(root));
}

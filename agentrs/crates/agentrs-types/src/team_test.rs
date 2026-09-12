use std::path::PathBuf;
use std::str::FromStr;

use chrono::Utc;

use super::{AgentId, AgentName, TeamFile, TeamId, TeamInfo, TeamMember};

#[test]
fn names_are_sanitized_without_path_or_id_separators() {
    assert_eq!(TeamId::new("My Team!").unwrap().as_str(), "my-team-");
    assert_eq!(TeamId::new("../../etc").unwrap().as_str(), "------etc");
    assert_eq!(AgentName::new("api@worker").unwrap().as_str(), "api-worker");
    assert!(TeamId::new("  ").is_err());
}

#[test]
fn deserialization_cannot_bypass_identifier_validation() {
    assert!(serde_json::from_str::<TeamId>(r#""""#).is_err());
    assert_eq!(
        serde_json::from_str::<AgentName>(r#""api@worker""#).unwrap().as_str(),
        "api-worker"
    );
    assert!(serde_json::from_str::<AgentId>(r#""missing-team""#).is_err());
}

#[test]
fn agent_id_round_trips_as_name_at_team() {
    let id = AgentId::new(&AgentName::new("Builder").unwrap(), &TeamId::new("Core Team").unwrap());
    assert_eq!(id.as_str(), "builder@core-team");
    assert_eq!(AgentId::from_str(id.as_str()).unwrap(), id);
    assert!(AgentId::from_str("missing-team").is_err());
    assert!(AgentId::from_str("a@b@c").is_err());
}

#[test]
fn team_file_json_round_trip_preserves_fields() {
    let id = TeamId::new("compiler").unwrap();
    let lead = AgentId::team_lead(&id);
    let file = TeamFile {
        team: TeamInfo {
            id: id.clone(),
            description: Some("Compiler work".to_string()),
            agent_type: Some("general-purpose".to_string()),
            team_file_path: PathBuf::from("teams").join("compiler").join("config.json"),
            lead_agent_id: lead.clone(),
            created_at: Utc::now(),
        },
        members: vec![TeamMember {
            name: AgentName::team_lead(),
            agent_id: lead,
            agent_type: None,
            color: "blue".to_string(),
            is_active: true,
        }],
    };
    let json = serde_json::to_string(&file).unwrap();
    assert_eq!(serde_json::from_str::<TeamFile>(&json).unwrap(), file);
}

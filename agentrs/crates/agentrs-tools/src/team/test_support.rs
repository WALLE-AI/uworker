use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;

use agentrs_types::team::{
    AgentId, AgentName, DeliveryReport, InboxMessage, Recipient, TeamDeleteReport, TeamError, TeamId, TeamInfo,
    TeamMember, TeamRuntime,
};

pub(super) type CreateArguments = (String, Option<String>, Option<String>);

pub(super) struct MockTeamRuntime {
    pub(super) team: Mutex<Option<TeamInfo>>,
    pub(super) members: Mutex<Vec<TeamMember>>,
    pub(super) create_error: Mutex<Option<TeamError>>,
    pub(super) delete_result: Mutex<Option<Result<TeamDeleteReport, TeamError>>>,
    pub(super) send_result: Mutex<Option<Result<DeliveryReport, TeamError>>>,
    pub(super) created_with: Mutex<Option<CreateArguments>>,
    pub(super) sent: Mutex<Vec<(Recipient, InboxMessage)>>,
}

impl Default for MockTeamRuntime {
    fn default() -> Self {
        Self {
            team: Mutex::new(Some(team_info("alpha"))),
            members: Mutex::new(Vec::new()),
            create_error: Mutex::new(None),
            delete_result: Mutex::new(None),
            send_result: Mutex::new(None),
            created_with: Mutex::new(None),
            sent: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl TeamRuntime for MockTeamRuntime {
    async fn create_team(
        &self,
        name: &str,
        description: Option<&str>,
        agent_type: Option<&str>,
    ) -> Result<TeamInfo, TeamError> {
        *self.created_with.lock().unwrap() = Some((
            name.to_string(),
            description.map(str::to_string),
            agent_type.map(str::to_string),
        ));
        if let Some(error) = self.create_error.lock().unwrap().take() {
            return Err(error);
        }
        Ok(team_info(name))
    }

    async fn delete_team(&self) -> Result<TeamDeleteReport, TeamError> {
        if let Some(result) = self.delete_result.lock().unwrap().take() {
            return result;
        }
        let team = self.team.lock().unwrap().take().map(|team| team.id);
        Ok(TeamDeleteReport {
            deleted: team.is_some(),
            team,
            stopped_members: Vec::new(),
        })
    }

    async fn send(&self, to: Recipient, message: InboxMessage) -> Result<DeliveryReport, TeamError> {
        self.sent.lock().unwrap().push((to.clone(), message));
        if let Some(result) = self.send_result.lock().unwrap().take() {
            return result;
        }
        Ok(DeliveryReport {
            delivered: 1,
            recipients: vec![AgentName::new("worker").unwrap()],
            broadcast: matches!(to, Recipient::Broadcast),
        })
    }

    fn current_team(&self) -> Option<TeamInfo> {
        self.team.lock().unwrap().clone()
    }

    fn members(&self) -> Vec<TeamMember> {
        self.members.lock().unwrap().clone()
    }
}

pub(super) fn team_info(name: &str) -> TeamInfo {
    let id = TeamId::new(name).unwrap();
    TeamInfo {
        lead_agent_id: AgentId::team_lead(&id),
        team_file_path: PathBuf::from("teams").join(id.as_str()).join("config.json"),
        id,
        description: None,
        agent_type: None,
        created_at: Utc::now(),
    }
}

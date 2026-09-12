use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

pub const TEAM_LEAD_NAME: &str = "team-lead";

fn sanitize_name(raw: &str, kind: &'static str) -> Result<String, TeamError> {
    if raw.trim().is_empty() {
        return Err(TeamError::InvalidName {
            kind,
            reason: "name cannot be empty".to_string(),
        });
    }
    let sanitized = raw
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    Ok(sanitized)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TeamId(String);

impl TeamId {
    pub fn new(raw: impl AsRef<str>) -> Result<Self, TeamError> {
        sanitize_name(raw.as_ref(), "team").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TeamId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for TeamId {
    type Err = TeamError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for TeamId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct AgentName(String);

impl AgentName {
    pub fn new(raw: impl AsRef<str>) -> Result<Self, TeamError> {
        sanitize_name(raw.as_ref(), "agent").map(Self)
    }

    pub fn team_lead() -> Self {
        Self(TEAM_LEAD_NAME.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for AgentName {
    type Err = TeamError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for AgentName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(name: &AgentName, team: &TeamId) -> Self {
        Self(format!("{name}@{team}"))
    }

    pub fn team_lead(team: &TeamId) -> Self {
        Self::new(&AgentName::team_lead(), team)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn name(&self) -> AgentName {
        let name = self.0.split_once('@').map_or(self.0.as_str(), |(name, _)| name);
        AgentName(name.to_string())
    }

    pub fn team(&self) -> TeamId {
        let team = self.0.split_once('@').map_or("", |(_, team)| team);
        TeamId(team.to_string())
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for AgentId {
    type Err = TeamError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((name, team)) = value.split_once('@') else {
            return Err(TeamError::InvalidName {
                kind: "agent id",
                reason: "expected name@team".to_string(),
            });
        };
        if team.contains('@') {
            return Err(TeamError::InvalidName {
                kind: "agent id",
                reason: "expected exactly one @ separator".to_string(),
            });
        }
        Ok(Self::new(&AgentName::new(name)?, &TeamId::new(team)?))
    }
}

impl<'de> Deserialize<'de> for AgentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamInfo {
    pub id: TeamId,
    pub description: Option<String>,
    pub agent_type: Option<String>,
    pub team_file_path: PathBuf,
    pub lead_agent_id: AgentId,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMember {
    pub name: AgentName,
    pub agent_id: AgentId,
    pub agent_type: Option<String>,
    pub color: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamFile {
    pub team: TeamInfo,
    pub members: Vec<TeamMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamMessageKind {
    Text,
    ShutdownRequest { request_id: String },
    ShutdownResponse { request_id: String, approved: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxMessage {
    pub id: String,
    pub from: AgentId,
    pub message: String,
    pub summary: String,
    pub kind: TeamMessageKind,
    pub sent_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    Named(AgentName),
    Broadcast,
}

impl Recipient {
    pub fn parse(raw: &str) -> Result<Self, TeamError> {
        if raw == "*" {
            Ok(Self::Broadcast)
        } else {
            AgentName::new(raw).map(Self::Named)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReport {
    pub delivered: usize,
    pub recipients: Vec<AgentName>,
    pub broadcast: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamDeleteReport {
    pub deleted: bool,
    pub team: Option<TeamId>,
    pub stopped_members: Vec<AgentName>,
}

#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error("invalid {kind} name: {reason}")]
    InvalidName { kind: &'static str, reason: String },
    #[error("Already leading team '{team}'; call TeamDelete before creating another team")]
    AlreadyLeading { team: TeamId },
    #[error("no active team")]
    NoCurrentTeam,
    #[error("team member name '{name}' is already in use")]
    DuplicateName { name: AgentName },
    #[error("unknown recipient '{recipient}'; available members: {available}")]
    UnknownRecipient { recipient: String, available: String },
    #[error("recipient '{recipient}' is no longer running")]
    RecipientGone { recipient: AgentName },
    #[error("recipient '{recipient}' inbox is full")]
    InboxFull { recipient: AgentName },
    #[error("team member limit reached ({limit})")]
    MemberLimit { limit: usize },
    #[error(
        "active team members must shut down first: {members}; send each member a shutdown_request before TeamDelete"
    )]
    ActiveMembers { members: String },
    #[error("team storage operation failed: {reason}")]
    Storage { reason: String },
    #[error("team runtime operation failed: {reason}")]
    Runtime { reason: String },
}

#[async_trait]
pub trait TeamRuntime: Send + Sync {
    async fn create_team(
        &self,
        name: &str,
        description: Option<&str>,
        agent_type: Option<&str>,
    ) -> Result<TeamInfo, TeamError>;

    async fn delete_team(&self) -> Result<TeamDeleteReport, TeamError>;

    async fn send(&self, to: Recipient, message: InboxMessage) -> Result<DeliveryReport, TeamError>;

    fn current_team(&self) -> Option<TeamInfo>;

    fn members(&self) -> Vec<TeamMember>;
}

#[cfg(test)]
#[path = "team_test.rs"]
mod team_test;

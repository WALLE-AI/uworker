use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, Weak};

use agentrs_protocol::events::TeamEvent;
use agentrs_types::subagent::SubAgentId;
use agentrs_types::team::{
    AgentId, AgentName, DeliveryReport, InboxMessage, Recipient, TeamDeleteReport, TeamError, TeamFile, TeamId,
    TeamInfo, TeamMember, TeamMessageKind, TeamRuntime,
};
use async_trait::async_trait;
use chrono::Utc;
use tokio_util::sync::CancellationToken;

use super::inbox::{InboxPushError, TeammateInbox};
use super::store::TeamStore;
use crate::output::OutputSink;
use crate::subagent::registry::SubAgentRegistry;

const COLORS: &[&str] = &[
    "blue",
    "green",
    "yellow",
    "magenta",
    "cyan",
    "red",
    "white",
    "bright_blue",
];

struct MemberState {
    member: TeamMember,
    subagent_id: Option<SubAgentId>,
    inbox: Arc<TeammateInbox>,
    cancel: Option<CancellationToken>,
}

#[derive(Default)]
struct RuntimeState {
    current: Option<TeamInfo>,
    members: HashMap<AgentName, MemberState>,
}

struct TeamCore {
    state: RwLock<RuntimeState>,
    store: TeamStore,
    max_members: usize,
    inbox_capacity: usize,
    leader_inbox: Arc<TeammateInbox>,
    registry: RwLock<Weak<SubAgentRegistry>>,
    output: Arc<dyn OutputSink>,
}

impl Drop for TeamCore {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner());
        for member in state.members.values() {
            if let Some(cancel) = &member.cancel {
                cancel.cancel();
            }
            member.inbox.close();
        }
        self.leader_inbox.close();
        if let Some(team) = &state.current
            && let Err(error) = self.store.delete(&team.id)
        {
            tracing::warn!(target: "agentrs_agent", %error, team = %team.id, "unable to clean team storage during shutdown");
        }
    }
}

#[derive(Clone)]
pub(crate) struct InProcessTeamRuntime {
    core: Arc<TeamCore>,
    sender: Option<AgentId>,
}

impl InProcessTeamRuntime {
    pub(crate) fn new(root: PathBuf, max_members: usize, inbox_capacity: usize, output: Arc<dyn OutputSink>) -> Self {
        Self {
            core: Arc::new(TeamCore {
                state: RwLock::new(RuntimeState::default()),
                store: TeamStore::new(root),
                max_members: max_members.max(1),
                inbox_capacity: inbox_capacity.max(1),
                leader_inbox: Arc::new(TeammateInbox::new(inbox_capacity)),
                registry: RwLock::new(Weak::new()),
                output,
            }),
            sender: None,
        }
    }

    pub(crate) fn attach_registry(&self, registry: &Arc<SubAgentRegistry>) {
        *self
            .core
            .registry
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(registry);
    }

    pub(crate) fn leader_inbox(&self) -> Arc<TeammateInbox> {
        Arc::clone(&self.core.leader_inbox)
    }

    pub(crate) fn for_member(&self, name: &AgentName) -> Result<Self, TeamError> {
        let state = self.core.state.read().unwrap_or_else(|poisoned| poisoned.into_inner());
        let member = state.members.get(name).ok_or_else(|| TeamError::UnknownRecipient {
            recipient: name.to_string(),
            available: available_members(&state),
        })?;
        Ok(Self {
            core: Arc::clone(&self.core),
            sender: Some(member.member.agent_id.clone()),
        })
    }

    pub(crate) fn register_member(
        &self,
        raw_name: &str,
        agent_type: Option<String>,
        subagent_id: SubAgentId,
        cancel: CancellationToken,
    ) -> Result<(AgentName, AgentId, Arc<TeammateInbox>), TeamError> {
        let name = AgentName::new(raw_name)?;
        let mut state = self.core.state.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        let team = state.current.clone().ok_or(TeamError::NoCurrentTeam)?;
        if state.members.contains_key(&name) {
            return Err(TeamError::DuplicateName { name });
        }
        if state.members.len().saturating_sub(1) >= self.core.max_members {
            return Err(TeamError::MemberLimit {
                limit: self.core.max_members,
            });
        }
        let agent_id = AgentId::new(&name, &team.id);
        let inbox = Arc::new(TeammateInbox::new(self.core.inbox_capacity));
        let member = TeamMember {
            name: name.clone(),
            agent_id: agent_id.clone(),
            agent_type,
            color: COLORS[(state.members.len().saturating_sub(1)) % COLORS.len()].to_string(),
            is_active: true,
        };
        state.members.insert(
            name.clone(),
            MemberState {
                member,
                subagent_id: Some(subagent_id),
                inbox: Arc::clone(&inbox),
                cancel: Some(cancel),
            },
        );
        persist(&self.core, &state)?;
        tracing::info!(target: "agentrs_agent", team = %team.id, member = %name, "team member joined");
        self.core.output.emit_team_event(TeamEvent::MemberJoined {
            team_name: team.id.to_string(),
            member_name: name.to_string(),
            agent_id: agent_id.to_string(),
        });
        Ok((name, agent_id, inbox))
    }

    pub(crate) fn member_exited(&self, name: &AgentName) {
        let mut state = self.core.state.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        let event = state.current.as_ref().and_then(|team| {
            state.members.get(name).map(|member| TeamEvent::MemberExited {
                team_name: team.id.to_string(),
                member_name: name.to_string(),
                agent_id: member.member.agent_id.to_string(),
            })
        });
        if let Some(member) = state.members.get_mut(name) {
            member.member.is_active = false;
            member.inbox.close();
        }
        if let Err(error) = persist(&self.core, &state) {
            tracing::warn!(target: "agentrs_agent", %error, member = %name, "unable to persist exited team member");
        }
        drop(state);
        if let Some(event) = event {
            self.core.output.emit_team_event(event);
        }
    }

    pub(crate) fn inbox_for(&self, name: &AgentName) -> Option<Arc<TeammateInbox>> {
        self.core
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .members
            .get(name)
            .map(|member| Arc::clone(&member.inbox))
    }

    pub(crate) fn sender_id(&self) -> Option<AgentId> {
        self.sender
            .clone()
            .or_else(|| self.current_team().map(|team| team.lead_agent_id))
    }
}

#[async_trait]
impl TeamRuntime for InProcessTeamRuntime {
    async fn create_team(
        &self,
        name: &str,
        description: Option<&str>,
        agent_type: Option<&str>,
    ) -> Result<TeamInfo, TeamError> {
        let mut state = self.core.state.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(team) = &state.current {
            return Err(TeamError::AlreadyLeading { team: team.id.clone() });
        }
        let base = TeamId::new(name)?;
        let mut id = base.clone();
        let mut suffix = 2;
        while self.core.store.read(&id)?.is_some() {
            id = TeamId::new(format!("{}-{suffix}", base.as_str()))?;
            suffix += 1;
        }
        let info = TeamInfo {
            id: id.clone(),
            description: description.map(str::to_string),
            agent_type: agent_type.map(str::to_string),
            team_file_path: self.core.store.config_path(&id),
            lead_agent_id: AgentId::team_lead(&id),
            created_at: Utc::now(),
        };
        let lead = TeamMember {
            name: AgentName::team_lead(),
            agent_id: info.lead_agent_id.clone(),
            agent_type: None,
            color: "default".to_string(),
            is_active: true,
        };
        state.current = Some(info.clone());
        state.members.insert(
            lead.name.clone(),
            MemberState {
                member: lead,
                subagent_id: None,
                inbox: Arc::clone(&self.core.leader_inbox),
                cancel: None,
            },
        );
        persist(&self.core, &state)?;
        tracing::info!(target: "agentrs_agent", team = %id, "team created");
        Ok(info)
    }

    async fn delete_team(&self) -> Result<TeamDeleteReport, TeamError> {
        let (team, members) = {
            let state = self.core.state.read().unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(team) = state.current.clone() else {
                return Ok(TeamDeleteReport {
                    deleted: false,
                    team: None,
                    stopped_members: Vec::new(),
                });
            };
            let active = state
                .members
                .values()
                .filter(|member| member.member.name != AgentName::team_lead() && member.member.is_active)
                .map(|member| member.member.name.to_string())
                .collect::<Vec<_>>();
            if !active.is_empty() {
                return Err(TeamError::ActiveMembers {
                    members: active.join(", "),
                });
            }
            let members = state
                .members
                .values()
                .filter_map(|member| member.subagent_id.clone().map(|id| (member.member.name.clone(), id)))
                .collect::<Vec<_>>();
            (team, members)
        };
        let registry = self
            .core
            .registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .upgrade();
        if let Some(registry) = registry {
            for (_, id) in &members {
                let _ = registry.wait(id).await;
            }
        }
        self.core.store.delete(&team.id)?;
        let mut state = self.core.state.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        for member in state.members.values() {
            if member.member.name != AgentName::team_lead() {
                member.inbox.close();
            }
        }
        state.current = None;
        state.members.clear();
        tracing::info!(target: "agentrs_agent", team = %team.id, "team deleted");
        Ok(TeamDeleteReport {
            deleted: true,
            team: Some(team.id),
            stopped_members: members.into_iter().map(|(name, _)| name).collect(),
        })
    }

    async fn send(&self, to: Recipient, mut message: InboxMessage) -> Result<DeliveryReport, TeamError> {
        let sender = self.sender_id().ok_or(TeamError::NoCurrentTeam)?;
        message.from = sender.clone();
        let mut state = self.core.state.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        let team = state.current.clone().ok_or(TeamError::NoCurrentTeam)?;
        let broadcast = matches!(to, Recipient::Broadcast);
        let recipients = match to {
            Recipient::Named(name) => vec![name],
            Recipient::Broadcast => state
                .members
                .keys()
                .filter(|name| **name != sender.name())
                .cloned()
                .collect(),
        };
        for recipient in &recipients {
            let Some(member) = state.members.get(recipient) else {
                return Err(TeamError::UnknownRecipient {
                    recipient: recipient.to_string(),
                    available: available_members(&state),
                });
            };
            if !member.member.is_active {
                return Err(TeamError::RecipientGone {
                    recipient: recipient.clone(),
                });
            }
            member.inbox.push(message.clone()).map_err(|error| match error {
                InboxPushError::Full => TeamError::InboxFull {
                    recipient: recipient.clone(),
                },
                InboxPushError::Closed => TeamError::RecipientGone {
                    recipient: recipient.clone(),
                },
            })?;
            self.core.store.append_inbox(&team.id, recipient.as_str(), &message)?;
        }
        if let TeamMessageKind::ShutdownResponse { approved: true, .. } = message.kind
            && let Some(member) = state.members.get_mut(&sender.name())
        {
            if let Some(cancel) = &member.cancel {
                cancel.cancel();
            }
            member.member.is_active = false;
        }
        persist(&self.core, &state)?;
        let event_recipient = if broadcast {
            "*".to_string()
        } else {
            recipients.first().map(ToString::to_string).unwrap_or_default()
        };
        self.core.output.emit_team_event(TeamEvent::MessageSent {
            team_name: team.id.to_string(),
            from: sender.to_string(),
            to: event_recipient,
        });
        tracing::debug!(target: "agentrs_agent", team = %team.id, from = %sender, delivered = recipients.len(), broadcast, "team message delivered");
        Ok(DeliveryReport {
            delivered: recipients.len(),
            recipients,
            broadcast,
        })
    }

    fn current_team(&self) -> Option<TeamInfo> {
        self.core
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current
            .clone()
    }

    fn members(&self) -> Vec<TeamMember> {
        self.core
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .members
            .values()
            .map(|member| member.member.clone())
            .collect()
    }
}

fn persist(core: &TeamCore, state: &RuntimeState) -> Result<(), TeamError> {
    let Some(team) = state.current.clone() else {
        return Ok(());
    };
    let mut members = state
        .members
        .values()
        .map(|member| member.member.clone())
        .collect::<Vec<_>>();
    members.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    core.store.write(&TeamFile { team, members })
}

fn available_members(state: &RuntimeState) -> String {
    let mut names = state.members.keys().map(ToString::to_string).collect::<Vec<_>>();
    names.sort();
    names.join(", ")
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod runtime_test;

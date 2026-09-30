use std::fmt::Write;

use agentrs_types::team::TeamRuntime;
use async_trait::async_trait;

use super::{CommandContext, CommandResult, SlashCommand};
use crate::team::InProcessTeamRuntime;

const NO_TEAM: &str =
    "No active team. The model creates one with TeamCreate, then adds members with a persistent Spawn.";

pub(super) struct TeamsCommand;

#[async_trait]
impl SlashCommand for TeamsCommand {
    fn name(&self) -> &str {
        "teams"
    }

    fn description(&self) -> &str {
        "Show the active team, its members, and their inboxes"
    }

    async fn execute(&self, ctx: &mut CommandContext<'_>, _args: &str) -> anyhow::Result<CommandResult> {
        let Some(runtime) = ctx.team_runtime else {
            ctx.output
                .emit_error("Team mode is disabled for this session; enable [team] in the config to use it.");
            return Ok(CommandResult::Continue);
        };

        ctx.output.emit_info(&describe_team(runtime));
        Ok(CommandResult::Continue)
    }
}

fn describe_team(runtime: &InProcessTeamRuntime) -> String {
    let Some(team) = runtime.current_team() else {
        return NO_TEAM.to_string();
    };

    let mut output = format!("Team {} · created {}", team.id, team.created_at.to_rfc3339());
    if let Some(description) = &team.description {
        let _ = write!(&mut output, "\n{description}");
    }
    let leader = runtime.leader_inbox();
    let _ = write!(
        &mut output,
        "\nLeader {} · inbox {}/{}",
        team.lead_agent_id,
        leader.depth(),
        leader.capacity()
    );

    // The leader occupies a member slot in the runtime's own bookkeeping, but
    // it already has its own line above; listing it twice would also make the
    // count disagree with the team's `max_members` limit.
    let members = runtime
        .members()
        .into_iter()
        .filter(|member| member.agent_id != team.lead_agent_id)
        .collect::<Vec<_>>();
    if members.is_empty() {
        output.push_str("\nNo members yet.");
        return output;
    }

    let _ = write!(&mut output, "\n{} members", members.len());
    for member in &members {
        // Inbox figures come from the runtime rather than the persisted team
        // file: a queue depth is live state, and a closed inbox has none.
        let inbox = runtime
            .inbox_for(&member.name)
            .map(|inbox| format!("inbox {}/{}", inbox.depth(), inbox.capacity()))
            .unwrap_or_else(|| "inbox gone".to_string());
        let agent_type = member
            .agent_type
            .as_deref()
            .map(|agent_type| format!("  {agent_type}"))
            .unwrap_or_default();
        let state = if member.is_active { "active" } else { "stopped" };
        let _ = write!(&mut output, "\n  {}{agent_type}  {state}  {inbox}", member.name);
    }
    output
}

#[cfg(test)]
#[path = "teams_test.rs"]
mod teams_test;

use std::fmt::Write;
use std::time::Duration;

use agentrs_types::subagent::SubAgentStatus;
use async_trait::async_trait;

use super::{CommandContext, CommandResult, SlashCommand};
use crate::subagent::registry::{SubAgentRegistry, SubAgentSnapshot};

const USAGE: &str = "Usage: /agents | /agents cancel <id>";

/// Explains an empty listing rather than leaving the user to guess.
///
/// Commands run between turns, and a one-shot sub-agent is awaited inside the
/// turn that spawned it — so by the time this command can run, only persistent
/// members are still registered.
const NOTHING_LIVE: &str = "No sub-agents are registered right now.\n\
One-shot sub-agents finish inside the turn that spawned them; \
see the `• Agents` block in the transcript for those. \
Persistent team members stay listed here until they stop.";

/// Characters of an id shown in the listing. Ids are unique well inside this
/// prefix, and `cancel` accepts the prefix back.
const SHORT_ID: usize = 8;

pub(super) struct AgentsCommand;

#[async_trait]
impl SlashCommand for AgentsCommand {
    fn name(&self) -> &str {
        "agents"
    }

    fn description(&self) -> &str {
        "List live sub-agents, or cancel one"
    }

    async fn execute(&self, ctx: &mut CommandContext<'_>, args: &str) -> anyhow::Result<CommandResult> {
        let Some(registry) = ctx.subagent_registry else {
            ctx.output
                .emit_error("Sub-agents are disabled for this session; there is nothing to show.");
            return Ok(CommandResult::Continue);
        };

        let (subcommand, argument) = split_subcommand(args);
        match subcommand {
            "" => ctx.output.emit_info(&list_agents(registry)),
            "cancel" => match cancel_agent(registry, argument) {
                Ok(message) => ctx.output.emit_info(&message),
                Err(message) => ctx.output.emit_error(&message),
            },
            other => ctx
                .output
                .emit_error(&format!("Unknown /agents subcommand: {other}. {USAGE}")),
        }
        Ok(CommandResult::Continue)
    }
}

fn split_subcommand(args: &str) -> (&str, &str) {
    let trimmed = args.trim();
    match trimmed.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (trimmed, ""),
    }
}

fn list_agents(registry: &SubAgentRegistry) -> String {
    let mut snapshots = registry.list();
    if snapshots.is_empty() {
        return NOTHING_LIVE.to_string();
    }
    // Oldest first, so a listing read twice keeps the same order even as
    // agents come and go.
    snapshots.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.name.cmp(&right.name))
    });

    let mut output = format!("{} sub-agents registered\n", snapshots.len());
    for snapshot in &snapshots {
        writeln!(&mut output, "  {}", format_snapshot(snapshot)).expect("writing to String cannot fail");
    }
    output.push_str("\nRun /agents cancel <id> to stop one.");
    output
}

fn format_snapshot(snapshot: &SubAgentSnapshot) -> String {
    let kind = if snapshot.persistent { "  persistent" } else { "" };
    format!(
        "{}  {}  {}  {}{kind}",
        short_id(snapshot.id.as_str()),
        snapshot.name,
        status_label(snapshot.status),
        format_elapsed(snapshot.started_at.elapsed()),
    )
}

fn status_label(status: SubAgentStatus) -> &'static str {
    match status {
        SubAgentStatus::Pending => "pending",
        SubAgentStatus::Running => "running",
        SubAgentStatus::Idle => "idle",
        SubAgentStatus::Finished => "done",
        SubAgentStatus::Failed => "failed",
        SubAgentStatus::Cancelled => "cancelled",
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(SHORT_ID).collect()
}

fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    format!("{}m{}s", seconds / 60, seconds % 60)
}

/// Cancel one agent, resolving a short id back to the full one.
///
/// Cancelling a persistent member is allowed but reported as forced: the
/// graceful stop is a `shutdown_request` from the leader followed by
/// `TeamDelete`, and a cancel skips the member's own wind-down.
fn cancel_agent(registry: &SubAgentRegistry, argument: &str) -> Result<String, String> {
    if argument.is_empty() {
        return Err(format!("Which sub-agent? {USAGE}"));
    }

    let snapshots = registry.list();
    let matches = snapshots
        .iter()
        .filter(|snapshot| snapshot.id.as_str().starts_with(argument))
        .collect::<Vec<_>>();
    let snapshot = match matches.as_slice() {
        [] => {
            return Err(format!(
                "No sub-agent matches {argument}.{}",
                available_suffix(&snapshots)
            ));
        }
        [single] => *single,
        several => {
            let ids = several
                .iter()
                .map(|snapshot| short_id(snapshot.id.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "{argument} matches several sub-agents: {ids}. Use a longer id."
            ));
        }
    };

    // Re-read the status instead of trusting the listing: an agent can reach a
    // terminal state between the two calls, and "cancelled" would then be a lie.
    if let Some(current) = registry.get(&snapshot.id)
        && current.status.is_terminal()
    {
        return Err(format!(
            "{} already {}.",
            short_id(snapshot.id.as_str()),
            status_label(current.status)
        ));
    }
    if !registry.cancel(&snapshot.id) {
        return Err(format!("{} is no longer registered.", short_id(snapshot.id.as_str())));
    }
    let mut message = format!("Cancelled {} ({})", short_id(snapshot.id.as_str()), snapshot.name);
    if snapshot.persistent {
        message.push_str(
            "\nThis was a forced cancel of a persistent team member. \
             The graceful path is a shutdown_request from the leader, then TeamDelete.",
        );
    }
    Ok(message)
}

fn available_suffix(snapshots: &[SubAgentSnapshot]) -> String {
    if snapshots.is_empty() {
        return format!(" {NOTHING_LIVE}");
    }
    let ids = snapshots
        .iter()
        .map(|snapshot| short_id(snapshot.id.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(" Registered: {ids}")
}

#[cfg(test)]
#[path = "agents_test.rs"]
mod agents_test;

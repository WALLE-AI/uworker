use std::path::Path;

use agentrs_config::config::Config;
use agentrs_config::shell::resolve_shell_config;
use agentrs_memory::paths::auto_memory_dir;

use crate::context::{SystemPromptCache, build_system_prompt_with_shell_and_tool_policy};
use crate::tool_policy::ToolPolicy;

pub(crate) struct SubAgentIdentity {
    pub(crate) name: String,
    pub(crate) team: Option<String>,
    pub(crate) teammates: Vec<String>,
}

pub(crate) fn build_child_prompt(
    policy: &ToolPolicy,
    config: &Config,
    workspace: &Path,
    identity: Option<&SubAgentIdentity>,
    omit_project_rules: bool,
) -> anyhow::Result<String> {
    let shell = resolve_shell_config(&config.shell)?;
    let memory_dir = auto_memory_dir(workspace);
    let mut cache = SystemPromptCache::new();
    let workspace_text = workspace.to_string_lossy();
    let mut prompt = build_system_prompt_with_shell_and_tool_policy(
        &mut cache,
        None,
        &workspace_text,
        &config.model,
        &shell,
        &[],
        None,
        memory_dir.as_deref(),
        false,
        config.compact.toon,
        policy,
    );
    if omit_project_rules && let Some(project_rules) = cache.sections.get("agents_md") {
        prompt = prompt.replace(project_rules, "");
    }
    append_subagent_role(&mut prompt, identity);
    Ok(prompt)
}

pub(crate) fn append_subagent_role(prompt: &mut String, identity: Option<&SubAgentIdentity>) {
    prompt.push_str(
        "\n\n# Sub-agent role\n\
         You are a sub-agent created by a primary agent to complete one specific task.\n\
         - Your final text message is the return value read by the primary agent; it is not shown directly to the user.\n\
         - Intermediate narration and tool activity are not visible; omit user-facing transition text.\n\
         - You cannot ask the primary agent questions. Make the most reasonable assumption and state it in the result.\n\
         - If you modify files, list their paths at the beginning of the result.\n\
         - Make the result self-contained because the primary agent receives only that text."
    );
    if let Some(identity) = identity {
        prompt.push_str(&format!("\n- Your agent name is {}.", identity.name));
        if let Some(team) = &identity.team {
            prompt.push_str(&format!(" You are a member of team {team}."));
        }
        if !identity.teammates.is_empty() {
            prompt.push_str(&format!(" Teammates: {}.", identity.teammates.join(", ")));
        }
    }
}

#[cfg(test)]
#[path = "context_test.rs"]
mod context_test;

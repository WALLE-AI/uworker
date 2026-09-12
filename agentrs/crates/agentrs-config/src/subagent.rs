use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubAgentConfig {
    pub enabled: bool,
    pub max_per_call: usize,
    pub max_concurrent: usize,
    pub max_turns: usize,
    pub max_tokens: u32,
    pub depth: usize,
    pub turn_output_budget: Option<u64>,
    #[serde(serialize_with = "serialize_duration")]
    pub cancel_grace: Duration,
    pub persist_sessions: bool,
    pub builtin_agents: bool,
    #[serde(skip)]
    specified: BTreeSet<&'static str>,
}

impl Default for SubAgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_per_call: 5,
            max_concurrent: 5,
            max_turns: 200,
            max_tokens: 4096,
            depth: 1,
            turn_output_budget: None,
            cancel_grace: Duration::from_secs(5),
            persist_sessions: true,
            builtin_agents: true,
            specified: BTreeSet::new(),
        }
    }
}

#[derive(Default, Deserialize)]
struct SubAgentConfigInput {
    enabled: Option<bool>,
    max_per_call: Option<usize>,
    max_concurrent: Option<usize>,
    max_turns: Option<usize>,
    max_tokens: Option<u32>,
    depth: Option<usize>,
    turn_output_budget: Option<u64>,
    cancel_grace: Option<u64>,
    persist_sessions: Option<bool>,
    builtin_agents: Option<bool>,
}

impl<'de> Deserialize<'de> for SubAgentConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = SubAgentConfigInput::deserialize(deserializer)?;
        let mut output = Self::default();
        macro_rules! assign {
            ($field:ident) => {
                if let Some(value) = input.$field {
                    output.$field = value;
                    output.specified.insert(stringify!($field));
                }
            };
        }
        assign!(enabled);
        assign!(max_per_call);
        assign!(max_concurrent);
        assign!(max_turns);
        assign!(max_tokens);
        assign!(depth);
        if let Some(value) = input.turn_output_budget {
            output.turn_output_budget = Some(value);
            output.specified.insert("turn_output_budget");
        }
        if let Some(value) = input.cancel_grace {
            output.cancel_grace = Duration::from_millis(value);
            output.specified.insert("cancel_grace");
        }
        assign!(persist_sessions);
        assign!(builtin_agents);
        Ok(output)
    }
}

impl SubAgentConfig {
    pub(crate) fn overlay(mut self, project: Self) -> Self {
        macro_rules! overlay {
            ($field:ident) => {
                if project.specified.contains(stringify!($field)) {
                    self.$field = project.$field;
                    self.specified.insert(stringify!($field));
                }
            };
        }
        overlay!(enabled);
        overlay!(max_per_call);
        overlay!(max_concurrent);
        overlay!(max_turns);
        overlay!(max_tokens);
        overlay!(depth);
        overlay!(turn_output_budget);
        overlay!(cancel_grace);
        overlay!(persist_sessions);
        overlay!(builtin_agents);
        self
    }
}

fn serialize_duration<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_u64(duration.as_millis().try_into().unwrap_or(u64::MAX))
}

#[cfg(test)]
#[path = "subagent_test.rs"]
mod subagent_test;

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

/// Limits and feature gate for persistent in-process agent teams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamConfig {
    pub enabled: bool,
    pub max_members: usize,
    pub inbox_capacity: usize,
    #[serde(skip)]
    specified: BTreeSet<&'static str>,
}

impl Default for TeamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_members: 8,
            inbox_capacity: 64,
            specified: BTreeSet::new(),
        }
    }
}

#[derive(Default, Deserialize)]
struct TeamConfigInput {
    enabled: Option<bool>,
    max_members: Option<usize>,
    inbox_capacity: Option<usize>,
}

impl<'de> Deserialize<'de> for TeamConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = TeamConfigInput::deserialize(deserializer)?;
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
        assign!(max_members);
        assign!(inbox_capacity);
        Ok(output)
    }
}

impl TeamConfig {
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
        overlay!(max_members);
        overlay!(inbox_capacity);
        self
    }
}

#[cfg(test)]
#[path = "team_test.rs"]
mod team_test;

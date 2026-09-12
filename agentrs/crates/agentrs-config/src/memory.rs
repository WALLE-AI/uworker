use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecallConfig {
    pub enabled: bool,
    pub model: Option<String>,
    pub limit: usize,
    pub timeout_ms: u64,
    #[serde(skip)]
    specified: BTreeSet<&'static str>,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            limit: 5,
            timeout_ms: 3_000,
            specified: BTreeSet::new(),
        }
    }
}

#[derive(Default, Deserialize)]
struct RecallConfigInput {
    enabled: Option<bool>,
    model: Option<String>,
    limit: Option<usize>,
    timeout_ms: Option<u64>,
}

impl<'de> Deserialize<'de> for RecallConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = RecallConfigInput::deserialize(deserializer)?;
        let mut output = Self::default();
        if let Some(value) = input.enabled {
            output.enabled = value;
            output.specified.insert("enabled");
        }
        if input.model.is_some() {
            output.model = input.model;
            output.specified.insert("model");
        }
        if let Some(value) = input.limit {
            output.limit = value;
            output.specified.insert("limit");
        }
        if let Some(value) = input.timeout_ms {
            output.timeout_ms = value;
            output.specified.insert("timeout_ms");
        }
        Ok(output)
    }
}

impl RecallConfig {
    fn overlay(mut self, project: Self) -> Self {
        if project.specified.contains("enabled") {
            self.enabled = project.enabled;
        }
        if project.specified.contains("model") {
            self.model = project.model;
        }
        if project.specified.contains("limit") {
            self.limit = project.limit;
        }
        if project.specified.contains("timeout_ms") {
            self.timeout_ms = project.timeout_ms;
        }
        self.specified.extend(project.specified);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtractConfig {
    pub enabled: bool,
    pub model: Option<String>,
    pub max_turns: usize,
    #[serde(skip)]
    specified: BTreeSet<&'static str>,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: None,
            max_turns: 5,
            specified: BTreeSet::new(),
        }
    }
}

#[derive(Default, Deserialize)]
struct ExtractConfigInput {
    enabled: Option<bool>,
    model: Option<String>,
    max_turns: Option<usize>,
}

impl<'de> Deserialize<'de> for ExtractConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = ExtractConfigInput::deserialize(deserializer)?;
        let mut output = Self::default();
        if let Some(value) = input.enabled {
            output.enabled = value;
            output.specified.insert("enabled");
        }
        if input.model.is_some() {
            output.model = input.model;
            output.specified.insert("model");
        }
        if let Some(value) = input.max_turns {
            output.max_turns = value;
            output.specified.insert("max_turns");
        }
        Ok(output)
    }
}

impl ExtractConfig {
    fn overlay(mut self, project: Self) -> Self {
        if project.specified.contains("enabled") {
            self.enabled = project.enabled;
        }
        if project.specified.contains("model") {
            self.model = project.model;
        }
        if project.specified.contains("max_turns") {
            self.max_turns = project.max_turns;
        }
        self.specified.extend(project.specified);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub dir: Option<PathBuf>,
    pub recall: RecallConfig,
    pub extract: ExtractConfig,
    #[serde(skip)]
    specified: BTreeSet<&'static str>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: None,
            recall: RecallConfig::default(),
            extract: ExtractConfig::default(),
            specified: BTreeSet::new(),
        }
    }
}

#[derive(Default, Deserialize)]
struct MemoryConfigInput {
    enabled: Option<bool>,
    dir: Option<PathBuf>,
    recall: Option<RecallConfig>,
    extract: Option<ExtractConfig>,
}

impl<'de> Deserialize<'de> for MemoryConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = MemoryConfigInput::deserialize(deserializer)?;
        let mut output = Self::default();
        if let Some(value) = input.enabled {
            output.enabled = value;
            output.specified.insert("enabled");
        }
        if input.dir.is_some() {
            output.dir = input.dir;
            output.specified.insert("dir");
        }
        if let Some(value) = input.recall {
            output.recall = value;
            output.specified.insert("recall");
        }
        if let Some(value) = input.extract {
            output.extract = value;
            output.specified.insert("extract");
        }
        Ok(output)
    }
}

impl MemoryConfig {
    pub(crate) fn overlay(mut self, project: Self) -> Self {
        if project.specified.contains("enabled") {
            self.enabled = project.enabled;
        }
        if project.specified.contains("dir") {
            self.dir = project.dir;
        }
        if project.specified.contains("recall") {
            self.recall = self.recall.overlay(project.recall);
        }
        if project.specified.contains("extract") {
            self.extract = self.extract.overlay(project.extract);
        }
        self.specified.extend(project.specified);
        self
    }
}

#[cfg(test)]
#[path = "memory_test.rs"]
mod memory_test;

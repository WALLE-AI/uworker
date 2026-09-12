use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use agentrs_types::team::{InboxMessage, TeamError, TeamFile, TeamId};

pub(crate) struct TeamStore {
    root: PathBuf,
}

impl TeamStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn team_dir(&self, team: &TeamId) -> PathBuf {
        self.root.join(team.as_str())
    }

    pub(crate) fn config_path(&self, team: &TeamId) -> PathBuf {
        self.team_dir(team).join("config.json")
    }

    pub(crate) fn write(&self, team: &TeamFile) -> Result<(), TeamError> {
        let path = self.config_path(&team.team.id);
        write_json_atomic(&path, team)
    }

    pub(crate) fn read(&self, team: &TeamId) -> Result<Option<TeamFile>, TeamError> {
        let path = self.config_path(team);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(storage_error),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(storage_error(error)),
        }
    }

    pub(crate) fn append_inbox(&self, team: &TeamId, recipient: &str, message: &InboxMessage) -> Result<(), TeamError> {
        let path = self.team_dir(team).join("inboxes").join(format!("{recipient}.json"));
        let mut messages = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Vec<InboxMessage>>(&bytes).map_err(storage_error)?,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(storage_error(error)),
        };
        messages.push(message.clone());
        write_json_atomic(&path, &messages)
    }

    pub(crate) fn delete(&self, team: &TeamId) -> Result<(), TeamError> {
        let path = self.team_dir(team);
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(storage_error(error)),
        }
    }
}

fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> Result<(), TeamError> {
    let parent = path.parent().ok_or_else(|| TeamError::Storage {
        reason: "team path has no parent directory".to_string(),
    })?;
    fs::create_dir_all(parent).map_err(storage_error)?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(storage_error)?;
    fs::write(&temporary, bytes).map_err(storage_error)?;
    fs::rename(&temporary, path).map_err(storage_error)
}

fn storage_error(error: impl std::fmt::Display) -> TeamError {
    TeamError::Storage {
        reason: error.to_string(),
    }
}

#[cfg(test)]
#[path = "store_test.rs"]
mod store_test;

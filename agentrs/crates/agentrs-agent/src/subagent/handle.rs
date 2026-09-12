use std::sync::{Arc, RwLock};
use std::time::Instant;

use agentrs_types::subagent::{SubAgentId, SubAgentResult, SubAgentStatus};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::registry::SubAgentSnapshot;

pub(crate) struct SubAgentHandle {
    pub(crate) id: SubAgentId,
    pub(crate) name: String,
    pub(crate) status: Arc<RwLock<SubAgentStatus>>,
    pub(crate) cancel: CancellationToken,
    pub(crate) join: Mutex<Option<JoinHandle<SubAgentResult>>>,
    pub(crate) persistent: bool,
    pub(crate) started_at: Instant,
}

impl SubAgentHandle {
    pub(crate) fn new(
        id: SubAgentId,
        name: String,
        status: Arc<RwLock<SubAgentStatus>>,
        cancel: CancellationToken,
        join: JoinHandle<SubAgentResult>,
    ) -> Self {
        Self {
            id,
            name,
            status,
            cancel,
            join: Mutex::new(Some(join)),
            persistent: false,
            started_at: Instant::now(),
        }
    }

    pub(crate) fn new_persistent(
        id: SubAgentId,
        name: String,
        status: Arc<RwLock<SubAgentStatus>>,
        cancel: CancellationToken,
        join: JoinHandle<SubAgentResult>,
    ) -> Self {
        let mut handle = Self::new(id, name, status, cancel, join);
        handle.persistent = true;
        handle
    }

    pub(crate) fn snapshot(&self) -> SubAgentSnapshot {
        let status = *self.status.read().unwrap_or_else(|poisoned| poisoned.into_inner());
        SubAgentSnapshot {
            id: self.id.clone(),
            name: self.name.clone(),
            status,
            started_at: self.started_at,
        }
    }

    pub(crate) fn transition_status(status: &RwLock<SubAgentStatus>, next: SubAgentStatus) -> bool {
        let mut current = status.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.is_terminal() {
            return false;
        }
        *current = next;
        true
    }
}

#[cfg(test)]
#[path = "handle_test.rs"]
mod handle_test;

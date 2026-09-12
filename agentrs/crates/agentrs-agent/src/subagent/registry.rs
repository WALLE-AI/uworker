use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use agentrs_types::message::TokenUsage;
use agentrs_types::subagent::{SubAgentId, SubAgentResult, SubAgentStatus};
use tokio::sync::Semaphore;

use super::handle::SubAgentHandle;

#[derive(Debug, Clone)]
#[allow(dead_code)] // Read by addressable control surfaces added after the runtime core.
pub(crate) struct SubAgentSnapshot {
    pub(crate) id: SubAgentId,
    pub(crate) name: String,
    pub(crate) status: SubAgentStatus,
    pub(crate) started_at: Instant,
}

pub(crate) struct SubAgentRegistry {
    handles: RwLock<HashMap<SubAgentId, Arc<SubAgentHandle>>>,
    permits: Arc<Semaphore>,
    turn_usage: Mutex<TokenUsage>,
    turn_output_budget: Option<u64>,
    cancel_grace: Duration,
}

impl SubAgentRegistry {
    pub(crate) fn new(max_concurrent: usize, turn_output_budget: Option<u64>, cancel_grace: Duration) -> Self {
        Self {
            handles: RwLock::new(HashMap::new()),
            permits: Arc::new(Semaphore::new(max_concurrent.max(1))),
            turn_usage: Mutex::new(TokenUsage::default()),
            turn_output_budget,
            cancel_grace,
        }
    }

    pub(crate) fn permits(&self) -> Arc<Semaphore> {
        Arc::clone(&self.permits)
    }

    pub(crate) fn register(&self, handle: SubAgentHandle) {
        let id = handle.id.clone();
        self.handles
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, Arc::new(handle));
    }

    #[allow(dead_code)] // Addressable query API used by the upcoming control tools.
    pub(crate) fn get(&self, id: &SubAgentId) -> Option<SubAgentSnapshot> {
        self.handles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .map(|handle| handle.snapshot())
    }

    #[allow(dead_code)] // Addressable query API used by the upcoming control tools.
    pub(crate) fn list(&self) -> Vec<SubAgentSnapshot> {
        self.handles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|handle| handle.snapshot())
            .collect()
    }

    #[allow(dead_code)] // Addressable cancellation API used by the upcoming control tools.
    pub(crate) fn cancel(&self, id: &SubAgentId) -> bool {
        let handle = self
            .handles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned();
        if let Some(handle) = handle {
            handle.cancel.cancel();
            true
        } else {
            false
        }
    }

    pub(crate) fn cancel_all_now(&self) {
        for handle in self
            .handles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
        {
            handle.cancel.cancel();
        }
    }

    pub(crate) async fn wait(&self, id: &SubAgentId) -> Option<SubAgentResult> {
        let handle = self
            .handles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()?;
        let join = handle.join.lock().await.take()?;
        let result = join.await.ok();
        self.handles
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
        if let Some(result) = &result {
            self.record_usage(&result.usage);
        }
        result
    }

    #[allow(dead_code)] // Graceful shutdown path is exercised by lifecycle tests and hosts.
    pub(crate) async fn cancel_all(&self) {
        let handles = self
            .handles
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for handle in &handles {
            handle.cancel.cancel();
        }
        for handle in handles {
            if let Some(mut join) = handle.join.lock().await.take()
                && tokio::time::timeout(self.cancel_grace, &mut join).await.is_err()
            {
                join.abort();
                let _ = join.await;
            }
        }
        self.handles
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    pub(crate) fn budget_available(&self) -> bool {
        let usage = self.turn_usage.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.turn_output_budget
            .is_none_or(|budget| usage.output_tokens < budget)
    }

    pub(crate) fn drain_turn_usage(&self) -> TokenUsage {
        let mut usage = self.turn_usage.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *usage)
    }

    fn record_usage(&self, additional: &TokenUsage) {
        let mut usage = self.turn_usage.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        usage.input_tokens += additional.input_tokens;
        usage.output_tokens += additional.output_tokens;
        usage.cache_creation_tokens += additional.cache_creation_tokens;
        usage.cache_read_tokens += additional.cache_read_tokens;
    }
}

impl Drop for SubAgentRegistry {
    fn drop(&mut self) {
        let handles = self.handles.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !handles.is_empty() {
            tracing::warn!(target: "agentrs_agent", active = handles.len(), "sub-agent registry dropped with active tasks");
            for handle in handles.values() {
                handle.cancel.cancel();
                if let Ok(mut join) = handle.join.try_lock()
                    && let Some(join) = join.take()
                {
                    join.abort();
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod registry_test;

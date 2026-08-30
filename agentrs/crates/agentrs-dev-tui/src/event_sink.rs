//! Bounded runtime-to-TUI event delivery.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use agentrs_contracts::event::RunEventEnvelope;
use agentrs_contracts::ports::{EventSinkError, RunEventSink};
use tokio::sync::mpsc;

/// A bounded event sink. Durable events backpressure the producer; live events
/// may be dropped because the durable projection remains authoritative.
pub struct ChannelEventSink {
    sender: mpsc::Sender<RunEventEnvelope>,
    dropped_live: Arc<AtomicU64>,
}

impl ChannelEventSink {
    /// Creates a channel-backed sink and its consumer.
    pub fn bounded(capacity: usize) -> (Arc<Self>, mpsc::Receiver<RunEventEnvelope>, Arc<AtomicU64>) {
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        let dropped_live = Arc::new(AtomicU64::new(0));
        (
            Arc::new(Self {
                sender,
                dropped_live: dropped_live.clone(),
            }),
            receiver,
            dropped_live,
        )
    }
}

#[async_trait::async_trait]
impl RunEventSink for ChannelEventSink {
    async fn publish(&self, event: RunEventEnvelope) -> Result<(), EventSinkError> {
        if event.is_durable() {
            return self.sender.send(event).await.map_err(|_| EventSinkError::Closed);
        }

        self.sender.try_send(event).map_err(|error| match error {
            mpsc::error::TrySendError::Closed(_) => EventSinkError::Closed,
            mpsc::error::TrySendError::Full(_) => {
                self.dropped_live.fetch_add(1, Ordering::Relaxed);
                EventSinkError::Lagged
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentrs_contracts::event::{Causality, Durability, EventPayload, Visibility};
    use agentrs_contracts::ids::{EventId, RunEpoch, Timestamp};

    fn event(id: &str, durability: Durability) -> RunEventEnvelope {
        RunEventEnvelope {
            run_id: "run".into(),
            epoch: RunEpoch(1),
            event_id: EventId::new(id),
            seq: None,
            live_seq: None,
            at: Timestamp(0),
            durability,
            visibility: Visibility::Internal,
            causality: Causality::default(),
            surface: None,
            payload: EventPayload::RunStarted,
        }
    }

    #[tokio::test]
    async fn drops_only_live_events_when_full() {
        let (sink, mut receiver, dropped) = ChannelEventSink::bounded(1);
        sink.publish(event("one", Durability::LiveStream)).await.unwrap();
        assert!(matches!(
            sink.publish(event("two", Durability::LiveStream)).await,
            Err(EventSinkError::Lagged)
        ));
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert_eq!(receiver.recv().await.unwrap().event_id, EventId::new("one"));
    }

    #[tokio::test]
    async fn durable_event_waits_for_capacity() {
        let (sink, mut receiver, _) = ChannelEventSink::bounded(1);
        sink.publish(event("one", Durability::LiveStream)).await.unwrap();
        let task = tokio::spawn({
            let sink = sink.clone();
            async move { sink.publish(event("fact", Durability::DurableFact)).await }
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        receiver.recv().await;
        task.await.unwrap().unwrap();
        assert_eq!(receiver.recv().await.unwrap().event_id, EventId::new("fact"));
    }
}

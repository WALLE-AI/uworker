use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use agentrs_types::team::InboxMessage;
use tokio::sync::Notify;

pub(crate) struct TeammateInbox {
    messages: Mutex<VecDeque<InboxMessage>>,
    notify: Notify,
    capacity: usize,
    closed: AtomicBool,
}

impl TeammateInbox {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            messages: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            capacity: capacity.max(1),
            closed: AtomicBool::new(false),
        }
    }

    pub(crate) fn push(&self, message: InboxMessage) -> Result<(), InboxPushError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(InboxPushError::Closed);
        }
        let mut messages = self.messages.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if messages.len() >= self.capacity {
            return Err(InboxPushError::Full);
        }
        messages.push_back(message);
        drop(messages);
        self.notify.notify_one();
        Ok(())
    }

    pub(crate) fn drain(&self) -> Vec<InboxMessage> {
        self.messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
            .collect()
    }

    pub(crate) async fn wait(&self) -> Vec<InboxMessage> {
        loop {
            let notified = self.notify.notified();
            let messages = self.drain();
            if !messages.is_empty() || self.closed.load(Ordering::Acquire) {
                return messages;
            }
            notified.await;
        }
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
}

#[derive(Debug)]
pub(crate) enum InboxPushError {
    Full,
    Closed,
}

pub(crate) fn render_messages(messages: &[InboxMessage]) -> String {
    messages
        .iter()
        .map(|message| {
            format!(
                "<teammate-message from=\"{}\" type=\"{}\">{}</teammate-message>",
                escape_xml(message.from.as_str()),
                message_kind(message),
                escape_xml(&message.message),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn message_kind(message: &InboxMessage) -> &'static str {
    use agentrs_types::team::TeamMessageKind;
    match message.kind {
        TeamMessageKind::Text => "message",
        TeamMessageKind::ShutdownRequest { .. } => "shutdown_request",
        TeamMessageKind::ShutdownResponse { .. } => "shutdown_response",
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
#[path = "inbox_test.rs"]
mod inbox_test;

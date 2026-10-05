//! In-process fan-out for live SSE subscribers.
//!
//! Subscribers get a bounded buffer; a subscriber that falls too far behind is
//! disconnected rather than allowed to block event journaling (docs: slow
//! clients are dropped, never allowed to stall durable writes).

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;

use bollo_protocol::events::EventEnvelope;

/// Per-subscriber backlog before the client counts as slow.
pub const SUBSCRIBER_BUFFER: usize = 256;

#[derive(Default)]
pub struct EventBus {
    subscribers: Mutex<Vec<SyncSender<EventEnvelope>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&self) -> Receiver<EventEnvelope> {
        let (sender, receiver) = sync_channel(SUBSCRIBER_BUFFER);
        self.subscribers
            .lock()
            .expect("event bus lock")
            .push(sender);
        receiver
    }

    pub fn publish(&self, event: &EventEnvelope) {
        let Ok(mut subscribers) = self.subscribers.lock() else {
            return;
        };
        subscribers.retain(|sender| match sender.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().expect("event bus lock").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_protocol::events::{EventEnvelope, EventType};
    use bollo_protocol::ids::{RunId, SessionId};
    use serde_json::json;

    fn event(session: &SessionId, seq: u64, text: &str) -> EventEnvelope {
        EventEnvelope::new(
            session,
            Some(&RunId::generate()),
            seq,
            EventType::AssistantDelta,
            &json!({ "text": text }),
        )
        .unwrap()
    }

    #[test]
    fn publishes_to_every_live_subscriber() {
        let bus = EventBus::new();
        let first = bus.subscribe();
        let second = bus.subscribe();
        let session = SessionId::generate();
        bus.publish(&event(&session, 1, "hello"));
        assert_eq!(first.recv().unwrap().data["text"], "hello");
        assert_eq!(second.recv().unwrap().data["text"], "hello");
    }

    #[test]
    fn dropped_subscribers_are_forgotten() {
        let bus = EventBus::new();
        let subscriber = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        drop(subscriber);
        let session = SessionId::generate();
        bus.publish(&event(&session, 1, "x"));
        assert_eq!(bus.subscriber_count(), 0);
    }
}

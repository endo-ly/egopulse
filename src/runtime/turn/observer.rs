//! Runtime-owned observers for turn event and completion delivery.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::{mpsc, oneshot};

use crate::agent_loop::event::AgentEvent;

/// Receives lifecycle events and the terminal result of one scheduled turn.
pub(crate) struct TurnObserver {
    pub(crate) events: mpsc::UnboundedReceiver<AgentEvent>,
    pub(crate) completion: oneshot::Receiver<Result<String, String>>,
}

struct ObserverSink {
    events: mpsc::UnboundedSender<AgentEvent>,
    completion: oneshot::Sender<Result<String, String>>,
}

/// Routes runtime-owned turn output to a client without owning the turn.
pub(crate) struct TurnObserverRegistry {
    sinks: Mutex<HashMap<String, ObserverSink>>,
}

impl TurnObserverRegistry {
    pub(crate) fn new() -> Self {
        Self {
            sinks: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn register(&self, request_key: String) -> TurnObserver {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (completion_tx, completion) = oneshot::channel();
        self.sinks.lock().expect("turn observer lock").insert(
            request_key,
            ObserverSink {
                events: events_tx,
                completion: completion_tx,
            },
        );
        TurnObserver { events, completion }
    }

    pub(crate) fn unregister(&self, request_key: &str) {
        self.sinks
            .lock()
            .expect("turn observer lock")
            .remove(request_key);
    }

    pub(crate) fn has_live_observer(&self, request_key: &str) -> bool {
        let mut sinks = self.sinks.lock().expect("turn observer lock");
        let Some(sink) = sinks.get(request_key) else {
            return false;
        };
        if sink.events.is_closed() || sink.completion.is_closed() {
            sinks.remove(request_key);
            false
        } else {
            true
        }
    }

    /// Moves a live client observer when a staged follow-up becomes a new
    /// root turn. Returns `false` when the source observer is absent or the
    /// destination is already occupied.
    pub(crate) fn transfer(&self, from_request_key: &str, to_request_key: &str) -> bool {
        if from_request_key == to_request_key {
            return true;
        }
        let mut sinks = self.sinks.lock().expect("turn observer lock");
        if sinks.contains_key(to_request_key) {
            return false;
        }
        let Some(sink) = sinks.remove(from_request_key) else {
            return false;
        };
        sinks.insert(to_request_key.to_string(), sink);
        true
    }

    pub(crate) fn emit(&self, request_key: &str, event: AgentEvent) {
        let mut sinks = self.sinks.lock().expect("turn observer lock");
        let disconnected = sinks
            .get(request_key)
            .is_some_and(|sink| sink.events.send(event).is_err());
        if disconnected {
            sinks.remove(request_key);
        }
    }

    pub(crate) fn finish(&self, request_key: &str, result: Result<String, String>) {
        let Some(sink) = self
            .sinks
            .lock()
            .expect("turn observer lock")
            .remove(request_key)
        else {
            return;
        };
        let _ = sink.completion.send(result);
    }
}

impl Default for TurnObserverRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transfer_keeps_the_live_observer_until_the_new_turn_finishes() {
        // Arrange
        let registry = TurnObserverRegistry::new();
        let observer = registry.register("parent-request".to_string());
        let TurnObserver {
            mut events,
            completion,
        } = observer;

        // Act
        assert!(registry.transfer("parent-request", "promoted-request"));
        registry.emit("promoted-request", AgentEvent::Iteration { iteration: 1 });
        registry.finish("promoted-request", Ok("done".to_string()));

        // Assert
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::Iteration { iteration: 1 })
        ));
        assert_eq!(
            completion.await.expect("completion sender"),
            Ok("done".to_string())
        );
        assert!(!registry.has_live_observer("promoted-request"));
    }
}

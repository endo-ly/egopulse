//! Runtime-owned observers for turn event and completion delivery.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::{mpsc, oneshot};

use crate::agent_loop::event::AgentEvent;

use super::ResponseDelivery;

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

    pub(crate) fn register(&self, observer_id: String) -> (ResponseDelivery, TurnObserver) {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (completion_tx, completion) = oneshot::channel();
        self.sinks.lock().expect("turn observer lock").insert(
            observer_id.clone(),
            ObserverSink {
                events: events_tx,
                completion: completion_tx,
            },
        );
        (
            ResponseDelivery::Observer { observer_id },
            TurnObserver { events, completion },
        )
    }

    pub(crate) fn unregister(&self, observer_id: &str) {
        self.sinks
            .lock()
            .expect("turn observer lock")
            .remove(observer_id);
    }

    pub(crate) fn emit(&self, observer_id: &str, event: AgentEvent) {
        let mut sinks = self.sinks.lock().expect("turn observer lock");
        let disconnected = sinks
            .get(observer_id)
            .is_some_and(|sink| sink.events.send(event).is_err());
        if disconnected {
            sinks.remove(observer_id);
        }
    }

    pub(crate) fn finish(&self, observer_id: &str, result: Result<String, String>) {
        let Some(sink) = self
            .sinks
            .lock()
            .expect("turn observer lock")
            .remove(observer_id)
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

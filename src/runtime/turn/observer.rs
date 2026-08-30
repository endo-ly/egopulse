//! Runtime-owned observers for turn event and completion delivery.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use crate::agent_loop::event::AgentEvent;

/// Receives lifecycle events and the terminal result of a client interaction.
pub(crate) struct TurnObserver {
    pub(crate) events: mpsc::UnboundedReceiver<AgentEvent>,
    pub(crate) completion: oneshot::Receiver<Result<String, String>>,
}

struct ObserverSink {
    events: mpsc::UnboundedSender<AgentEvent>,
    state: Mutex<ObserverState>,
    initial_events: Mutex<HashMap<String, AgentEvent>>,
}

struct ObserverState {
    completion: Option<oneshot::Sender<Result<String, String>>>,
    pending_turns: usize,
    failure: Option<String>,
    last_response: Option<String>,
}

/// Routes runtime-owned turn output to a client without owning the turn.
pub(crate) struct TurnObserverRegistry {
    sinks: Mutex<HashMap<String, Arc<ObserverSink>>>,
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
            Arc::new(ObserverSink {
                events: events_tx,
                state: Mutex::new(ObserverState {
                    completion: Some(completion_tx),
                    pending_turns: 1,
                    failure: None,
                    last_response: None,
                }),
                initial_events: Mutex::new(HashMap::new()),
            }),
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
        let Some(sink) = sinks.get(request_key).cloned() else {
            return false;
        };
        let completion_closed = sink
            .state
            .lock()
            .expect("turn observer state lock")
            .completion
            .as_ref()
            .is_none_or(oneshot::Sender::is_closed);
        if sink.events.is_closed() || completion_closed {
            remove_sink_routes(&mut sinks, &sink);
            false
        } else {
            true
        }
    }

    /// Assigns one live client observer to several staged follow-up root
    /// turns. The observer completes only after every assigned turn finishes.
    /// Returns `false` when the source observer is absent, closed, or any
    /// destination is already occupied.
    pub(crate) fn transfer_many(&self, from_request_key: &str, to_request_keys: &[String]) -> bool {
        if to_request_keys.is_empty()
            || to_request_keys
                .iter()
                .any(|request_key| request_key == from_request_key)
            || has_duplicate_keys(to_request_keys)
        {
            return false;
        }
        let mut sinks = self.sinks.lock().expect("turn observer lock");
        if to_request_keys
            .iter()
            .any(|request_key| sinks.contains_key(request_key))
        {
            return false;
        }
        let Some(sink) = sinks.get(from_request_key).cloned() else {
            return false;
        };
        if sink.events.is_closed()
            || sink
                .state
                .lock()
                .expect("turn observer state lock")
                .completion
                .as_ref()
                .is_none_or(oneshot::Sender::is_closed)
        {
            remove_sink_routes(&mut sinks, &sink);
            return false;
        }

        {
            let mut state = sink.state.lock().expect("turn observer state lock");
            let Some(pending_without_source) = state.pending_turns.checked_sub(1) else {
                return false;
            };
            state.pending_turns = pending_without_source + to_request_keys.len();
        }
        sinks.remove(from_request_key);
        for request_key in to_request_keys {
            sinks.insert(request_key.clone(), Arc::clone(&sink));
        }
        true
    }

    /// Queues an event for delivery when the assigned turn actually starts.
    /// This keeps a queued turn's initial input behind all events from the
    /// preceding turn in the same client interaction.
    pub(crate) fn queue_initial_event(&self, request_key: String, event: AgentEvent) {
        let sink = self
            .sinks
            .lock()
            .expect("turn observer lock")
            .get(&request_key)
            .cloned();
        if let Some(sink) = sink {
            sink.initial_events
                .lock()
                .expect("turn observer initial event lock")
                .insert(request_key, event);
        }
    }

    pub(crate) fn emit_initial_event(&self, request_key: &str) {
        let sink = self
            .sinks
            .lock()
            .expect("turn observer lock")
            .get(request_key)
            .cloned();
        let Some(sink) = sink else {
            return;
        };
        let event = sink
            .initial_events
            .lock()
            .expect("turn observer initial event lock")
            .remove(request_key);
        if let Some(event) = event {
            self.emit(request_key, event);
        }
    }

    pub(crate) fn emit(&self, request_key: &str, event: AgentEvent) {
        let mut sinks = self.sinks.lock().expect("turn observer lock");
        let Some(sink) = sinks.get(request_key).cloned() else {
            return;
        };
        if sink.events.send(event).is_err() {
            remove_sink_routes(&mut sinks, &sink);
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
        let completion = {
            let mut state = sink.state.lock().expect("turn observer state lock");
            match result {
                Ok(response) => state.last_response = Some(response),
                Err(message) => {
                    if state.failure.is_none() {
                        state.failure = Some(message);
                    }
                }
            }
            state.pending_turns = state
                .pending_turns
                .checked_sub(1)
                .expect("turn observer finished more times than assigned");
            if state.pending_turns == 0 {
                let result = match state.failure.take() {
                    Some(message) => Err(message),
                    None => Ok(state.last_response.take().unwrap_or_default()),
                };
                state.completion.take().map(|sender| (sender, result))
            } else {
                None
            }
        };
        if let Some((sender, result)) = completion {
            let _ = sender.send(result);
        }
    }
}

fn has_duplicate_keys(keys: &[String]) -> bool {
    keys.iter()
        .enumerate()
        .any(|(index, key)| keys[..index].iter().any(|previous| previous == key))
}

fn remove_sink_routes(sinks: &mut HashMap<String, Arc<ObserverSink>>, target: &Arc<ObserverSink>) {
    sinks.retain(|_, sink| !Arc::ptr_eq(sink, target));
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
    async fn transfer_many_keeps_the_live_observer_until_all_turns_finish() {
        // Arrange
        let registry = TurnObserverRegistry::new();
        let observer = registry.register("parent-request".to_string());
        let TurnObserver {
            mut events,
            completion,
        } = observer;

        // Act
        assert!(registry.transfer_many(
            "parent-request",
            &[
                "promoted-request-1".to_string(),
                "promoted-request-2".to_string()
            ]
        ));
        registry.emit("promoted-request-1", AgentEvent::Iteration { iteration: 1 });
        registry.finish("promoted-request-1", Ok("first".to_string()));
        registry.finish("promoted-request-2", Ok("second".to_string()));

        // Assert
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::Iteration { iteration: 1 })
        ));
        assert_eq!(
            completion.await.expect("completion sender"),
            Ok("second".to_string())
        );
        assert!(!registry.has_live_observer("promoted-request-1"));
        assert!(!registry.has_live_observer("promoted-request-2"));
    }
}

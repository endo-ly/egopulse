//! Runtime-owned observers for turn event and completion delivery.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use crate::agent_loop::event::AgentEvent;

/// Receives lifecycle events and the completion signal of a client interaction.
pub(crate) struct TurnObserver {
    pub(crate) events: mpsc::UnboundedReceiver<AgentEvent>,
    pub(crate) completion: oneshot::Receiver<()>,
}

struct ObserverSink {
    events: mpsc::UnboundedSender<AgentEvent>,
    state: Mutex<ObserverState>,
    initial_events: Mutex<HashMap<String, AgentEvent>>,
}

struct ObserverState {
    completion: Option<oneshot::Sender<()>>,
    pending_turns: usize,
    pending_final_response: Option<PendingFinalResponse>,
}

/// A FinalResponse held back until its Turn finishes, so the shared
/// interaction can compute `terminal` across staged follow-up Turns. The
/// producing Turn's id and final message id travel with the text so
/// publishers forward them without further lookup.
struct PendingFinalResponse {
    turn_id: String,
    assistant_message_id: Option<String>,
    text: String,
}

/// How a Turn finished, as reported to the observer.
pub(crate) enum TurnOutcome {
    /// The Turn produced its final response: the pending FinalResponse is
    /// forwarded with the id it already carries.
    Completed,
    /// The Turn failed: an Error event is emitted. Partial output keeps the
    /// id it streamed under, so no lookup is needed.
    Failed { message: String },
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

    /// Registers an observer only when the request key has no live owner.
    ///
    /// The decision and insertion share one lock so a duplicate delivery can
    /// never replace the observer that owns the original request.
    pub(crate) fn register_if_absent(&self, request_key: String) -> Option<TurnObserver> {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (completion_tx, completion) = oneshot::channel();
        let mut sinks = self.sinks.lock().expect("turn observer lock");
        if let Some(existing) = sinks.get(&request_key).cloned() {
            let completion_closed = existing
                .state
                .lock()
                .expect("turn observer state lock")
                .completion
                .as_ref()
                .is_none_or(oneshot::Sender::is_closed);
            if !existing.events.is_closed() && !completion_closed {
                return None;
            }
            remove_sink_routes(&mut sinks, &existing);
        }
        sinks.insert(
            request_key,
            Arc::new(ObserverSink {
                events: events_tx,
                state: Mutex::new(ObserverState {
                    completion: Some(completion_tx),
                    pending_turns: 1,
                    pending_final_response: None,
                }),
                initial_events: Mutex::new(HashMap::new()),
            }),
        );
        Some(TurnObserver { events, completion })
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
    /// turns. The observer completes only after every assigned turn finishes;
    /// each turn's result remains an event in the shared stream.
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
        if let AgentEvent::FinalResponse {
            turn_id,
            assistant_message_id,
            text,
            ..
        } = event
        {
            sink.state
                .lock()
                .expect("turn observer state lock")
                .pending_final_response = Some(PendingFinalResponse {
                turn_id,
                assistant_message_id,
                text,
            });
            return;
        }
        if sink.events.send(event).is_err() {
            remove_sink_routes(&mut sinks, &sink);
        }
    }

    pub(crate) fn finish(&self, request_key: &str, turn_id: &str, outcome: TurnOutcome) {
        let mut sinks = self.sinks.lock().expect("turn observer lock");
        let Some(sink) = sinks.get(request_key).cloned() else {
            return;
        };
        sinks.remove(request_key);
        sink.initial_events
            .lock()
            .expect("turn observer initial event lock")
            .remove(request_key);
        let (terminal, final_response, completion) = {
            let mut state = sink.state.lock().expect("turn observer state lock");
            let terminal = state.pending_turns == 1;
            let final_response = match outcome {
                TurnOutcome::Completed => state.pending_final_response.take(),
                TurnOutcome::Failed { .. } => {
                    state.pending_final_response.take();
                    None
                }
            };
            state.pending_turns = state
                .pending_turns
                .checked_sub(1)
                .expect("turn observer finished more times than assigned");
            let completion = if terminal {
                state.completion.take()
            } else {
                None
            };
            (terminal, final_response, completion)
        };
        if terminal {
            remove_sink_routes(&mut sinks, &sink);
        }
        drop(sinks);

        match outcome {
            TurnOutcome::Completed => {
                if let Some(final_response) = final_response {
                    let _ = sink.events.send(AgentEvent::FinalResponse {
                        turn_id: final_response.turn_id,
                        assistant_message_id: final_response.assistant_message_id,
                        text: final_response.text,
                        terminal,
                    });
                }
            }
            TurnOutcome::Failed { message } => {
                let _ = sink.events.send(AgentEvent::Error {
                    turn_id: turn_id.to_string(),
                    message,
                    terminal,
                });
            }
        }
        if let Some(sender) = completion {
            let _ = sender.send(());
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
        let observer = registry
            .register_if_absent("parent-request".to_string())
            .expect("observer should be registered");
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
        registry.finish("promoted-request-1", "turn-1", TurnOutcome::Completed);
        registry.finish("promoted-request-2", "turn-2", TurnOutcome::Completed);

        // Assert
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::Iteration { iteration: 1 })
        ));
        completion.await.expect("completion sender");
        assert!(!registry.has_live_observer("promoted-request-1"));
        assert!(!registry.has_live_observer("promoted-request-2"));
    }

    #[tokio::test]
    async fn final_response_becomes_terminal_only_after_all_transferred_turns_finish() {
        // Arrange
        let registry = TurnObserverRegistry::new();
        let observer = registry
            .register_if_absent("parent-request".to_string())
            .expect("observer should be registered");
        let TurnObserver {
            mut events,
            completion,
        } = observer;
        assert!(registry.transfer_many(
            "parent-request",
            &["follow-up-a".to_string(), "follow-up-b".to_string()]
        ));

        // Act
        registry.emit(
            "follow-up-a",
            AgentEvent::FinalResponse {
                turn_id: "turn-a".to_string(),
                assistant_message_id: Some("turn:turn-a:assistant:2".to_string()),
                text: "response A".to_string(),
                terminal: false,
            },
        );
        registry.finish("follow-up-a", "turn-a", TurnOutcome::Completed);
        registry.emit(
            "follow-up-b",
            AgentEvent::FinalResponse {
                turn_id: "turn-b".to_string(),
                assistant_message_id: Some("turn:turn-b:assistant:1".to_string()),
                text: "response B".to_string(),
                terminal: false,
            },
        );
        registry.finish("follow-up-b", "turn-b", TurnOutcome::Completed);

        // Assert
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::FinalResponse {
                turn_id,
                assistant_message_id,
                text,
                terminal: false,
                ..
            }) if text == "response A"
                && turn_id == "turn-a"
                && assistant_message_id.as_deref() == Some("turn:turn-a:assistant:2")
        ));
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::FinalResponse {
                turn_id,
                assistant_message_id,
                text,
                terminal: true,
                ..
            }) if text == "response B"
                && turn_id == "turn-b"
                && assistant_message_id.as_deref() == Some("turn:turn-b:assistant:1")
        ));
        completion.await.expect("completion sender");
    }

    #[tokio::test]
    async fn intermediate_error_does_not_close_a_transferred_observer() {
        // Arrange
        let registry = TurnObserverRegistry::new();
        let observer = registry
            .register_if_absent("parent-request".to_string())
            .expect("observer should be registered");
        let TurnObserver {
            mut events,
            completion,
        } = observer;
        assert!(registry.transfer_many(
            "parent-request",
            &["follow-up-a".to_string(), "follow-up-b".to_string()]
        ));

        // Act
        registry.finish(
            "follow-up-a",
            "turn-a",
            TurnOutcome::Failed {
                message: "follow-up A failed".to_string(),
            },
        );
        registry.emit(
            "follow-up-b",
            AgentEvent::FinalResponse {
                turn_id: "turn-b".to_string(),
                assistant_message_id: Some("turn:turn-b:assistant:1".to_string()),
                text: "response B".to_string(),
                terminal: false,
            },
        );
        registry.finish("follow-up-b", "turn-b", TurnOutcome::Completed);

        // Assert
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::Error {
                turn_id,
                message,
                terminal: false
            }) if message == "follow-up A failed" && turn_id == "turn-a"
        ));
        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::FinalResponse {
                text,
                terminal: true,
                ..
            }) if text == "response B"
        ));
        completion.await.expect("completion sender");
    }

    #[tokio::test]
    async fn duplicate_registration_keeps_the_original_observer() {
        // Arrange
        let registry = TurnObserverRegistry::new();
        let mut owner = registry
            .register_if_absent("request-1".to_string())
            .expect("owner observer should be registered");

        // Act
        let duplicate = registry.register_if_absent("request-1".to_string());
        registry.emit("request-1", AgentEvent::Iteration { iteration: 1 });

        // Assert
        assert!(duplicate.is_none());
        assert!(matches!(
            owner.events.recv().await,
            Some(AgentEvent::Iteration { iteration: 1 })
        ));
    }
}

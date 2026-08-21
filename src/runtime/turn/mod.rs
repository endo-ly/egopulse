//! Runtime turn subsystem: durable representation, scheduling, dispatch, and progress.

pub(crate) mod dispatch;
pub(crate) mod progress;
pub(crate) mod scheduled;
pub(crate) mod scheduler;

pub(crate) use dispatch::{execute_observed_turn, execute_scheduled_turn};
pub(in crate::runtime) use dispatch::{
    recover_durable_state, rehydrate_origin_tracker, spawn_turn_dispatcher,
};
pub(crate) use progress::ToolProgressCoordinator;
pub(crate) use scheduled::{
    ScheduledTurn, canonical_request_hash, deserialize_scheduled_turn, serialize_scheduled_turn,
};
pub(crate) use scheduler::{
    ActiveTurnTracker, RejectReason, ScheduleResult, StopReason, SubmitOutcome, TurnScheduler,
    TurnTracker, evaluate_stop_conditions,
};

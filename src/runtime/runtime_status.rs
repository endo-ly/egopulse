//! In-memory live health summary for the EgoPulse runtime.
//!
//! `RuntimeStatus` provides a thread-safe, lock-protected snapshot of runtime
//! health: channel states, database connectivity, and a ring-buffer of recent
//! errors. It is designed for injection into `AppState` and periodic
//! serialization for observability endpoints.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::RwLock;

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Default capacity for the recent-errors ring buffer.
const DEFAULT_ERROR_CAPACITY: usize = 100;
/// Default capacity for the recent-turns ring buffer.
const DEFAULT_TURN_CAPACITY: usize = 100;

// ---------------------------------------------------------------------------
// Public (crate) types
// ---------------------------------------------------------------------------

/// Thread-safe in-memory health summary.
///
/// All mutation methods acquire a standard `std::sync::RwLock` internally so
/// callers do not need async-compatible primitives.
pub(crate) struct RuntimeStatus {
    inner: RwLock<RuntimeStatusInner>,
}

/// Immutable point-in-time copy suitable for serialization.
///
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StatusSnapshot {
    pub version: String,
    pub pid: u32,
    pub started_at: String,
    pub db_healthy: bool,
    pub channels: HashMap<String, ChannelHealth>,
    pub recent_errors: Vec<AuditError>,
    pub recent_turns: Vec<TurnRecord>,
}

/// Operational state of a single channel (web / discord / telegram / …).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum ChannelState {
    Starting,
    Running,
    Failed,
    Stopped,
}

impl fmt::Display for ChannelState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Failed => write!(f, "failed"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

/// Per-channel health record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ChannelHealth {
    pub state: ChannelState,
    pub last_error: Option<String>,
    pub last_activity: Option<String>,
}

/// A single error entry in the recent-errors ring buffer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AuditError {
    pub at: String,
    pub trace_id: String,
    pub error_kind: String,
    pub agent_id: String,
    pub channel: String,
    pub summary: String,
}

/// A single turn record in the recent-turns ring buffer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TurnRecord {
    pub trace_id: String,
    pub agent_id: String,
    pub channel: String,
    pub started_at: String,
    pub duration_secs: f64,
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// Internal mutable state
// ---------------------------------------------------------------------------

#[allow(dead_code)]
struct RuntimeStatusInner {
    started_at: chrono::DateTime<Utc>,
    pid: u32,
    version: String,
    db_healthy: bool,
    channels: HashMap<String, ChannelHealth>,
    recent_errors: VecDeque<AuditError>,
    error_capacity: usize,
    recent_turns: VecDeque<TurnRecord>,
    turn_capacity: usize,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

#[allow(dead_code)]
impl RuntimeStatus {
    /// Creates a new `RuntimeStatus` initialized with the current timestamp,
    /// process ID, and crate version.
    ///
    /// The database is assumed healthy at construction time, the channel map
    /// starts empty, and the error ring buffer has capacity 100.
    pub(crate) fn new() -> Self {
        Self {
            inner: RwLock::new(RuntimeStatusInner {
                started_at: Utc::now(),
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                db_healthy: true,
                channels: HashMap::new(),
                recent_errors: VecDeque::new(),
                error_capacity: DEFAULT_ERROR_CAPACITY,
                recent_turns: VecDeque::new(),
                turn_capacity: DEFAULT_TURN_CAPACITY,
            }),
        }
    }

    /// Sets the operational state of the named channel.
    ///
    /// If the channel has not been registered yet it is inserted;
    /// otherwise only `state` is updated, preserving `last_error` and
    /// `last_activity`.
    pub(crate) fn update_channel(&self, name: &str, state: ChannelState) {
        let mut guard = self.inner.write().expect("runtime_status lock");
        guard
            .channels
            .entry(name.to_string())
            .and_modify(|ch| ch.state = state.clone())
            .or_insert_with(|| ChannelHealth {
                state,
                last_error: None,
                last_activity: None,
            });
    }

    /// Marks the named channel as [`ChannelState::Failed`] and records the
    /// error message.
    pub(crate) fn update_channel_error(&self, name: &str, error_msg: &str) {
        let mut guard = self.inner.write().expect("runtime_status lock");
        guard
            .channels
            .entry(name.to_string())
            .and_modify(|ch| {
                ch.state = ChannelState::Failed;
                ch.last_error = Some(error_msg.to_string());
            })
            .or_insert_with(|| ChannelHealth {
                state: ChannelState::Failed,
                last_error: Some(error_msg.to_string()),
                last_activity: None,
            });
    }

    /// Updates the `last_activity` timestamp of the named channel to now.
    ///
    /// If the channel has not been registered yet it is inserted with state
    /// [`ChannelState::Starting`].
    pub(crate) fn touch_channel_activity(&self, name: &str) {
        let mut guard = self.inner.write().expect("runtime_status lock");
        let now = Utc::now().to_rfc3339();
        guard
            .channels
            .entry(name.to_string())
            .and_modify(|ch| {
                ch.last_activity = Some(now.clone());
            })
            .or_insert_with(|| ChannelHealth {
                state: ChannelState::Starting,
                last_error: None,
                last_activity: Some(now),
            });
    }

    /// Toggles the database health flag.
    pub(crate) fn set_db_healthy(&self, healthy: bool) {
        let mut guard = self.inner.write().expect("runtime_status lock");
        guard.db_healthy = healthy;
    }

    /// Appends an error to the ring buffer.
    ///
    /// If the buffer is at capacity the oldest entry is discarded.
    pub(crate) fn push_error(
        &self,
        trace_id: &str,
        error_kind: &str,
        agent_id: &str,
        channel: &str,
        summary: &str,
    ) {
        let mut guard = self.inner.write().expect("runtime_status lock");
        let entry = AuditError {
            at: Utc::now().to_rfc3339(),
            trace_id: trace_id.to_string(),
            error_kind: error_kind.to_string(),
            agent_id: agent_id.to_string(),
            channel: channel.to_string(),
            summary: summary.to_string(),
        };
        if guard.recent_errors.len() >= guard.error_capacity {
            guard.recent_errors.pop_front();
        }
        guard.recent_errors.push_back(entry);
    }

    /// Returns an independent point-in-time copy of the full runtime status.
    pub(crate) fn snapshot(&self) -> StatusSnapshot {
        let guard = self.inner.read().expect("runtime_status lock");
        StatusSnapshot {
            version: guard.version.clone(),
            pid: guard.pid,
            started_at: guard.started_at.to_rfc3339(),
            db_healthy: guard.db_healthy,
            channels: guard.channels.clone(),
            recent_errors: guard.recent_errors.iter().cloned().collect(),
            recent_turns: guard.recent_turns.iter().cloned().collect(),
        }
    }

    /// Returns a copy of all recent errors in chronological order (oldest
    /// first).
    pub(crate) fn recent_errors(&self) -> Vec<AuditError> {
        let guard = self.inner.read().expect("runtime_status lock");
        guard.recent_errors.iter().cloned().collect()
    }

    /// Appends a turn record to the ring buffer.
    ///
    /// If the buffer is at capacity the oldest entry is discarded.
    pub(crate) fn push_turn(
        &self,
        trace_id: &str,
        agent_id: &str,
        channel: &str,
        started_at: &str,
        duration_secs: f64,
        ok: bool,
    ) {
        let mut guard = self.inner.write().expect("runtime_status lock");
        let entry = TurnRecord {
            trace_id: trace_id.to_string(),
            agent_id: agent_id.to_string(),
            channel: channel.to_string(),
            started_at: started_at.to_string(),
            duration_secs,
            ok,
        };
        if guard.recent_turns.len() >= guard.turn_capacity {
            guard.recent_turns.pop_front();
        }
        guard.recent_turns.push_back(entry);
    }

    /// Returns a copy of all recent turns in chronological order (oldest
    /// first).
    pub(crate) fn recent_turns(&self) -> Vec<TurnRecord> {
        let guard = self.inner.read().expect("runtime_status lock");
        guard.recent_turns.iter().cloned().collect()
    }

    /// Returns the health record for the named channel, or `None` if the
    /// channel has never been registered.
    pub(crate) fn channel_health(&self, name: &str) -> Option<ChannelHealth> {
        let guard = self.inner.read().expect("runtime_status lock");
        guard.channels.get(name).cloned()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // 1. new_sets_initial_state
    #[test]
    fn new_sets_initial_state() {
        // Arrange & Act
        let status = RuntimeStatus::new();

        // Assert — started_at is recent (within last 5 s)
        let snapshot = status.snapshot();
        let now = Utc::now();
        let diff = now.signed_duration_since(
            chrono::DateTime::parse_from_rfc3339(&snapshot.started_at)
                .expect("valid rfc3339")
                .to_utc(),
        );
        assert!(
            diff.num_seconds() >= 0 && diff.num_seconds() < 5,
            "started_at should be within the last few seconds"
        );
        assert!(snapshot.pid > 0, "pid should be non-zero");
        assert!(!snapshot.version.is_empty(), "version should be set");
        assert!(snapshot.db_healthy, "db_healthy should default to true");
        assert!(snapshot.channels.is_empty(), "channels should start empty");
        assert!(
            snapshot.recent_errors.is_empty(),
            "recent_errors should start empty"
        );
        assert!(
            snapshot.recent_turns.is_empty(),
            "recent_turns should start empty"
        );
    }

    // 2. update_channel_sets_state
    #[test]
    fn update_channel_sets_state() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        status.update_channel("web", ChannelState::Running);

        // Assert
        let health = status.channel_health("web").expect("web channel");
        assert!(matches!(health.state, ChannelState::Running));
    }

    // 3. update_channel_error_sets_failed
    #[test]
    fn update_channel_error_sets_failed() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        status.update_channel_error("discord", "timeout");

        // Assert
        let health = status.channel_health("discord").expect("discord channel");
        assert!(matches!(health.state, ChannelState::Failed));
        assert_eq!(health.last_error.as_deref(), Some("timeout"));
    }

    // 4. touch_channel_activity_updates_timestamp
    #[test]
    fn touch_channel_activity_updates_timestamp() {
        // Arrange
        let status = RuntimeStatus::new();
        status.update_channel("web", ChannelState::Running);

        // Act
        status.touch_channel_activity("web");

        // Assert
        let health = status.channel_health("web").expect("web channel");
        let activity = health.last_activity.expect("last_activity should be set");
        // Verify it parses as valid RFC3339
        chrono::DateTime::parse_from_rfc3339(&activity).expect("should be valid rfc3339");
    }

    // 5. set_db_healthy_toggles
    #[test]
    fn set_db_healthy_toggles() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        status.set_db_healthy(false);

        // Assert
        assert!(!status.snapshot().db_healthy);
    }

    // 6. push_error_appends_to_ring_buffer
    #[test]
    fn push_error_appends_to_ring_buffer() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        status.push_error("t1", "timeout", "agent-a", "web", "request timed out");

        // Assert
        let errors = status.recent_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].trace_id, "t1");
    }

    // 7. push_error_respects_capacity
    #[test]
    fn push_error_respects_capacity() {
        // Arrange — build a status with capacity 5
        let status = RuntimeStatus::new();
        // Temporarily set capacity by directly accessing inner via a helper.
        // Since we cannot change the capacity after construction, we exercise
        // the default capacity of 100 and push 102 entries.
        // Instead, we test by creating a dedicated RuntimeStatus with a
        // constructor that accepts capacity.  We add a small helper for tests.
        //
        // Alternative: push 101 items into default (capacity=100) and verify
        // the oldest is discarded.
        for i in 0..102 {
            status.push_error(
                &format!("trace-{i}"),
                "kind",
                "agent",
                "web",
                &format!("error #{i}"),
            );
        }

        // Act
        let errors = status.recent_errors();

        // Assert — only the latest 100 entries survive
        assert_eq!(errors.len(), 100, "should cap at 100 entries");
        // The first surviving entry should be trace-2 (0 and 1 discarded)
        assert_eq!(errors[0].trace_id, "trace-2");
        assert_eq!(errors[99].trace_id, "trace-101");
    }

    // 8. push_error_records_all_fields
    #[test]
    fn push_error_records_all_fields() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        status.push_error("tid-42", "llm_error", "ego", "discord", "rate limited");

        // Assert
        let errors = status.recent_errors();
        assert_eq!(errors.len(), 1);
        let err = &errors[0];
        assert_eq!(err.trace_id, "tid-42");
        assert_eq!(err.error_kind, "llm_error");
        assert_eq!(err.agent_id, "ego");
        assert_eq!(err.channel, "discord");
        assert_eq!(err.summary, "rate limited");
        // Verify `at` is valid RFC3339
        chrono::DateTime::parse_from_rfc3339(&err.at).expect("at should be rfc3339");
    }

    // 9. snapshot_returns_independent_copy
    #[test]
    fn snapshot_returns_independent_copy() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        let snap = status.snapshot();
        status.set_db_healthy(false);
        status.update_channel("web", ChannelState::Running);

        // Assert — snapshot was taken before the mutations
        assert!(
            snap.db_healthy,
            "snapshot should reflect original db_healthy"
        );
        assert!(
            snap.channels.is_empty(),
            "snapshot should not reflect later channel additions"
        );
    }

    // 10. channel_health_returns_none_for_unknown
    #[test]
    fn channel_health_returns_none_for_unknown() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        let result = status.channel_health("nonexistent");

        // Assert
        assert!(result.is_none());
    }

    // 11. push_turn_appends_to_ring_buffer
    #[test]
    fn push_turn_appends_to_ring_buffer() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        status.push_turn("t1", "alice", "discord", "2025-01-01T00:00:00Z", 5.2, true);

        // Assert
        let turns = status.recent_turns();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].trace_id, "t1");
        assert_eq!(turns[0].agent_id, "alice");
        assert_eq!(turns[0].channel, "discord");
        assert_eq!(turns[0].started_at, "2025-01-01T00:00:00Z");
        assert!((turns[0].duration_secs - 5.2).abs() < f64::EPSILON);
        assert!(turns[0].ok);
    }

    // 12. push_turn_respects_capacity
    #[test]
    fn push_turn_respects_capacity() {
        // Arrange
        let status = RuntimeStatus::new();
        for i in 0..102 {
            status.push_turn(
                &format!("trace-{i}"),
                "agent",
                "web",
                "2025-01-01T00:00:00Z",
                1.0,
                i % 2 == 0,
            );
        }

        // Act
        let turns = status.recent_turns();

        // Assert
        assert_eq!(turns.len(), 100, "should cap at 100 entries");
        assert_eq!(turns[0].trace_id, "trace-2");
        assert_eq!(turns[99].trace_id, "trace-101");
    }

    // 13. push_turn_records_failure
    #[test]
    fn push_turn_records_failure() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        status.push_turn("tid-err", "bob", "cli", "2025-06-01T12:00:00Z", 0.5, false);

        // Assert
        let turns = status.recent_turns();
        assert_eq!(turns.len(), 1);
        assert!(!turns[0].ok);
    }

    // 14. recent_turns_preserves_order
    #[test]
    fn recent_turns_preserves_order() {
        // Arrange
        let status = RuntimeStatus::new();

        // Act
        status.push_turn("first", "a", "web", "2025-01-01T00:00:00Z", 1.0, true);
        status.push_turn("second", "b", "web", "2025-01-01T00:01:00Z", 2.0, true);

        // Assert
        let turns = status.recent_turns();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].trace_id, "first");
        assert_eq!(turns[1].trace_id, "second");
    }
}

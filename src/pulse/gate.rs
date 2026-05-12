//! Pulse Gate — duplicate/active-turn check before activation.

use std::sync::Arc;

use crate::storage::Database;

/// Result of gate evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GateDecision {
    /// The intention should proceed to activation.
    Allow,
    /// The intention was already processed (duplicate due_key).
    Duplicate,
    /// The agent currently has an active turn; defer until next tick.
    DeferActive,
}

/// Evaluate whether a due intention should pass through the gate.
///
/// Gate v1 checks:
/// 1. Has this due_key already been processed? (check `pulse_runs`)
/// 2. Is the agent currently in an active turn?
///
/// # Errors
/// Returns `StorageError` when the database query fails.
pub(crate) async fn evaluate_gate(
    db: &Arc<Database>,
    agent_id: &str,
    intention_id: &str,
    due_key: &str,
    is_active: bool,
) -> Result<GateDecision, crate::error::StorageError> {
    let agent_id = agent_id.to_string();
    let intention_id = intention_id.to_string();
    let due_key = due_key.to_string();

    let has_run = crate::storage::call_blocking(db.clone(), move |db| {
        db.has_pulse_due_run(&agent_id, &intention_id, &due_key)
    })
    .await?;

    if has_run {
        return Ok(GateDecision::Duplicate);
    }

    if is_active {
        return Ok(GateDecision::DeferActive);
    }

    Ok(GateDecision::Allow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (Arc::new(db), dir)
    }

    #[tokio::test]
    async fn gate_blocks_duplicate_due_key() {
        // Arrange
        let (db, _dir) = test_db();
        db.try_create_pulse_run("run-1", "agent-a", "int-1", "2025-01-01")
            .expect("create pulse run");

        // Act
        let decision = evaluate_gate(&db, "agent-a", "int-1", "2025-01-01", false)
            .await
            .expect("gate");

        // Assert
        assert_eq!(decision, GateDecision::Duplicate);
    }

    #[tokio::test]
    async fn gate_defers_active_agent_without_run_record() {
        // Arrange
        let (db, _dir) = test_db();

        // Act
        let decision = evaluate_gate(&db, "agent-a", "int-1", "2025-01-01", true)
            .await
            .expect("gate");

        // Assert: decision is DeferActive
        assert_eq!(decision, GateDecision::DeferActive);

        // Assert: no pulse_run record was created (due_key not consumed)
        let has_run = db
            .has_pulse_due_run("agent-a", "int-1", "2025-01-01")
            .expect("has run");
        assert!(!has_run, "defer should not consume the due_key");
    }

    #[tokio::test]
    async fn gate_allows_deferred_due_key_on_next_tick() {
        // Arrange
        let (db, _dir) = test_db();

        // Act: first call with active agent → DeferActive
        let first = evaluate_gate(&db, "agent-a", "int-1", "2025-01-01", true)
            .await
            .expect("gate first");
        assert_eq!(first, GateDecision::DeferActive);

        // Act: second call with inactive agent → Allow (proves due_key not consumed)
        let second = evaluate_gate(&db, "agent-a", "int-1", "2025-01-01", false)
            .await
            .expect("gate second");

        // Assert
        assert_eq!(second, GateDecision::Allow);
    }
}

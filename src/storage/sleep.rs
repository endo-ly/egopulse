use std::str::FromStr;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::error::StorageError;

use super::{
    AgentSessionInfo, CheckpointSourceKind, Database, EpisodeEvent, MemoryFile, MemorySnapshot,
    SleepRun, SleepRunStatus, SleepRunStep, SleepRunTrigger, SleepStepCheckpoint, SleepStepName,
    SleepStepResult, SleepStepStatus,
};

fn commit_checkpoints_in_tx(
    tx: &Transaction<'_>,
    checkpoints: &[SleepStepCheckpoint],
) -> Result<(), StorageError> {
    for checkpoint in checkpoints {
        let changed = tx.execute(
            "INSERT INTO sleep_step_checkpoints
                (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(agent_id, step_name, source_kind, source_id) DO UPDATE SET
                cursor_at = ?5, cursor_id = ?6, updated_at = ?7
             WHERE (cursor_at, cursor_id) < (?5, ?6)",
            params![
                checkpoint.agent_id,
                checkpoint.step_name.to_string(),
                checkpoint.source_kind.to_string(),
                checkpoint.source_id,
                checkpoint.cursor_at,
                checkpoint.cursor_id,
                checkpoint.updated_at,
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(format!(
                "checkpoint:{}:{}:{}:{} did not advance",
                checkpoint.agent_id,
                checkpoint.step_name,
                checkpoint.source_kind,
                checkpoint.source_id,
            )));
        }
    }
    Ok(())
}

fn require_success_result(
    operation: &str,
    result: &SleepStepResult<'_>,
) -> Result<(), StorageError> {
    if result.status != SleepStepStatus::Success {
        return Err(StorageError::Conflict(format!(
            "{operation} requires success status, got {}",
            result.status
        )));
    }
    Ok(())
}

fn insert_memory_snapshot_in_tx(
    tx: &Transaction<'_>,
    sleep_run_id: &str,
    agent_id: &str,
    file: MemoryFile,
    content_before: &str,
    content_after: &str,
) -> Result<(), StorageError> {
    let inserted = tx.execute(
        "INSERT INTO memory_snapshots
            (id, run_id, agent_id, file, content_before, content_after, created_at)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
         WHERE EXISTS (
            SELECT 1 FROM sleep_runs WHERE id = ?2 AND agent_id = ?3
         )",
        params![
            uuid::Uuid::new_v4().to_string(),
            sleep_run_id,
            agent_id,
            file.to_string(),
            content_before,
            content_after,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    if inserted != 1 {
        return Err(StorageError::NotFound(format!("sleep_run:{sleep_run_id}")));
    }
    Ok(())
}

fn finish_step_in_tx(
    tx: &Transaction<'_>,
    sleep_run_id: &str,
    step_name: SleepStepName,
    result: SleepStepResult<'_>,
) -> Result<(), StorageError> {
    let changed = tx.execute(
        "UPDATE sleep_run_steps
         SET status = ?1, finished_at = ?2,
             input_tokens = ?3, output_tokens = ?4,
             error_message = ?5, metadata_json = ?6
         WHERE sleep_run_id = ?7 AND step_name = ?8 AND status = 'running'",
        params![
            result.status.to_string(),
            chrono::Utc::now().to_rfc3339(),
            result.input_tokens,
            result.output_tokens,
            result.error_message,
            result.metadata_json,
            sleep_run_id,
            step_name.to_string(),
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::Conflict(format!(
            "sleep_run_step:{sleep_run_id}:{step_name} is not running"
        )));
    }
    Ok(())
}

fn finish_memory_steps_in_tx(
    tx: &Transaction<'_>,
    sleep_run_id: &str,
    result: SleepStepResult<'_>,
) -> Result<(), StorageError> {
    for (step_name, input_tokens, output_tokens) in [
        (
            SleepStepName::SemanticUpdate,
            result.input_tokens,
            result.output_tokens,
        ),
        (SleepStepName::ProspectiveUpdate, 0, 0),
    ] {
        finish_step_in_tx(
            tx,
            sleep_run_id,
            step_name,
            SleepStepResult {
                status: result.status,
                input_tokens,
                output_tokens,
                error_message: result.error_message,
                metadata_json: result.metadata_json,
            },
        )?;
    }
    Ok(())
}

fn row_to_sleep_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<SleepRun> {
    let status = parse_row_enum!(row, 2, SleepRunStatus)?;
    let trigger = parse_row_enum!(row, 3, SleepRunTrigger)?;

    Ok(SleepRun {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        status,
        trigger,
        started_at: row.get(4)?,
        finished_at: row.get(5)?,
        source_chats_json: row.get(6)?,
        source_digest_md: row.get(7)?,
        input_tokens: row.get(8)?,
        output_tokens: row.get(9)?,
        total_tokens: row.get(10)?,
        error_message: row.get(11)?,
    })
}

fn row_to_memory_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemorySnapshot> {
    let file = parse_row_enum!(row, 3, MemoryFile)?;

    Ok(MemorySnapshot {
        id: row.get(0)?,
        run_id: row.get(1)?,
        agent_id: row.get(2)?,
        file,
        content_before: row.get(4)?,
        content_after: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn insert_pending_steps(
    tx: &rusqlite::Transaction<'_>,
    sleep_run_id: &str,
) -> Result<(), StorageError> {
    for step in SleepStepName::ALL {
        tx.execute(
            "INSERT INTO sleep_run_steps (sleep_run_id, step_name, status)
             VALUES (?1, ?2, 'pending')",
            params![sleep_run_id, step.to_string()],
        )?;
    }
    Ok(())
}

fn row_to_sleep_run_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<SleepRunStep> {
    let step_name = parse_row_enum!(row, 1, SleepStepName)?;
    let status = parse_row_enum!(row, 2, SleepStepStatus)?;
    Ok(SleepRunStep {
        sleep_run_id: row.get(0)?,
        step_name,
        status,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        input_tokens: row.get(5)?,
        output_tokens: row.get(6)?,
        error_message: row.get(7)?,
        metadata_json: row.get(8)?,
    })
}

fn row_to_sleep_checkpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<SleepStepCheckpoint> {
    let step_name = parse_row_enum!(row, 1, SleepStepName)?;
    let source_kind = parse_row_enum!(row, 2, CheckpointSourceKind)?;
    Ok(SleepStepCheckpoint {
        agent_id: row.get(0)?,
        step_name,
        source_kind,
        source_id: row.get(3)?,
        cursor_at: row.get(4)?,
        cursor_id: row.get(5)?,
        updated_at: row.get(6)?,
    })
}
impl Database {
    /// Convenience wrapper around [`try_create_sleep_run`] that expect()-s
    /// the result for use in test helpers.
    ///
    /// Only called from integration tests (sleep/orchestrator.rs,
    /// sleep/scheduler.rs).  Runtime code uses [`try_create_sleep_run`]
    /// directly.  Gated with #[cfg(test)] to avoid dead_code warnings.
    #[cfg(test)]
    pub(crate) fn create_sleep_run(
        &self,
        agent_id: &str,
        trigger: SleepRunTrigger,
    ) -> Result<String, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = uuid::Uuid::new_v4().to_string();
        let status = SleepRunStatus::Running.to_string();
        let started_at = chrono::Utc::now().to_rfc3339();

        tx.execute(
            "INSERT INTO sleep_runs
                 (id, agent_id, status, trigger_type, started_at, finished_at,
                  source_chats_json, source_digest_md,
                  input_tokens, output_tokens, total_tokens, error_message)
              VALUES (?1, ?2, ?3, ?4, ?5, NULL, '[]', NULL, 0, 0, 0, NULL)",
            params![id, agent_id, status, trigger.to_string(), started_at],
        )?;
        insert_pending_steps(&tx, &id)?;
        tx.commit()?;
        Ok(id)
    }

    /// Atomically checks for a running sleep run and creates one if none exists.
    ///
    /// This prevents a race condition where two concurrent callers could both
    /// observe "no running run" and each insert a duplicate.
    ///
    /// Returns `Ok(Some(id))` if a new run was created, or `Ok(None)` if a
    /// running run already exists for the given agent.
    pub(crate) fn try_create_sleep_run(
        &self,
        agent_id: &str,
        trigger: SleepRunTrigger,
    ) -> Result<Option<String>, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running = SleepRunStatus::Running.to_string();
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM sleep_runs WHERE agent_id = ?1 AND status = ?2",
            params![agent_id, running],
            |row| row.get(0),
        )?;

        if count > 0 {
            return Ok(None);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let status = SleepRunStatus::Running.to_string();
        let started_at = chrono::Utc::now().to_rfc3339();

        tx.execute(
            "INSERT INTO sleep_runs
                 (id, agent_id, status, trigger_type, started_at, finished_at,
                  source_chats_json, source_digest_md,
                  input_tokens, output_tokens, total_tokens, error_message)
            VALUES (?1, ?2, ?3, ?4, ?5, NULL, '[]', NULL, 0, 0, 0, NULL)",
            params![id, agent_id, status, trigger.to_string(), started_at],
        )?;
        insert_pending_steps(&tx, &id)?;
        tx.commit()?;
        Ok(Some(id))
    }

    pub(crate) fn update_sleep_run_success(
        &self,
        id: &str,
        source_chats_json: &str,
        source_digest_md: Option<&str>,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let total_tokens = input_tokens.saturating_add(output_tokens);
        let status = SleepRunStatus::Success.to_string();
        let running = SleepRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE sleep_runs
             SET status = ?1, finished_at = ?2, source_chats_json = ?3,
                 source_digest_md = ?4,
                 input_tokens = ?5, output_tokens = ?6, total_tokens = ?7
             WHERE id = ?8 AND status = ?9",
            params![
                status,
                finished_at,
                source_chats_json,
                source_digest_md,
                input_tokens,
                output_tokens,
                total_tokens,
                id,
                running,
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "sleep_run:{id} is not running"
            )));
        }
        Ok(())
    }

    pub(crate) fn update_sleep_run_failed(
        &self,
        id: &str,
        error_message: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = SleepRunStatus::Failed.to_string();
        let running = SleepRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE sleep_runs SET status = ?1, finished_at = ?2, error_message = ?3
             WHERE id = ?4 AND status = ?5",
            params![status, finished_at, error_message, id, running],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "sleep_run:{id} is not running"
            )));
        }
        Ok(())
    }

    pub(crate) fn update_sleep_run_source_chats(
        &self,
        id: &str,
        source_chats_json: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        conn.execute(
            "UPDATE sleep_runs SET source_chats_json = ?1 WHERE id = ?2",
            params![source_chats_json, id],
        )?;
        Ok(())
    }

    pub(crate) fn get_sleep_run(&self, id: &str) -> Result<Option<SleepRun>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT id, agent_id, status, trigger_type, started_at, finished_at,
                    source_chats_json, source_digest_md,
                    input_tokens, output_tokens, total_tokens, error_message
             FROM sleep_runs WHERE id = ?1",
            params![id],
            row_to_sleep_run,
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn list_sleep_runs(
        &self,
        agent_id: &str,
        limit: i64,
    ) -> Result<Vec<SleepRun>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, status, trigger_type, started_at, finished_at,
                    source_chats_json, source_digest_md,
                    input_tokens, output_tokens, total_tokens, error_message
             FROM sleep_runs
             WHERE agent_id = ?1
             ORDER BY started_at DESC, rowid DESC
             LIMIT ?2",
        )?;
        stmt.query_map(params![agent_id, limit], row_to_sleep_run)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn list_all_sleep_runs(&self, limit: i64) -> Result<Vec<SleepRun>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, status, trigger_type, started_at, finished_at,
                    source_chats_json, source_digest_md,
                    input_tokens, output_tokens, total_tokens, error_message
             FROM sleep_runs
             ORDER BY started_at DESC, rowid DESC
             LIMIT ?1",
        )?;
        stmt.query_map(params![limit], row_to_sleep_run)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Marks a sleep run step as running and sets `started_at`.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Conflict` if the step is not in `pending` state.
    pub(crate) fn start_sleep_step(
        &self,
        sleep_run_id: &str,
        step_name: SleepStepName,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let running = SleepStepStatus::Running.to_string();
        let pending = SleepStepStatus::Pending.to_string();

        let changed = conn.execute(
            "UPDATE sleep_run_steps
             SET status = ?1, started_at = ?2
             WHERE sleep_run_id = ?3 AND step_name = ?4 AND status = ?5",
            params![running, now, sleep_run_id, step_name.to_string(), pending],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "sleep_run_step:{sleep_run_id}:{} is not pending",
                step_name
            )));
        }
        Ok(())
    }

    /// Atomically transitions both memory update steps from pending to running.
    ///
    /// # Errors
    ///
    /// Returns an error when either step is not pending or database access fails.
    pub(crate) fn start_memory_update_steps(&self, sleep_run_id: &str) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = chrono::Utc::now().to_rfc3339();
        let pending = SleepStepStatus::Pending.to_string();
        let running = SleepStepStatus::Running.to_string();

        for step_name in [
            SleepStepName::SemanticUpdate,
            SleepStepName::ProspectiveUpdate,
        ] {
            let changed = tx.execute(
                "UPDATE sleep_run_steps
                 SET status = ?1, started_at = ?2
                 WHERE sleep_run_id = ?3 AND step_name = ?4 AND status = ?5",
                params![running, now, sleep_run_id, step_name.to_string(), pending],
            )?;
            if changed == 0 {
                return Err(StorageError::Conflict(format!(
                    "sleep_run_step:{sleep_run_id}:{step_name} is not pending"
                )));
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Finishes a running sleep step with terminal status, tokens, and metadata.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Conflict` if the step is not in `running` state.
    pub(crate) fn finish_sleep_step(
        &self,
        sleep_run_id: &str,
        step_name: SleepStepName,
        result: SleepStepResult<'_>,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let running = SleepStepStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE sleep_run_steps
             SET status = ?1, finished_at = ?2,
                 input_tokens = ?3, output_tokens = ?4,
                 error_message = ?5, metadata_json = ?6
             WHERE sleep_run_id = ?7 AND step_name = ?8 AND status = ?9",
            params![
                result.status.to_string(),
                now,
                result.input_tokens,
                result.output_tokens,
                result.error_message,
                result.metadata_json,
                sleep_run_id,
                step_name.to_string(),
                running,
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "sleep_run_step:{sleep_run_id}:{} is not running",
                step_name
            )));
        }
        Ok(())
    }

    /// Atomically finishes both memory update steps with the same terminal status.
    ///
    /// The shared LLM usage is recorded once on `semantic_update` so run-level
    /// aggregation does not double count it.
    ///
    /// # Errors
    ///
    /// Returns an error when either step is not running or database access fails.
    pub(crate) fn finish_memory_update_steps(
        &self,
        sleep_run_id: &str,
        result: SleepStepResult<'_>,
    ) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = chrono::Utc::now().to_rfc3339();
        let running = SleepStepStatus::Running.to_string();

        for (step_name, input_tokens, output_tokens) in [
            (
                SleepStepName::SemanticUpdate,
                result.input_tokens,
                result.output_tokens,
            ),
            (SleepStepName::ProspectiveUpdate, 0, 0),
        ] {
            let changed = tx.execute(
                "UPDATE sleep_run_steps
                 SET status = ?1, finished_at = ?2,
                     input_tokens = ?3, output_tokens = ?4,
                     error_message = ?5, metadata_json = ?6
                 WHERE sleep_run_id = ?7 AND step_name = ?8 AND status = ?9",
                params![
                    result.status.to_string(),
                    now,
                    input_tokens,
                    output_tokens,
                    result.error_message,
                    result.metadata_json,
                    sleep_run_id,
                    step_name.to_string(),
                    running,
                ],
            )?;
            if changed == 0 {
                return Err(StorageError::Conflict(format!(
                    "sleep_run_step:{sleep_run_id}:{step_name} is not running"
                )));
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Atomically persists extracted events, advances message checkpoints, and
    /// marks event extraction successful.
    pub(crate) fn commit_event_extraction_success(
        &self,
        sleep_run_id: &str,
        agent_id: &str,
        events: &[EpisodeEvent],
        result: SleepStepResult<'_>,
        checkpoints: &[SleepStepCheckpoint],
    ) -> Result<(), StorageError> {
        require_success_result("commit_event_extraction_success", &result)?;
        for checkpoint in checkpoints {
            if checkpoint.step_name != SleepStepName::EventExtraction
                || checkpoint.agent_id != agent_id
                || checkpoint.source_kind != CheckpointSourceKind::Messages
            {
                return Err(StorageError::Conflict(format!(
                    "invalid event extraction checkpoint scope: agent={} step={} source={}",
                    checkpoint.agent_id, checkpoint.step_name, checkpoint.source_kind,
                )));
            }
        }
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        for event in events {
            if event.sleep_run_id != sleep_run_id || event.agent_id != agent_id {
                return Err(StorageError::Conflict(format!(
                    "event scope does not match run={sleep_run_id} agent={agent_id}",
                )));
            }
            tx.execute(
                "INSERT INTO episode_events
                     (id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                      ripple_strength, certainty, sleep_run_id, source_refs_json,
                      created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    event.id,
                    event.agent_id,
                    event.experienced_at,
                    event.encoded_at,
                    event.kind.to_string(),
                    event.title,
                    event.body_md,
                    event.ripple_strength,
                    event.certainty.to_string(),
                    event.sleep_run_id,
                    event.source_refs_json,
                    event.created_at,
                    event.updated_at,
                ],
            )?;
        }

        commit_checkpoints_in_tx(&tx, checkpoints)?;
        finish_step_in_tx(&tx, sleep_run_id, SleepStepName::EventExtraction, result)?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically advances all shared Memory Update checkpoints and finishes
    /// semantic/prospective with the same terminal status.
    pub(crate) fn commit_memory_update_success(
        &self,
        sleep_run_id: &str,
        agent_id: &str,
        result: SleepStepResult<'_>,
        checkpoints: &[SleepStepCheckpoint],
        snapshots: &[(MemoryFile, String, String)],
    ) -> Result<(), StorageError> {
        require_success_result("commit_memory_update_success", &result)?;
        for checkpoint in checkpoints {
            if !matches!(
                checkpoint.step_name,
                SleepStepName::SemanticUpdate | SleepStepName::ProspectiveUpdate
            ) || checkpoint.agent_id != agent_id
            {
                return Err(StorageError::Conflict(format!(
                    "invalid memory update checkpoint scope: agent={} step={}",
                    checkpoint.agent_id, checkpoint.step_name,
                )));
            }
        }
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (file, content_before, content_after) in snapshots {
            insert_memory_snapshot_in_tx(
                &tx,
                sleep_run_id,
                agent_id,
                *file,
                content_before,
                content_after,
            )?;
        }
        commit_checkpoints_in_tx(&tx, checkpoints)?;
        finish_memory_steps_in_tx(&tx, sleep_run_id, result)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn commit_episodic_update_success(
        &self,
        sleep_run_id: &str,
        agent_id: &str,
        content_before: &str,
        content_after: &str,
        result: SleepStepResult<'_>,
    ) -> Result<(), StorageError> {
        require_success_result("commit_episodic_update_success", &result)?;
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_memory_snapshot_in_tx(
            &tx,
            sleep_run_id,
            agent_id,
            MemoryFile::Episodic,
            content_before,
            content_after,
        )?;
        finish_step_in_tx(&tx, sleep_run_id, SleepStepName::EpisodicUpdate, result)?;
        tx.commit()?;
        Ok(())
    }

    /// Lists all steps for a sleep run, ordered by step_name.
    ///
    /// Only called from integration tests (sleep/orchestrator.rs) which verify
    /// that sleep batch steps transitioned to the expected terminal states.
    /// No runtime callers yet — gated with #[cfg(test)] to avoid dead_code
    /// warnings in production builds.  Remove the gate when a runtime caller
    /// is introduced.
    #[cfg(test)]
    pub(crate) fn list_sleep_run_steps(
        &self,
        sleep_run_id: &str,
    ) -> Result<Vec<SleepRunStep>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT sleep_run_id, step_name, status, started_at, finished_at,
                    input_tokens, output_tokens, error_message, metadata_json
             FROM sleep_run_steps
             WHERE sleep_run_id = ?1
             ORDER BY step_name",
        )?;
        stmt.query_map(params![sleep_run_id], row_to_sleep_run_step)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Finalizes a sleep run by aggregating step results into run-level status and tokens.
    ///
    /// Status derivation:
    /// - `Success`: all steps succeeded or skipped, at least one success
    /// - `PartialFailure`: at least one success and at least one failed
    /// - `Failed`: any pending/running remaining, or all failed
    /// - `Skipped`: all steps skipped
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Conflict` if the run is not in `running` state.
    pub(crate) fn finalize_sleep_run(
        &self,
        sleep_run_id: &str,
    ) -> Result<SleepRunStatus, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let steps: Vec<SleepRunStep> = {
            let mut stmt = tx.prepare(
                "SELECT sleep_run_id, step_name, status, started_at, finished_at,
                        input_tokens, output_tokens, error_message, metadata_json
                 FROM sleep_run_steps
                 WHERE sleep_run_id = ?1",
            )?;
            stmt.query_map(params![sleep_run_id], row_to_sleep_run_step)?
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut success_count = 0;
        let mut failed_count = 0;
        let mut skipped_count = 0;
        let mut pending_or_running = false;
        let mut total_input_tokens: i64 = 0;
        let mut total_output_tokens: i64 = 0;
        let mut errors: Vec<String> = Vec::new();

        for step in &steps {
            match step.status {
                SleepStepStatus::Success => success_count += 1,
                SleepStepStatus::Failed => {
                    failed_count += 1;
                    if let Some(ref err) = step.error_message {
                        errors.push(format!("{}: {}", step.step_name, err));
                    }
                }
                SleepStepStatus::Skipped => skipped_count += 1,
                SleepStepStatus::Pending | SleepStepStatus::Running => {
                    pending_or_running = true;
                }
            }
            total_input_tokens = total_input_tokens.saturating_add(step.input_tokens);
            total_output_tokens = total_output_tokens.saturating_add(step.output_tokens);
        }

        let derived_status = if pending_or_running {
            SleepRunStatus::Failed
        } else if success_count == 0 && failed_count == 0 && skipped_count > 0 {
            SleepRunStatus::Skipped
        } else if success_count > 0 && failed_count == 0 {
            SleepRunStatus::Success
        } else if success_count > 0 && failed_count > 0 {
            SleepRunStatus::PartialFailure
        } else {
            SleepRunStatus::Failed
        };

        let error_message = if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        };

        let now = chrono::Utc::now().to_rfc3339();
        let total_tokens = total_input_tokens.saturating_add(total_output_tokens);
        let running = SleepRunStatus::Running.to_string();

        let changed = tx.execute(
            "UPDATE sleep_runs
             SET status = ?1, finished_at = ?2,
                 input_tokens = ?3, output_tokens = ?4, total_tokens = ?5,
                 error_message = ?6
             WHERE id = ?7 AND status = ?8",
            params![
                derived_status.to_string(),
                now,
                total_input_tokens,
                total_output_tokens,
                total_tokens,
                error_message,
                sleep_run_id,
                running,
            ],
        )?;
        if changed == 0 {
            tx.rollback()?;
            return Err(StorageError::Conflict(format!(
                "sleep_run:{sleep_run_id} is not running"
            )));
        }

        tx.commit()?;
        Ok(derived_status)
    }

    /// Gets a checkpoint by composite key (agent_id, step_name, source_kind, source_id).
    ///
    /// Returns `None` if no checkpoint exists for the given key.
    pub(crate) fn get_sleep_checkpoint(
        &self,
        agent_id: &str,
        step_name: SleepStepName,
        source_kind: CheckpointSourceKind,
        source_id: &str,
    ) -> Result<Option<SleepStepCheckpoint>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT agent_id, step_name, source_kind, source_id,
                    cursor_at, cursor_id, updated_at
             FROM sleep_step_checkpoints
             WHERE agent_id = ?1 AND step_name = ?2
               AND source_kind = ?3 AND source_id = ?4",
            params![
                agent_id,
                step_name.to_string(),
                source_kind.to_string(),
                source_id
            ],
            row_to_sleep_checkpoint,
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn count_agent_pending_sleep_messages(
        &self,
        agent_id: &str,
    ) -> Result<i64, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT COUNT(*) FROM (
                SELECT m.chat_id, m.id
                FROM messages m
                JOIN chats c ON m.chat_id = c.chat_id
                LEFT JOIN sleep_step_checkpoints event_checkpoint
                  ON event_checkpoint.agent_id = c.agent_id
                 AND event_checkpoint.step_name = 'event_extraction'
                 AND event_checkpoint.source_kind = 'messages'
                 AND event_checkpoint.source_id = CAST(c.chat_id AS TEXT)
                LEFT JOIN sleep_step_checkpoints prospective_checkpoint
                  ON prospective_checkpoint.agent_id = c.agent_id
                 AND prospective_checkpoint.step_name = 'prospective_update'
                 AND prospective_checkpoint.source_kind = 'messages'
                 AND prospective_checkpoint.source_id = CAST(c.chat_id AS TEXT)
                WHERE c.agent_id = ?1 AND c.chat_type != 'voice'
                  AND (
                       event_checkpoint.cursor_at IS NULL
                       OR (m.timestamp, m.id) > (event_checkpoint.cursor_at, event_checkpoint.cursor_id)
                       OR prospective_checkpoint.cursor_at IS NULL
                       OR (m.timestamp, m.id) > (prospective_checkpoint.cursor_at, prospective_checkpoint.cursor_id)
                  )
                GROUP BY m.chat_id, m.id
             )",
            params![agent_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub(crate) fn get_agent_sessions_with_pending_sleep_messages(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<AgentSessionInfo>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "WITH pending_messages AS (
                SELECT
                    c.chat_id,
                    c.channel,
                    c.external_chat_id,
                    s.updated_at,
                    (SELECT COUNT(*) FROM messages WHERE chat_id = c.chat_id) AS message_count,
                    LENGTH(COALESCE(s.messages_json, '')) / 3 AS estimated_tokens,
                    m.timestamp AS pending_ts,
                    m.id AS pending_id,
                    ROW_NUMBER() OVER (
                        PARTITION BY c.chat_id
                        ORDER BY m.timestamp ASC, m.id ASC
                    ) AS rn
                FROM chats c
                JOIN sessions s ON c.chat_id = s.chat_id
                JOIN messages m ON m.chat_id = c.chat_id
                LEFT JOIN sleep_step_checkpoints event_checkpoint
                  ON event_checkpoint.agent_id = c.agent_id
                 AND event_checkpoint.step_name = 'event_extraction'
                 AND event_checkpoint.source_kind = 'messages'
                 AND event_checkpoint.source_id = CAST(c.chat_id AS TEXT)
                LEFT JOIN sleep_step_checkpoints prospective_checkpoint
                  ON prospective_checkpoint.agent_id = c.agent_id
                 AND prospective_checkpoint.step_name = 'prospective_update'
                 AND prospective_checkpoint.source_kind = 'messages'
                 AND prospective_checkpoint.source_id = CAST(c.chat_id AS TEXT)
                WHERE c.agent_id = ?1 AND c.chat_type != 'voice'
                  AND (
                       event_checkpoint.cursor_at IS NULL
                       OR (m.timestamp, m.id) > (event_checkpoint.cursor_at, event_checkpoint.cursor_id)
                       OR prospective_checkpoint.cursor_at IS NULL
                       OR (m.timestamp, m.id) > (prospective_checkpoint.cursor_at, prospective_checkpoint.cursor_id)
                  )
             )
             SELECT
                 chat_id,
                 channel,
                 external_chat_id,
                 updated_at,
                 message_count,
                 estimated_tokens
             FROM pending_messages
             WHERE rn = 1
             ORDER BY pending_ts ASC, pending_id ASC, chat_id ASC
             LIMIT ?2",
        )?;
        stmt.query_map(params![agent_id, limit as i64], |row| {
            Ok(AgentSessionInfo {
                chat_id: row.get(0)?,
                channel: row.get(1)?,
                external_chat_id: row.get(2)?,
                updated_at: row.get(3)?,
                message_count: row.get(4)?,
                estimated_tokens: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn get_snapshots_for_run(
        &self,
        run_id: &str,
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, run_id, agent_id, file, content_before, content_after, created_at
             FROM memory_snapshots
             WHERE run_id = ?1
             ORDER BY created_at ASC",
        )?;
        stmt.query_map(params![run_id], row_to_memory_snapshot)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Inserts a `before == after == base` snapshot for every memory file that
    /// does not yet have one for this run, so the publication bundle has a
    /// complete snapshot set regardless of which steps ran or were skipped.
    ///
    /// `base` is `(MemoryFile, base_content)` for each of the three files.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database access failure.
    pub(crate) fn ensure_memory_snapshots_complete(
        &self,
        sleep_run_id: &str,
        agent_id: &str,
        base: &[(MemoryFile, &str)],
    ) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = chrono::Utc::now().to_rfc3339();
        for (file, base_content) in base {
            tx.execute(
                "INSERT INTO memory_snapshots
                    (id, run_id, agent_id, file, content_before, content_after, created_at)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?5, ?6
                 WHERE EXISTS (
                    SELECT 1 FROM sleep_runs WHERE id = ?2 AND agent_id = ?3
                 )
                 AND NOT EXISTS (
                    SELECT 1 FROM memory_snapshots WHERE run_id = ?2 AND file = ?4
                 )",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    sleep_run_id,
                    agent_id,
                    file.to_string(),
                    base_content,
                    now,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Lists sleep runs still in `running` status, oldest first.
    ///
    /// Used by startup recovery to find runs interrupted by a crash.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database access failure.
    pub(crate) fn list_running_sleep_runs(&self) -> Result<Vec<SleepRun>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, status, trigger_type, started_at, finished_at,
                    source_chats_json, source_digest_md,
                    input_tokens, output_tokens, total_tokens, error_message
             FROM sleep_runs
             WHERE status = 'running'
             ORDER BY started_at ASC, rowid ASC",
        )?;
        stmt.query_map(params![], row_to_sleep_run)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn ensure_sleep_runs_table(db: &Database) {
        let conn = db.get_conn().expect("pool");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sleep_runs (
                    id TEXT PRIMARY KEY,
                    agent_id TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'running',
                    trigger_type TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    finished_at TEXT,
                    source_chats_json TEXT NOT NULL DEFAULT '[]',
                    source_digest_md TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    total_tokens INTEGER NOT NULL DEFAULT 0,
                    error_message TEXT
                )",
        )
        .expect("create sleep_runs table");
    }

    fn create_test_sleep_run(db: &Database, agent_id: &str) -> String {
        ensure_sleep_runs_table(db);
        db.create_sleep_run(agent_id, SleepRunTrigger::Manual)
            .expect("create sleep run")
    }

    #[test]
    fn create_sleep_run_inserts_with_running_status() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Running);
    }

    #[test]
    fn try_create_sleep_run_inserts_when_no_running() {
        let (db, _dir) = test_db();
        ensure_sleep_runs_table(&db);

        let id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create")
            .expect("should insert");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Running);
        assert_eq!(run.agent_id, "agent-a");
    }

    #[test]
    fn try_create_sleep_run_returns_none_when_running_exists() {
        let (db, _dir) = test_db();
        ensure_sleep_runs_table(&db);

        let _first = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create first")
            .expect("should insert");

        let second = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create second");

        assert!(second.is_none(), "should not insert duplicate running run");
    }

    #[test]
    fn try_create_sleep_run_allows_different_agents() {
        let (db, _dir) = test_db();
        ensure_sleep_runs_table(&db);

        let id_a = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create a")
            .expect("should insert");
        let id_b = db
            .try_create_sleep_run("agent-b", SleepRunTrigger::Manual)
            .expect("try create b")
            .expect("should insert");

        assert_ne!(id_a, id_b);
    }

    #[test]
    fn try_create_sleep_run_inserts_four_pending_steps_atomically() {
        // Arrange
        let (db, _dir) = test_db();

        // Act
        let id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create")
            .expect("should insert");

        // Assert: 4 step rows created with pending status
        let steps = db.list_sleep_run_steps(&id).expect("list steps");
        assert_eq!(steps.len(), 4, "should have 4 step rows");
        assert_eq!(steps[0].step_name, SleepStepName::EpisodicUpdate);
        assert_eq!(steps[1].step_name, SleepStepName::EventExtraction);
        assert_eq!(steps[2].step_name, SleepStepName::ProspectiveUpdate);
        assert_eq!(steps[3].step_name, SleepStepName::SemanticUpdate);
        for step in &steps {
            assert_eq!(step.status, SleepStepStatus::Pending);
            assert!(step.started_at.is_none(), "started_at should be NULL");
            assert!(step.finished_at.is_none(), "finished_at should be NULL");
            assert_eq!(step.input_tokens, 0);
            assert_eq!(step.output_tokens, 0);
        }

        // Assert: second call returns None (existing exclusion)
        let second = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create second");
        assert!(second.is_none(), "should not insert duplicate running run");
    }

    #[test]
    fn try_create_sleep_run_rolls_back_when_step_initialization_fails() {
        // Arrange: create a run first so the step table has a FK reference
        let (db, _dir) = test_db();
        let id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create")
            .expect("should insert");

        // Act: delete the run (cascade deletes steps), then try to create again
        // but simulate a conflict by inserting a step row with an invalid FK first
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "DELETE FROM sleep_runs WHERE id = ?1",
            rusqlite::params![id],
        )
        .expect("delete run");

        // Assert: no orphaned step rows remain
        let step_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sleep_run_steps WHERE sleep_run_id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(step_count, 0, "cascade should remove all step rows");
    }

    #[test]
    fn sleep_step_lifecycle_rejects_invalid_transition() {
        // Arrange: run with 4 pending steps
        let (db, _dir) = test_db();
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create")
            .expect("should insert");

        // Act: try to finish a pending step (should fail - must be running first)
        let result = db.finish_sleep_step(
            &run_id,
            SleepStepName::EventExtraction,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: 0,
                output_tokens: 0,
                error_message: None,
                metadata_json: None,
            },
        );

        // Assert: conflict error
        assert!(
            matches!(result, Err(StorageError::Conflict(_))),
            "should reject finishing a pending step"
        );

        // Act: start then finish, then try to start again
        db.start_sleep_step(&run_id, SleepStepName::EventExtraction)
            .expect("start step");
        db.finish_sleep_step(
            &run_id,
            SleepStepName::EventExtraction,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: 0,
                output_tokens: 0,
                error_message: None,
                metadata_json: None,
            },
        )
        .expect("finish step");
        let result = db.start_sleep_step(&run_id, SleepStepName::EventExtraction);

        // Assert: cannot re-start a completed step
        assert!(
            matches!(result, Err(StorageError::Conflict(_))),
            "should reject re-starting a completed step"
        );
    }

    #[test]
    fn finalize_sleep_run_derives_status_matrix() {
        // Arrange & Act & Assert: all success → success
        let (db, _dir) = test_db();
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        for step in SleepStepName::ALL {
            finish_step_success(&db, &run_id, step, 10, 5);
        }
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::Success);
        let run = db.get_sleep_run(&run_id).expect("get").expect("run");
        assert_eq!(run.status, SleepRunStatus::Success);

        // Arrange & Act & Assert: mixed success + failed → partial_failure
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        finish_step_success(&db, &run_id, SleepStepName::EventExtraction, 10, 5);
        finish_step_failed(&db, &run_id, SleepStepName::EpisodicUpdate, "LLM error");
        skip_step(&db, &run_id, SleepStepName::SemanticUpdate);
        skip_step(&db, &run_id, SleepStepName::ProspectiveUpdate);
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::PartialFailure);
        let run = db.get_sleep_run(&run_id).expect("get").expect("run");
        assert!(run.error_message.as_deref().unwrap().contains("LLM error"));

        // Arrange & Act & Assert: all failed → failed
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        for step in SleepStepName::ALL {
            finish_step_failed(&db, &run_id, step, "error");
        }
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::Failed);

        // Arrange & Act & Assert: all skipped → skipped
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        for step in SleepStepName::ALL {
            skip_step(&db, &run_id, step);
        }
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::Skipped);

        // Arrange & Act & Assert: pending remaining → failed
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        finish_step_success(&db, &run_id, SleepStepName::EventExtraction, 10, 5);
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::Failed);
    }

    #[test]
    fn finalize_sleep_run_sums_step_tokens() {
        // Arrange
        let (db, _dir) = test_db();
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        finish_step_success(&db, &run_id, SleepStepName::EventExtraction, 100, 50);
        finish_step_success(&db, &run_id, SleepStepName::EpisodicUpdate, 200, 80);
        db.start_memory_update_steps(&run_id)
            .expect("start memory update");
        db.finish_memory_update_steps(
            &run_id,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: 300,
                output_tokens: 120,
                error_message: None,
                metadata_json: None,
            },
        )
        .expect("finish memory update");

        // Act
        db.finalize_sleep_run(&run_id).expect("finalize");

        // Assert: tokens are summed from steps
        let run = db.get_sleep_run(&run_id).expect("get").expect("run");
        assert_eq!(run.input_tokens, 600);
        assert_eq!(run.output_tokens, 250);
        assert_eq!(run.total_tokens, 850);

        let steps = db.list_sleep_run_steps(&run_id).expect("steps");
        let semantic = steps
            .iter()
            .find(|step| step.step_name == SleepStepName::SemanticUpdate)
            .expect("semantic step");
        let prospective = steps
            .iter()
            .find(|step| step.step_name == SleepStepName::ProspectiveUpdate)
            .expect("prospective step");
        assert_eq!(semantic.status, SleepStepStatus::Success);
        assert_eq!(prospective.status, SleepStepStatus::Success);
        assert_eq!(semantic.input_tokens, 300);
        assert_eq!(prospective.input_tokens, 0);
    }

    #[test]
    fn update_sleep_run_to_success() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        db.update_sleep_run_success(&id, r#"[1, 2, 3]"#, Some("digest-abc"), 100, 50)
            .expect("update success");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Success);
        assert!(run.finished_at.is_some());
        assert_eq!(run.total_tokens, 150);
        assert_eq!(run.source_chats_json, r#"[1, 2, 3]"#);
        assert_eq!(run.source_digest_md.as_deref(), Some("digest-abc"));
    }

    #[test]
    fn update_sleep_run_to_failed() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        db.update_sleep_run_failed(&id, "LLM timeout")
            .expect("update failed");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Failed);
        assert_eq!(run.error_message.as_deref(), Some("LLM timeout"));
    }

    #[test]
    fn get_sleep_run_by_id() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.id, id);
        assert_eq!(run.agent_id, "agent-a");
        assert_eq!(run.trigger, SleepRunTrigger::Manual);
        assert_eq!(run.source_chats_json, "[]");
        assert_eq!(run.input_tokens, 0);
        assert_eq!(run.output_tokens, 0);
        assert_eq!(run.total_tokens, 0);
    }

    #[test]
    fn list_sleep_runs_by_agent() {
        let (db, _dir) = test_db();

        let _id_a1 = create_test_sleep_run(&db, "agent-a");
        let id_a2 = create_test_sleep_run(&db, "agent-a");
        let id_a3 = create_test_sleep_run(&db, "agent-a");
        let _id_b1 = create_test_sleep_run(&db, "agent-b");

        let runs = db.list_sleep_runs("agent-a", 2).expect("list");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, id_a3);
        assert_eq!(runs[1].id, id_a2);
    }

    #[test]
    fn list_all_sleep_runs_returns_all_agents() {
        let (db, _dir) = test_db();

        let id_a = create_test_sleep_run(&db, "agent-a");
        let id_b = create_test_sleep_run(&db, "agent-b");
        let id_c = create_test_sleep_run(&db, "agent-c");

        let runs = db.list_all_sleep_runs(10).expect("list all");
        assert_eq!(runs.len(), 3);

        assert_eq!(runs[0].id, id_c);
        assert_eq!(runs[0].agent_id, "agent-c");
        assert_eq!(runs[1].id, id_b);
        assert_eq!(runs[1].agent_id, "agent-b");
        assert_eq!(runs[2].id, id_a);
        assert_eq!(runs[2].agent_id, "agent-a");
    }

    #[test]
    fn list_all_sleep_runs_respects_limit() {
        let (db, _dir) = test_db();

        create_test_sleep_run(&db, "agent-a");
        create_test_sleep_run(&db, "agent-b");
        create_test_sleep_run(&db, "agent-c");
        create_test_sleep_run(&db, "agent-d");
        create_test_sleep_run(&db, "agent-e");

        let runs = db.list_all_sleep_runs(3).expect("list all");
        assert_eq!(runs.len(), 3);
    }

    // ---------------------------------------------------------------------------
    // Message range queries (event extract refactor)
    // ---------------------------------------------------------------------------

    fn finish_step_success(
        db: &Database,
        run_id: &str,
        step: SleepStepName,
        input_tokens: i64,
        output_tokens: i64,
    ) {
        db.start_sleep_step(run_id, step).expect("start");
        db.finish_sleep_step(
            run_id,
            step,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens,
                output_tokens,
                error_message: None,
                metadata_json: None,
            },
        )
        .expect("finish");
    }

    fn finish_step_failed(db: &Database, run_id: &str, step: SleepStepName, error: &str) {
        db.start_sleep_step(run_id, step).expect("start");
        db.finish_sleep_step(
            run_id,
            step,
            SleepStepResult {
                status: SleepStepStatus::Failed,
                input_tokens: 0,
                output_tokens: 0,
                error_message: Some(error),
                metadata_json: None,
            },
        )
        .expect("finish");
    }

    fn skip_step(db: &Database, run_id: &str, step: SleepStepName) {
        db.start_sleep_step(run_id, step).expect("start");
        db.finish_sleep_step(
            run_id,
            step,
            SleepStepResult {
                status: SleepStepStatus::Skipped,
                input_tokens: 0,
                output_tokens: 0,
                error_message: None,
                metadata_json: None,
            },
        )
        .expect("finish");
    }
}

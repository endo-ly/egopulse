//! Read-only aggregations behind the WebUI Metrics screen.
//!
//! Every query takes a half-open `[start, end)` window of RFC3339 UTC
//! timestamps, compared as strings like the rest of the storage layer. Rows
//! are attributed to an agent through `chats.agent_id`. Grouping is kept at
//! the UTC minute so callers can re-bucket into any local timezone exactly.

use rusqlite::{Connection, params};

use crate::error::StorageError;

use super::Database;

/// LLM usage of one agent, request kind, and model within one UTC minute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageMinute {
    /// `YYYY-MM-DDTHH:MM` in UTC.
    pub minute: String,
    pub agent_id: String,
    pub request_kind: String,
    pub provider: String,
    pub model: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Prompt tokens served from cache, over the rows that reported it.
    pub cached_input_tokens: i64,
    /// Input tokens of the rows that reported `cached_input_tokens`, the
    /// denominator of a cache hit rate.
    pub cache_reported_input_tokens: i64,
}

/// Tokens spent by Sleep runs of one agent started within one UTC minute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SleepUsageMinute {
    pub minute: String,
    pub agent_id: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// Pulse runs and LLM usage of one intention. `intention_id` is `None` for
/// Pulse usage that falls inside no run of its agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PulseIntentionUsage {
    pub agent_id: String,
    pub intention_id: Option<String>,
    pub runs: i64,
    pub failed_runs: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// One Turn accepted within the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnFact {
    pub agent_id: String,
    pub state: String,
    pub error_kind: Option<String>,
    /// Accept-to-finish time; `None` while the Turn is unfinished.
    pub duration_ms: Option<i64>,
}

/// Finished Tool calls of one agent, tool, state, and error kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolCallCount {
    pub agent_id: String,
    pub tool_name: String,
    pub state: String,
    pub error_kind: Option<String>,
    pub count: i64,
}

/// Sleep runs of one agent and status (skipped checks excluded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SleepRunCount {
    pub agent_id: String,
    pub status: String,
    pub count: i64,
}

/// Which ledger a [`FailureRecord`] comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureSource {
    Turn,
    Pulse,
    Sleep,
}

/// A failed Turn, Pulse run, or Sleep run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FailureRecord {
    pub at: String,
    pub source: FailureSource,
    pub agent_id: String,
    /// Turn `error_kind`, Pulse intention id, or Sleep status.
    pub kind: String,
    pub summary: Option<String>,
    /// Chat of a failed Turn.
    pub chat_id: Option<i64>,
    /// Run id of a failed Pulse or Sleep run.
    pub run_id: Option<String>,
}

/// The latest activity of an agent, independent of any window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentActivity {
    pub agent_id: String,
    pub last_turn_at: Option<String>,
    /// Status of the latest Sleep run that did work (not `skipped`).
    pub last_sleep_status: Option<String>,
}

impl Database {
    /// LLM usage grouped by UTC minute, agent, request kind, and model.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on connection or query failures.
    pub(crate) fn usage_by_minute(
        &self,
        start: &str,
        end: &str,
    ) -> Result<Vec<UsageMinute>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT substr(l.created_at, 1, 16), c.agent_id, l.request_kind, l.provider, l.model,
                    COUNT(*), SUM(l.input_tokens), SUM(l.output_tokens),
                    COALESCE(SUM(l.cached_input_tokens), 0),
                    COALESCE(SUM(CASE WHEN l.cached_input_tokens IS NOT NULL THEN l.input_tokens END), 0)
             FROM llm_usage_logs l
             JOIN chats c ON c.chat_id = l.chat_id
             WHERE l.created_at >= ?1 AND l.created_at < ?2
             GROUP BY 1, 2, 3, 4, 5
             ORDER BY 1",
        )?;
        let rows = stmt
            .query_map(params![start, end], |row| {
                Ok(UsageMinute {
                    minute: row.get(0)?,
                    agent_id: row.get(1)?,
                    request_kind: row.get(2)?,
                    provider: row.get(3)?,
                    model: row.get(4)?,
                    calls: row.get(5)?,
                    input_tokens: row.get(6)?,
                    output_tokens: row.get(7)?,
                    cached_input_tokens: row.get(8)?,
                    cache_reported_input_tokens: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Sleep run tokens grouped by the UTC minute the run started and agent.
    /// Sleep requests are not written to `llm_usage_logs`; the run carries
    /// their totals.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on connection or query failures.
    pub(crate) fn sleep_usage_by_minute(
        &self,
        start: &str,
        end: &str,
    ) -> Result<Vec<SleepUsageMinute>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT substr(started_at, 1, 16), agent_id, SUM(input_tokens), SUM(output_tokens)
             FROM sleep_runs
             WHERE started_at >= ?1 AND started_at < ?2
               AND (input_tokens > 0 OR output_tokens > 0)
             GROUP BY 1, 2
             ORDER BY 1",
        )?;
        let rows = stmt
            .query_map(params![start, end], |row| {
                Ok(SleepUsageMinute {
                    minute: row.get(0)?,
                    agent_id: row.get(1)?,
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Pulse runs started in the window and the Pulse LLM usage attributed to
    /// them, per intention.
    ///
    /// A usage row belongs to the run of the same agent whose
    /// `[started_at, finished_at]` contains it. The agent comes from the chat
    /// the row was logged against, so silent runs (no `pulse_runs.chat_id`)
    /// are attributed too. Pulse scans agents sequentially, so the windows of
    /// one agent's runs never overlap and the attribution is unique.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on connection or query failures.
    pub(crate) fn pulse_intention_usage(
        &self,
        start: &str,
        end: &str,
    ) -> Result<Vec<PulseIntentionUsage>, StorageError> {
        let conn = self.get_conn()?;
        let mut usage = attributed_pulse_usage(&conn, start, end)?;

        let mut stmt = conn.prepare_cached(
            "SELECT agent_id, intention_id, COUNT(*), SUM(status = 'failed')
             FROM pulse_runs
             WHERE started_at >= ?1 AND started_at < ?2
             GROUP BY 1, 2",
        )?;
        let runs = stmt
            .query_map(params![start, end], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        for (agent_id, intention_id, total, failed) in runs {
            match usage.iter_mut().find(|entry| {
                entry.agent_id == agent_id && entry.intention_id.as_deref() == Some(&intention_id)
            }) {
                Some(entry) => {
                    entry.runs = total;
                    entry.failed_runs = failed;
                }
                None => usage.push(PulseIntentionUsage {
                    agent_id,
                    intention_id: Some(intention_id),
                    runs: total,
                    failed_runs: failed,
                    input_tokens: 0,
                    output_tokens: 0,
                }),
            }
        }
        Ok(usage)
    }

    /// Turns accepted in the window.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on connection or query failures.
    pub(crate) fn turn_facts(&self, start: &str, end: &str) -> Result<Vec<TurnFact>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT c.agent_id, t.state, t.error_kind,
                    CASE WHEN t.finished_at IS NULL THEN NULL
                         ELSE CAST(ROUND((julianday(t.finished_at) - julianday(t.accepted_at)) * 86400000) AS INTEGER)
                    END
             FROM turn_runs t
             JOIN chats c ON c.chat_id = t.chat_id
             WHERE t.accepted_at >= ?1 AND t.accepted_at < ?2",
        )?;
        let rows = stmt
            .query_map(params![start, end], |row| {
                Ok(TurnFact {
                    agent_id: row.get(0)?,
                    state: row.get(1)?,
                    error_kind: row.get(2)?,
                    duration_ms: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Finished Tool calls in the window, grouped by agent, tool, state, and
    /// error kind. Calls still pending or running are left out.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on connection or query failures.
    pub(crate) fn tool_call_counts(
        &self,
        start: &str,
        end: &str,
    ) -> Result<Vec<ToolCallCount>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT c.agent_id, t.tool_name, t.state, t.error_kind, COUNT(*)
             FROM tool_calls t
             JOIN chats c ON c.chat_id = t.chat_id
             WHERE t.timestamp >= ?1 AND t.timestamp < ?2
               AND t.state IN ('succeeded', 'failed', 'uncertain')
             GROUP BY 1, 2, 3, 4",
        )?;
        let rows = stmt
            .query_map(params![start, end], |row| {
                Ok(ToolCallCount {
                    agent_id: row.get(0)?,
                    tool_name: row.get(1)?,
                    state: row.get(2)?,
                    error_kind: row.get(3)?,
                    count: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Sleep runs started in the window by agent and status, leaving out the
    /// daily `skipped` checks.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on connection or query failures.
    pub(crate) fn sleep_run_counts(
        &self,
        start: &str,
        end: &str,
    ) -> Result<Vec<SleepRunCount>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT agent_id, status, COUNT(*)
             FROM sleep_runs
             WHERE started_at >= ?1 AND started_at < ?2 AND status != 'skipped'
             GROUP BY 1, 2",
        )?;
        let rows = stmt
            .query_map(params![start, end], |row| {
                Ok(SleepRunCount {
                    agent_id: row.get(0)?,
                    status: row.get(1)?,
                    count: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The newest failed Turns, Pulse runs, and Sleep runs of the window,
    /// newest first. `agent_id` narrows them to one agent.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on connection or query failures.
    pub(crate) fn recent_failures(
        &self,
        start: &str,
        end: &str,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FailureRecord>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT at, source, agent_id, kind, summary, chat_id, run_id FROM (
                 SELECT COALESCE(t.finished_at, t.updated_at) AS at, 'turn' AS source,
                        c.agent_id AS agent_id, COALESCE(t.error_kind, t.state) AS kind,
                        t.error_message AS summary, t.chat_id AS chat_id, NULL AS run_id
                 FROM turn_runs t JOIN chats c ON c.chat_id = t.chat_id
                 WHERE t.state IN ('failed', 'uncertain')
                 UNION ALL
                 SELECT COALESCE(finished_at, started_at), 'pulse', agent_id, intention_id,
                        error_message, NULL, id
                 FROM pulse_runs WHERE status = 'failed'
                 UNION ALL
                 SELECT COALESCE(finished_at, started_at), 'sleep', agent_id, status,
                        error_message, NULL, id
                 FROM sleep_runs WHERE status IN ('failed', 'partial_failure')
             )
             WHERE at >= ?1 AND at < ?2 AND (?3 IS NULL OR agent_id = ?3)
             ORDER BY at DESC
             LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![start, end, agent_id, limit as i64], |row| {
                let source: String = row.get(1)?;
                Ok(FailureRecord {
                    at: row.get(0)?,
                    source: match source.as_str() {
                        "turn" => FailureSource::Turn,
                        "pulse" => FailureSource::Pulse,
                        _ => FailureSource::Sleep,
                    },
                    agent_id: row.get(2)?,
                    kind: row.get(3)?,
                    summary: row.get(4)?,
                    chat_id: row.get(5)?,
                    run_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The latest Turn and the latest working Sleep run of every agent that
    /// has either.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on connection or query failures.
    pub(crate) fn agent_activity(&self) -> Result<Vec<AgentActivity>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "WITH turns AS (
                 SELECT c.agent_id AS agent_id, MAX(t.accepted_at) AS last_turn_at
                 FROM turn_runs t JOIN chats c ON c.chat_id = t.chat_id
                 GROUP BY c.agent_id
             ), sleeps AS (
                 SELECT agent_id, status FROM (
                     SELECT agent_id, status,
                            ROW_NUMBER() OVER (PARTITION BY agent_id ORDER BY started_at DESC) AS rn
                     FROM sleep_runs WHERE status != 'skipped'
                 ) WHERE rn = 1
             ), agents AS (
                 SELECT agent_id FROM turns UNION SELECT agent_id FROM sleeps
             )
             SELECT a.agent_id, t.last_turn_at, s.status
             FROM agents a
             LEFT JOIN turns t ON t.agent_id = a.agent_id
             LEFT JOIN sleeps s ON s.agent_id = a.agent_id
             ORDER BY a.agent_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(AgentActivity {
                    agent_id: row.get(0)?,
                    last_turn_at: row.get(1)?,
                    last_sleep_status: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Pulse usage of the window grouped by the intention of the run it falls in.
fn attributed_pulse_usage(
    conn: &Connection,
    start: &str,
    end: &str,
) -> Result<Vec<PulseIntentionUsage>, StorageError> {
    let mut stmt = conn.prepare_cached(
        "SELECT c.agent_id, r.intention_id, SUM(l.input_tokens), SUM(l.output_tokens)
         FROM llm_usage_logs l
         JOIN chats c ON c.chat_id = l.chat_id
         LEFT JOIN pulse_runs r
           ON r.agent_id = c.agent_id
          AND l.created_at >= r.started_at
          AND l.created_at <= COALESCE(r.finished_at, ?2)
         WHERE l.request_kind = 'pulse' AND l.created_at >= ?1 AND l.created_at < ?2
         GROUP BY 1, 2",
    )?;
    let rows = stmt
        .query_map(params![start, end], |row| {
            Ok(PulseIntentionUsage {
                agent_id: row.get(0)?,
                intention_id: row.get(1)?,
                runs: 0,
                failed_runs: 0,
                input_tokens: row.get(2)?,
                output_tokens: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;

    const START: &str = "2026-10-01T00:00:00+00:00";
    const END: &str = "2026-10-02T00:00:00+00:00";

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(&dir.path().join("runtime").join("egopulse.db")).expect("db");
        (db, dir)
    }

    fn chat(db: &Database, chat_id: i64, agent_id: &str, channel: &str) {
        db.get_conn()
            .expect("conn")
            .execute(
                "INSERT INTO chats (chat_id, chat_type, last_message_time, channel, external_chat_id, agent_id)
                 VALUES (?1, 'private', ?2, ?3, ?4, ?5)",
                params![chat_id, START, channel, format!("ext-{chat_id}"), agent_id],
            )
            .expect("insert chat");
    }

    fn usage(
        db: &Database,
        chat_id: i64,
        kind: &str,
        model: &str,
        at: &str,
        io: (i64, i64),
        cached: Option<i64>,
    ) {
        db.get_conn()
            .expect("conn")
            .execute(
                "INSERT INTO llm_usage_logs
                     (chat_id, caller_channel, provider, model, input_tokens, output_tokens, total_tokens,
                      request_kind, cached_input_tokens, created_at)
                 VALUES (?1, 'discord', 'openrouter', ?2, ?3, ?4, ?3 + ?4, ?5, ?6, ?7)",
                params![chat_id, model, io.0, io.1, kind, cached, at],
            )
            .expect("insert usage");
    }

    fn pulse_run(
        db: &Database,
        id: &str,
        agent_id: &str,
        intention: &str,
        status: &str,
        window: (&str, Option<&str>),
        chat_id: Option<i64>,
    ) {
        db.get_conn()
            .expect("conn")
            .execute(
                "INSERT INTO pulse_runs (id, agent_id, intention_id, due_key, chat_id, status, started_at, finished_at, error_message)
                 VALUES (?1, ?2, ?3, ?1, ?4, ?5, ?6, ?7, CASE WHEN ?5 = 'failed' THEN 'boom' END)",
                params![id, agent_id, intention, chat_id, status, window.0, window.1],
            )
            .expect("insert pulse run");
    }

    fn turn(
        db: &Database,
        id: &str,
        chat_id: i64,
        state: &str,
        accepted_at: &str,
        finished_at: Option<&str>,
        error_kind: Option<&str>,
    ) {
        db.get_conn()
            .expect("conn")
            .execute(
                "INSERT INTO turn_runs (turn_id, chat_id, request_key, state, accepted_at, updated_at, finished_at, error_kind, error_message)
                 VALUES (?1, ?2, ?1, ?3, ?4, ?4, ?5, ?6, CASE WHEN ?6 IS NOT NULL THEN 'went wrong' END)",
                params![id, chat_id, state, accepted_at, finished_at, error_kind],
            )
            .expect("insert turn");
    }

    fn sleep_run(
        db: &Database,
        id: &str,
        agent_id: &str,
        status: &str,
        started_at: &str,
        io: (i64, i64),
    ) {
        db.get_conn()
            .expect("conn")
            .execute(
                "INSERT INTO sleep_runs (id, agent_id, status, trigger_type, started_at, finished_at, input_tokens, output_tokens, total_tokens)
                 VALUES (?1, ?2, ?3, 'scheduled', ?4, ?4, ?5, ?6, ?5 + ?6)",
                params![id, agent_id, status, started_at, io.0, io.1],
            )
            .expect("insert sleep run");
    }

    fn tool_call(
        db: &Database,
        id: &str,
        chat_id: i64,
        tool: &str,
        state: &str,
        error_kind: Option<&str>,
        at: &str,
    ) {
        db.get_conn()
            .expect("conn")
            .execute(
                "INSERT INTO tool_calls (id, chat_id, message_id, tool_name, tool_input, timestamp, state, error_kind)
                 VALUES (?1, ?2, 'm1', ?3, '{}', ?4, ?5, ?6)",
                params![id, chat_id, tool, at, state, error_kind],
            )
            .expect("insert tool call");
    }

    #[test]
    fn usage_groups_by_minute_agent_kind_and_model_within_the_window() {
        // Arrange
        let (db, _dir) = test_db();
        chat(&db, 1, "lyre", "discord");
        chat(&db, 2, "musa", "web");
        usage(
            &db,
            1,
            "agent_loop",
            "m-a",
            "2026-10-01T09:15:10.5+00:00",
            (100, 10),
            Some(40),
        );
        usage(
            &db,
            1,
            "agent_loop",
            "m-a",
            "2026-10-01T09:15:50+00:00",
            (200, 20),
            None,
        );
        usage(
            &db,
            2,
            "pulse",
            "m-b",
            "2026-10-01T09:15:20+00:00",
            (50, 5),
            Some(0),
        );
        usage(
            &db,
            1,
            "agent_loop",
            "m-a",
            "2026-09-30T23:59:59+00:00",
            (999, 9),
            None,
        );
        usage(&db, 1, "agent_loop", "m-a", END, (999, 9), None);

        // Act
        let rows = db.usage_by_minute(START, END).expect("usage");

        // Assert
        assert_eq!(
            rows,
            vec![
                UsageMinute {
                    minute: "2026-10-01T09:15".into(),
                    agent_id: "lyre".into(),
                    request_kind: "agent_loop".into(),
                    provider: "openrouter".into(),
                    model: "m-a".into(),
                    calls: 2,
                    input_tokens: 300,
                    output_tokens: 30,
                    cached_input_tokens: 40,
                    cache_reported_input_tokens: 100,
                },
                UsageMinute {
                    minute: "2026-10-01T09:15".into(),
                    agent_id: "musa".into(),
                    request_kind: "pulse".into(),
                    provider: "openrouter".into(),
                    model: "m-b".into(),
                    calls: 1,
                    input_tokens: 50,
                    output_tokens: 5,
                    cached_input_tokens: 0,
                    cache_reported_input_tokens: 50,
                },
            ]
        );
    }

    #[test]
    fn sleep_usage_counts_runs_with_tokens_inside_the_window() {
        // Arrange
        let (db, _dir) = test_db();
        sleep_run(
            &db,
            "s1",
            "lyre",
            "success",
            "2026-10-01T04:00:01+00:00",
            (1000, 100),
        );
        sleep_run(
            &db,
            "s2",
            "lyre",
            "skipped",
            "2026-10-01T04:00:30+00:00",
            (0, 0),
        );
        sleep_run(
            &db,
            "s3",
            "lyre",
            "success",
            "2026-09-30T04:00:00+00:00",
            (5, 5),
        );

        // Act
        let rows = db.sleep_usage_by_minute(START, END).expect("sleep usage");

        // Assert
        assert_eq!(
            rows,
            vec![SleepUsageMinute {
                minute: "2026-10-01T04:00".into(),
                agent_id: "lyre".into(),
                input_tokens: 1000,
                output_tokens: 100,
            }]
        );
    }

    #[test]
    fn pulse_usage_is_attributed_by_agent_and_run_window_including_silent_runs() {
        // Arrange: the silent run has no chat_id; usage is logged against
        // the agent's home chat.
        let (db, _dir) = test_db();
        chat(&db, 1, "lyre", "discord");
        chat(&db, 2, "musa", "discord");
        pulse_run(
            &db,
            "r1",
            "lyre",
            "research",
            "success",
            (
                "2026-10-01T07:00:00+00:00",
                Some("2026-10-01T07:05:00+00:00"),
            ),
            None,
        );
        pulse_run(
            &db,
            "r2",
            "lyre",
            "murmur",
            "failed",
            (
                "2026-10-01T08:00:00+00:00",
                Some("2026-10-01T08:01:00+00:00"),
            ),
            Some(1),
        );
        usage(
            &db,
            1,
            "pulse",
            "m",
            "2026-10-01T07:01:00+00:00",
            (1000, 10),
            None,
        );
        usage(
            &db,
            1,
            "pulse",
            "m",
            "2026-10-01T07:04:00+00:00",
            (2000, 20),
            None,
        );
        usage(
            &db,
            1,
            "pulse",
            "m",
            "2026-10-01T08:00:30+00:00",
            (300, 3),
            None,
        );
        // Inside r1's window but another agent's chat.
        usage(
            &db,
            2,
            "pulse",
            "m",
            "2026-10-01T07:02:00+00:00",
            (40, 4),
            None,
        );
        // Not Pulse.
        usage(
            &db,
            1,
            "agent_loop",
            "m",
            "2026-10-01T07:02:00+00:00",
            (7, 7),
            None,
        );

        // Act
        let mut rows = db.pulse_intention_usage(START, END).expect("pulse usage");
        rows.sort_by(|a, b| (&a.agent_id, &a.intention_id).cmp(&(&b.agent_id, &b.intention_id)));

        // Assert
        assert_eq!(
            rows,
            vec![
                PulseIntentionUsage {
                    agent_id: "lyre".into(),
                    intention_id: Some("murmur".into()),
                    runs: 1,
                    failed_runs: 1,
                    input_tokens: 300,
                    output_tokens: 3,
                },
                PulseIntentionUsage {
                    agent_id: "lyre".into(),
                    intention_id: Some("research".into()),
                    runs: 1,
                    failed_runs: 0,
                    input_tokens: 3000,
                    output_tokens: 30,
                },
                PulseIntentionUsage {
                    agent_id: "musa".into(),
                    intention_id: None,
                    runs: 0,
                    failed_runs: 0,
                    input_tokens: 40,
                    output_tokens: 4,
                },
            ]
        );
    }

    #[test]
    fn pulse_runs_without_usage_still_count() {
        // Arrange
        let (db, _dir) = test_db();
        pulse_run(
            &db,
            "r1",
            "lyre",
            "murmur",
            "skipped",
            (
                "2026-10-01T07:00:00+00:00",
                Some("2026-10-01T07:00:01+00:00"),
            ),
            None,
        );

        // Act
        let rows = db.pulse_intention_usage(START, END).expect("pulse usage");

        // Assert
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].runs, rows[0].input_tokens), (1, 0));
    }

    #[test]
    fn turn_facts_carry_state_error_kind_and_duration() {
        // Arrange
        let (db, _dir) = test_db();
        chat(&db, 1, "lyre", "discord");
        turn(
            &db,
            "t1",
            1,
            "completed",
            "2026-10-01T10:00:00+00:00",
            Some("2026-10-01T10:00:08.500+00:00"),
            None,
        );
        turn(
            &db,
            "t2",
            1,
            "failed",
            "2026-10-01T11:00:00+00:00",
            Some("2026-10-01T11:00:02+00:00"),
            Some("llm_error"),
        );
        turn(
            &db,
            "t3",
            1,
            "model_pending",
            "2026-10-01T12:00:00+00:00",
            None,
            None,
        );
        turn(
            &db,
            "t4",
            1,
            "completed",
            "2026-09-30T12:00:00+00:00",
            Some("2026-09-30T12:00:01+00:00"),
            None,
        );

        // Act
        let mut rows = db.turn_facts(START, END).expect("turns");
        rows.sort_by(|a, b| a.state.cmp(&b.state));

        // Assert
        assert_eq!(
            rows,
            vec![
                TurnFact {
                    agent_id: "lyre".into(),
                    state: "completed".into(),
                    error_kind: None,
                    duration_ms: Some(8500)
                },
                TurnFact {
                    agent_id: "lyre".into(),
                    state: "failed".into(),
                    error_kind: Some("llm_error".into()),
                    duration_ms: Some(2000)
                },
                TurnFact {
                    agent_id: "lyre".into(),
                    state: "model_pending".into(),
                    error_kind: None,
                    duration_ms: None
                },
            ]
        );
    }

    #[test]
    fn tool_counts_leave_out_unfinished_calls() {
        // Arrange
        let (db, _dir) = test_db();
        chat(&db, 1, "lyre", "discord");
        tool_call(
            &db,
            "c1",
            1,
            "bash",
            "succeeded",
            None,
            "2026-10-01T10:00:00+00:00",
        );
        tool_call(
            &db,
            "c2",
            1,
            "bash",
            "failed",
            Some("timeout"),
            "2026-10-01T10:00:01+00:00",
        );
        tool_call(
            &db,
            "c3",
            1,
            "bash",
            "failed",
            Some("timeout"),
            "2026-10-01T10:00:02+00:00",
        );
        tool_call(
            &db,
            "c4",
            1,
            "bash",
            "running",
            None,
            "2026-10-01T10:00:03+00:00",
        );

        // Act
        let mut rows = db.tool_call_counts(START, END).expect("tools");
        rows.sort_by(|a, b| a.state.cmp(&b.state));

        // Assert
        assert_eq!(
            rows,
            vec![
                ToolCallCount {
                    agent_id: "lyre".into(),
                    tool_name: "bash".into(),
                    state: "failed".into(),
                    error_kind: Some("timeout".into()),
                    count: 2
                },
                ToolCallCount {
                    agent_id: "lyre".into(),
                    tool_name: "bash".into(),
                    state: "succeeded".into(),
                    error_kind: None,
                    count: 1
                },
            ]
        );
    }

    #[test]
    fn sleep_run_counts_leave_out_skipped_checks() {
        // Arrange
        let (db, _dir) = test_db();
        sleep_run(
            &db,
            "s1",
            "lyre",
            "success",
            "2026-10-01T04:00:00+00:00",
            (1, 1),
        );
        sleep_run(
            &db,
            "s2",
            "lyre",
            "skipped",
            "2026-10-01T05:00:00+00:00",
            (0, 0),
        );

        // Act
        let rows = db.sleep_run_counts(START, END).expect("sleep counts");

        // Assert
        assert_eq!(
            rows,
            vec![SleepRunCount {
                agent_id: "lyre".into(),
                status: "success".into(),
                count: 1
            }]
        );
    }

    #[test]
    fn recent_failures_merge_the_ledgers_newest_first_and_filter_by_agent() {
        // Arrange
        let (db, _dir) = test_db();
        chat(&db, 1, "lyre", "discord");
        chat(&db, 2, "musa", "web");
        turn(
            &db,
            "t1",
            1,
            "failed",
            "2026-10-01T10:00:00+00:00",
            Some("2026-10-01T10:00:05+00:00"),
            Some("llm_error"),
        );
        turn(
            &db,
            "t2",
            2,
            "uncertain",
            "2026-10-01T12:00:00+00:00",
            Some("2026-10-01T12:00:05+00:00"),
            Some("interrupted"),
        );
        turn(
            &db,
            "t3",
            1,
            "completed",
            "2026-10-01T13:00:00+00:00",
            Some("2026-10-01T13:00:05+00:00"),
            None,
        );
        pulse_run(
            &db,
            "p1",
            "lyre",
            "research",
            "failed",
            (
                "2026-10-01T11:00:00+00:00",
                Some("2026-10-01T11:00:10+00:00"),
            ),
            None,
        );
        sleep_run(
            &db,
            "s1",
            "lyre",
            "partial_failure",
            "2026-10-01T04:00:00+00:00",
            (1, 1),
        );

        // Act
        let all = db.recent_failures(START, END, None, 10).expect("failures");
        let lyre = db
            .recent_failures(START, END, Some("lyre"), 2)
            .expect("failures");

        // Assert
        let order: Vec<_> = all.iter().map(|f| (f.source, f.kind.as_str())).collect();
        assert_eq!(
            order,
            vec![
                (FailureSource::Turn, "interrupted"),
                (FailureSource::Pulse, "research"),
                (FailureSource::Turn, "llm_error"),
                (FailureSource::Sleep, "partial_failure"),
            ]
        );
        assert_eq!(all[2].chat_id, Some(1));
        assert_eq!(all[1].run_id.as_deref(), Some("p1"));
        assert_eq!(
            lyre.iter().map(|f| f.source).collect::<Vec<_>>(),
            vec![FailureSource::Pulse, FailureSource::Turn]
        );
    }

    #[test]
    fn agent_activity_reports_latest_turn_and_working_sleep() {
        // Arrange
        let (db, _dir) = test_db();
        chat(&db, 1, "lyre", "discord");
        turn(
            &db,
            "t1",
            1,
            "completed",
            "2026-10-01T10:00:00+00:00",
            None,
            None,
        );
        turn(
            &db,
            "t2",
            1,
            "completed",
            "2026-10-03T10:00:00+00:00",
            None,
            None,
        );
        sleep_run(
            &db,
            "s1",
            "lyre",
            "failed",
            "2026-10-01T04:00:00+00:00",
            (0, 0),
        );
        sleep_run(
            &db,
            "s2",
            "lyre",
            "skipped",
            "2026-10-02T04:00:00+00:00",
            (0, 0),
        );
        sleep_run(
            &db,
            "s3",
            "vega",
            "success",
            "2026-10-02T04:00:00+00:00",
            (0, 0),
        );

        // Act
        let rows = db.agent_activity().expect("activity");

        // Assert
        assert_eq!(
            rows,
            vec![
                AgentActivity {
                    agent_id: "lyre".into(),
                    last_turn_at: Some("2026-10-03T10:00:00+00:00".into()),
                    last_sleep_status: Some("failed".into())
                },
                AgentActivity {
                    agent_id: "vega".into(),
                    last_turn_at: None,
                    last_sleep_status: Some("success".into())
                },
            ]
        );
    }
}

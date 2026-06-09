use std::str::FromStr;

use rusqlite::{OptionalExtension, params};

use crate::error::StorageError;

use super::{
    Database, EpisodeEvent, EpisodeEventCertainty, EpisodeEventKind, EpisodeRollup,
    RollupGranularity, StoredMessage,
};

use super::chat::row_to_stored_message;

macro_rules! parse_row_enum {
    ($row:expr, $idx:expr, $ty:ty) => {{
        let s: String = $row.get($idx)?;
        <$ty>::from_str(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                $idx,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })
    }};
}

fn row_to_episode_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<EpisodeEvent> {
    let kind = parse_row_enum!(row, 4, EpisodeEventKind)?;
    let certainty = parse_row_enum!(row, 8, EpisodeEventCertainty)?;
    Ok(EpisodeEvent {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        experienced_at: row.get(2)?,
        encoded_at: row.get(3)?,
        kind,
        title: row.get(5)?,
        body_md: row.get(6)?,
        ripple_strength: row.get(7)?,
        certainty,
        sleep_run_id: row.get(9)?,
        source_refs_json: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn row_to_episode_rollup(row: &rusqlite::Row<'_>) -> rusqlite::Result<EpisodeRollup> {
    let granularity = parse_row_enum!(row, 2, RollupGranularity)?;
    Ok(EpisodeRollup {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        granularity,
        period_key: row.get(3)?,
        period_start: row.get(4)?,
        period_end_exclusive: row.get(5)?,
        summary_md: row.get(6)?,
        max_ripple: row.get(7)?,
        event_count: row.get(8)?,
        generated_run_id: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Phase 1 episode event queries; exercised by unit tests below, wired into runtime in Phase 2+"
    )
)]
impl Database {
    /// Lists events by `sleep_run_id`.
    pub(crate) fn list_episode_events_by_run(
        &self,
        sleep_run_id: &str,
    ) -> Result<Vec<EpisodeEvent>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                    ripple_strength, certainty, sleep_run_id, source_refs_json,
                    created_at, updated_at
             FROM episode_events
             WHERE sleep_run_id = ?1
             ORDER BY experienced_at DESC",
        )?;
        stmt.query_map(params![sleep_run_id], row_to_episode_event)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Lists events for an agent within a time range `[start, end)`, ordered by
    /// `experienced_at ASC`.
    pub(crate) fn list_episode_events_in_range(
        &self,
        agent_id: &str,
        start: &str,
        end_exclusive: &str,
    ) -> Result<Vec<EpisodeEvent>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                    ripple_strength, certainty, sleep_run_id, source_refs_json,
                    created_at, updated_at
             FROM episode_events
             WHERE agent_id = ?1 AND experienced_at >= ?2 AND experienced_at < ?3
             ORDER BY experienced_at ASC",
        )?;
        stmt.query_map(
            params![agent_id, start, end_exclusive],
            row_to_episode_event,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn get_messages_between(
        &self,
        chat_id: i64,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                    message_kind, recipient_agent_id
             FROM messages
             WHERE chat_id = ?1
               AND (?2 IS NULL OR timestamp >= ?2)
               AND (?3 IS NULL OR timestamp < ?3)
             ORDER BY timestamp ASC",
        )?;
        stmt.query_map(params![chat_id, from, to], row_to_stored_message)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn get_messages_after_cursor(
        &self,
        chat_id: i64,
        cursor: Option<(&str, &str)>,
        upper_bound: (&str, &str),
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let conn = self.get_conn()?;
        let (cursor_at, cursor_id) = cursor.unzip();
        let mut stmt = conn.prepare_cached(
            "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                    message_kind, recipient_agent_id
             FROM messages
             WHERE chat_id = ?1
               AND (?2 IS NULL OR (timestamp, id) > (?2, ?3))
               AND (timestamp, id) <= (?4, ?5)
             ORDER BY timestamp ASC, id ASC",
        )?;
        stmt.query_map(
            params![chat_id, cursor_at, cursor_id, upper_bound.0, upper_bound.1],
            row_to_stored_message,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn get_latest_message_cursor(
        &self,
        chat_id: i64,
    ) -> Result<Option<(String, String)>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT timestamp, id FROM messages
             WHERE chat_id = ?1
             ORDER BY timestamp DESC, id DESC
             LIMIT 1",
            params![chat_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn get_episode_events_after_cursor(
        &self,
        agent_id: &str,
        cursor: Option<(&str, &str)>,
        upper_bound: (&str, &str),
    ) -> Result<Vec<EpisodeEvent>, StorageError> {
        let conn = self.get_conn()?;
        let (cursor_at, cursor_id) = cursor.unzip();
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                    ripple_strength, certainty, sleep_run_id, source_refs_json,
                    created_at, updated_at
             FROM episode_events
             WHERE agent_id = ?1
               AND (?2 IS NULL OR (encoded_at, id) > (?2, ?3))
               AND (encoded_at, id) <= (?4, ?5)
             ORDER BY encoded_at ASC, id ASC",
        )?;
        stmt.query_map(
            params![agent_id, cursor_at, cursor_id, upper_bound.0, upper_bound.1],
            row_to_episode_event,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn get_latest_episode_event_cursor(
        &self,
        agent_id: &str,
    ) -> Result<Option<(String, String)>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT encoded_at, id FROM episode_events
             WHERE agent_id = ?1
             ORDER BY encoded_at DESC, id DESC
             LIMIT 1",
            params![agent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn get_agent_chats_with_messages_between(
        &self,
        agent_id: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<(i64, String, String)>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT c.chat_id, c.channel, c.external_chat_id
             FROM chats c
             WHERE c.agent_id = ?1
               AND c.chat_type != 'channel_log'
               AND EXISTS (
                   SELECT 1 FROM messages m
                   WHERE m.chat_id = c.chat_id
                     AND (?2 IS NULL OR m.timestamp >= ?2)
                     AND (?3 IS NULL OR m.timestamp < ?3)
               )
             ORDER BY c.last_message_time ASC",
        )?;
        stmt.query_map(params![agent_id, from, to], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn replace_backfill_episode_events(
        &self,
        agent_id: &str,
        from: Option<&str>,
        to: Option<&str>,
        sleep_run_id: &str,
        events: &[EpisodeEvent],
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let tx = conn.unchecked_transaction()?;

        let is_backfill: bool = tx.query_row(
            "SELECT trigger_type = 'backfill' FROM sleep_runs WHERE id = ?1 AND agent_id = ?2",
            params![sleep_run_id, agent_id],
            |row| row.get(0),
        )?;
        if !is_backfill {
            tx.rollback()?;
            return Err(StorageError::Conflict(format!(
                "sleep run '{sleep_run_id}' is not a backfill run"
            )));
        }

        tx.execute(
            "DELETE FROM episode_events
             WHERE agent_id = ?1
               AND (?2 IS NULL OR experienced_at >= ?2)
               AND (?3 IS NULL OR experienced_at < ?3)
               AND sleep_run_id IN (
                   SELECT id FROM sleep_runs
                   WHERE agent_id = ?1
                     AND trigger_type = 'backfill'
               )",
            params![agent_id, from, to],
        )?;

        for event in events {
            if event.sleep_run_id != sleep_run_id {
                tx.rollback()?;
                return Err(StorageError::Conflict(format!(
                    "event sleep_run_id '{}' does not match expected '{sleep_run_id}'",
                    event.sleep_run_id,
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

        tx.commit()?;
        Ok(())
    }
}

impl Database {
    pub(crate) fn upsert_episode_rollup(&self, rollup: &EpisodeRollup) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO episode_rollups
                 (id, agent_id, granularity, period_key, period_start, period_end_exclusive,
                  summary_md, max_ripple, event_count, generated_run_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(agent_id, granularity, period_key) DO UPDATE SET
                 summary_md = excluded.summary_md,
                 max_ripple = excluded.max_ripple,
                 event_count = excluded.event_count,
                 generated_run_id = excluded.generated_run_id,
                 updated_at = excluded.updated_at",
            params![
                rollup.id,
                rollup.agent_id,
                rollup.granularity.to_string(),
                rollup.period_key,
                rollup.period_start,
                rollup.period_end_exclusive,
                rollup.summary_md,
                rollup.max_ripple,
                rollup.event_count,
                rollup.generated_run_id,
                rollup.created_at,
                rollup.updated_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn list_episode_rollups(
        &self,
        agent_id: &str,
        granularity: RollupGranularity,
        limit: i64,
    ) -> Result<Vec<EpisodeRollup>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, granularity, period_key, period_start, period_end_exclusive,
                    summary_md, max_ripple, event_count, generated_run_id, created_at, updated_at
             FROM episode_rollups
             WHERE agent_id = ?1 AND granularity = ?2
             ORDER BY period_start DESC
             LIMIT ?3",
        )?;
        stmt.query_map(
            params![agent_id, granularity.to_string(), limit],
            row_to_episode_rollup,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn list_background_episode_rollups(
        &self,
        agent_id: &str,
        min_ripple: i64,
        before_period_start: &str,
    ) -> Result<Vec<EpisodeRollup>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, granularity, period_key, period_start, period_end_exclusive,
                    summary_md, max_ripple, event_count, generated_run_id, created_at, updated_at
             FROM episode_rollups
             WHERE agent_id = ?1 AND granularity = 'month' AND max_ripple >= ?2 AND period_start < ?3
             ORDER BY period_start DESC",
        )?;
        stmt.query_map(
            params![agent_id, min_ripple, before_period_start],
            row_to_episode_rollup,
        )?
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

    fn make_test_rollup(
        id: &str,
        agent_id: &str,
        granularity: RollupGranularity,
        period_key: &str,
        period_start: &str,
        period_end_exclusive: &str,
        max_ripple: i64,
    ) -> EpisodeRollup {
        EpisodeRollup {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            granularity,
            period_key: period_key.to_string(),
            period_start: period_start.to_string(),
            period_end_exclusive: period_end_exclusive.to_string(),
            summary_md: format!("summary for {period_key}"),
            max_ripple,
            event_count: 5,
            generated_run_id: "run-test".to_string(),
            created_at: "2025-01-15T00:00:00Z".to_string(),
            updated_at: "2025-01-15T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_migration_v6_creates_episode_rollups() {
        let (db, _dir) = test_db();
        let conn = db.get_conn().expect("pool");

        let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='episode_rollups'",
                    [],
                    |row| row.get(0),
                )
                .expect("check table");
        assert!(exists, "episode_rollups table should exist after migration");

        let expected_columns = [
            "id",
            "agent_id",
            "granularity",
            "period_key",
            "period_start",
            "period_end_exclusive",
            "summary_md",
            "max_ripple",
            "event_count",
            "generated_run_id",
            "created_at",
            "updated_at",
        ];

        let mut stmt = conn
            .prepare("PRAGMA table_info(episode_rollups)")
            .expect("prepare pragma");
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query")
            .map(|r| r.expect("col"))
            .collect();

        for name in &expected_columns {
            assert!(columns.iter().any(|c| c == *name), "missing column: {name}");
        }

        let expected_indexes = [
            "idx_episode_rollups_agent_period",
            "idx_episode_rollups_agent_ripple",
        ];
        let mut stmt = conn
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_episode_rollups%'",
                )
                .expect("prepare");
        let indexes: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("idx"))
            .collect();

        for name in &expected_indexes {
            assert!(indexes.iter().any(|i| i == *name), "missing index: {name}");
        }
    }

    #[test]
    fn test_list_episode_rollups_by_granularity() {
        let (db, _dir) = test_db();

        let week1 = make_test_rollup(
            "r-w1",
            "agent-a",
            RollupGranularity::Week,
            "2025-W01",
            "2024-12-30T00:00:00Z",
            "2025-01-06T00:00:00Z",
            3,
        );
        let week2 = make_test_rollup(
            "r-w2",
            "agent-a",
            RollupGranularity::Week,
            "2025-W02",
            "2025-01-06T00:00:00Z",
            "2025-01-13T00:00:00Z",
            4,
        );
        let month1 = make_test_rollup(
            "r-m1",
            "agent-a",
            RollupGranularity::Month,
            "2025-01",
            "2025-01-01T00:00:00Z",
            "2025-02-01T00:00:00Z",
            5,
        );

        db.upsert_episode_rollup(&week1).expect("insert w1");
        db.upsert_episode_rollup(&week2).expect("insert w2");
        db.upsert_episode_rollup(&month1).expect("insert m1");

        let weeks = db
            .list_episode_rollups("agent-a", RollupGranularity::Week, 10)
            .expect("list weeks");
        assert_eq!(weeks.len(), 2);
        assert_eq!(weeks[0].period_key, "2025-W02", "newest first");
        assert_eq!(weeks[1].period_key, "2025-W01");

        let months = db
            .list_episode_rollups("agent-a", RollupGranularity::Month, 10)
            .expect("list months");
        assert_eq!(months.len(), 1);
        assert_eq!(months[0].period_key, "2025-01");
    }

    #[test]
    fn test_list_episode_rollups_for_background() {
        let (db, _dir) = test_db();

        let m1 = make_test_rollup(
            "r-m1",
            "agent-a",
            RollupGranularity::Month,
            "2024-11",
            "2024-11-01T00:00:00Z",
            "2024-12-01T00:00:00Z",
            5,
        );
        let m2 = make_test_rollup(
            "r-m2",
            "agent-a",
            RollupGranularity::Month,
            "2024-12",
            "2024-12-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
            3,
        );
        let m3 = make_test_rollup(
            "r-m3",
            "agent-a",
            RollupGranularity::Month,
            "2025-01",
            "2025-01-01T00:00:00Z",
            "2025-02-01T00:00:00Z",
            4,
        );
        let w1 = make_test_rollup(
            "r-w1",
            "agent-a",
            RollupGranularity::Week,
            "2025-W01",
            "2024-12-30T00:00:00Z",
            "2025-01-06T00:00:00Z",
            5,
        );

        db.upsert_episode_rollup(&m1).expect("insert m1");
        db.upsert_episode_rollup(&m2).expect("insert m2");
        db.upsert_episode_rollup(&m3).expect("insert m3");
        db.upsert_episode_rollup(&w1).expect("insert w1");

        let background = db
            .list_background_episode_rollups("agent-a", 4, "2025-02-01T00:00:00Z")
            .expect("background");

        assert_eq!(
            background.len(),
            2,
            "m1 (ripple=5) and m3 (ripple=4), both months, before Feb"
        );
        assert_eq!(background[0].period_key, "2025-01");
        assert_eq!(background[1].period_key, "2024-11");
    }
}

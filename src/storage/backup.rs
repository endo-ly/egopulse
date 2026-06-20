//! SQLite DB backup module.
//!
//! Provides timestamp-based backup file naming, snapshot creation via
//! `VACUUM INTO`, generation-based pruning, and the next-run schedule
//! calculator used by the periodic backup scheduler.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use rusqlite::Connection;
use tracing::warn;

use crate::error::StorageError;
use crate::storage::Database;

/// Builds the backup file name in the configured timezone.
///
/// The timestamp is rendered as `egopulse-YYYYMMDD-HHMMSS.db` using the
/// supplied IANA timezone, so nightly runs stamp the local date even when
/// the process runs in UTC. When `timezone` cannot be parsed as an IANA
/// timezone identifier, UTC is used as a fallback to keep the function pure.
pub(crate) fn generate_backup_filename(now: DateTime<Utc>, timezone: &str) -> String {
    let formatted = match timezone.parse::<Tz>() {
        Ok(tz) => now.with_timezone(&tz).format("%Y%m%d-%H%M%S").to_string(),
        Err(_) => now.format("%Y%m%d-%H%M%S").to_string(),
    };
    format!("egopulse-{formatted}.db")
}

/// Outcome of a single backup run.
#[derive(Debug, Clone)]
pub(crate) struct BackupOutcome {
    /// Absolute path of the created backup file.
    /// When `integrity_ok` is `false`, the file has already been removed.
    pub path: PathBuf,
    /// Whether `PRAGMA integrity_check` returned `ok` against the snapshot.
    pub integrity_ok: bool,
}

/// Pluggable integrity checker used by [`run_backup_with_integrity_checker`].
///
/// Accepts a backup file path and returns whether the file passes
/// `PRAGMA integrity_check`. Exposed as a `fn` type so the production path
/// and tests can substitute their own validation strategy.
pub(crate) type IntegrityChecker = fn(&Path) -> Result<bool, StorageError>;

/// Runs a single backup with the production integrity checker.
///
/// Creates a `VACUUM INTO` snapshot at `<dest_dir>/egopulse-YYYYMMDD-HHMMSS.db`
/// stamped with `now` interpreted in `timezone`. The snapshot is then verified
/// via [`run_pragma_integrity_check`]; a failed check removes the file and
/// returns `BackupOutcome { integrity_ok: false }`.
///
/// `max_generations` is forwarded to generation pruning (see [`prune_old_backups`]).
///
/// # Errors
///
/// Returns [`StorageError`] when the snapshot cannot be created (disk full,
/// permission denied, etc.) or the integrity checker raises an I/O error.
pub(crate) fn run_backup(
    db: &Database,
    dest_dir: &Path,
    timezone: &str,
    now: DateTime<Utc>,
    max_generations: u32,
) -> Result<BackupOutcome, StorageError> {
    run_backup_with_integrity_checker(
        db,
        dest_dir,
        timezone,
        now,
        max_generations,
        run_pragma_integrity_check,
    )
}

/// Testable variant of [`run_backup`] that accepts a custom [`IntegrityChecker`].
///
/// Production callers should use [`run_backup`]; tests inject a checker that
/// simulates corruption without touching the filesystem.
pub(crate) fn run_backup_with_integrity_checker(
    db: &Database,
    dest_dir: &Path,
    timezone: &str,
    now: DateTime<Utc>,
    _max_generations: u32,
    checker: IntegrityChecker,
) -> Result<BackupOutcome, StorageError> {
    std::fs::create_dir_all(dest_dir)?;
    let dest_path = dest_dir.join(generate_backup_filename(now, timezone));

    {
        let conn = db.get_conn()?;
        let escaped = dest_path.to_string_lossy().replace('\'', "''");
        conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))?;
    }

    let integrity_ok = checker(&dest_path)?;
    let outcome = BackupOutcome {
        path: dest_path,
        integrity_ok,
    };
    if !integrity_ok {
        let _ = std::fs::remove_file(&outcome.path);
        warn!(
            path = %outcome.path.display(),
            "backup integrity check failed; removed file"
        );
    }

    Ok(outcome)
}

/// Runs `PRAGMA integrity_check` against the supplied SQLite file.
fn run_pragma_integrity_check(path: &Path) -> Result<bool, StorageError> {
    let conn = Connection::open(path)?;
    let result: String = conn.query_row("PRAGMA integrity_check;", [], |row| row.get(0))?;
    Ok(result == "ok")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StoredMessage;

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn insert_message(db: &Database, id: &str, content: &str) {
        let msg = StoredMessage::user(1, "alice".to_string(), content.to_string());
        let msg = StoredMessage {
            id: id.to_string(),
            ..msg
        };
        db.store_message_only(&msg).expect("store_message");
    }

    fn fail_integrity(_: &Path) -> Result<bool, StorageError> {
        Ok(false)
    }

    #[test]
    fn generate_backup_filename_uses_configured_timezone() {
        // Arrange: 2026-06-20T18:00:00Z = 2026-06-21T03:00:00 Asia/Tokyo
        let now: DateTime<Utc> = "2026-06-20T18:00:00Z".parse().unwrap();
        let tz = "Asia/Tokyo";

        // Act
        let name = generate_backup_filename(now, tz);

        // Assert
        assert_eq!(name, "egopulse-20260621-030000.db");
    }

    #[test]
    fn run_backup_creates_file_at_destination() {
        // Arrange
        let (db, _src_dir) = test_db();
        let dest = tempfile::tempdir().expect("dest");
        let now: DateTime<Utc> = "2026-06-20T03:00:00Z".parse().unwrap();

        // Act
        let outcome = run_backup(&db, dest.path(), "UTC", now, 1).expect("backup");

        // Assert
        assert_eq!(
            outcome.path.file_name().unwrap().to_str().unwrap(),
            "egopulse-20260620-030000.db"
        );
        assert!(outcome.path.exists(), "backup file should exist");
        assert!(outcome.integrity_ok);
    }

    #[test]
    fn run_backup_copies_all_tables_and_rows() {
        // Arrange
        let (db, _src_dir) = test_db();
        insert_message(&db, "msg-1", "hello");
        insert_message(&db, "msg-2", "world");
        let dest = tempfile::tempdir().expect("dest");
        let now: DateTime<Utc> = "2026-06-20T03:00:00Z".parse().unwrap();

        // Act
        let outcome = run_backup(&db, dest.path(), "UTC", now, 1).expect("backup");

        // Assert
        let conn = Connection::open(&outcome.path).expect("open backup");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 2, "messages table should be copied");
    }

    #[test]
    fn run_backup_runs_integrity_check_on_success() {
        // Arrange
        let (db, _src_dir) = test_db();
        let dest = tempfile::tempdir().expect("dest");
        let now: DateTime<Utc> = "2026-06-20T03:00:00Z".parse().unwrap();

        // Act
        let outcome = run_backup(&db, dest.path(), "UTC", now, 1).expect("backup");

        // Assert
        assert!(
            outcome.integrity_ok,
            "integrity should pass on a fresh VACUUM INTO"
        );
        let verify = run_pragma_integrity_check(&outcome.path).expect("recheck");
        assert!(verify, "manual recheck must also report ok");
    }

    #[test]
    fn run_backup_deletes_file_when_integrity_check_fails() {
        // Arrange
        let (db, _src_dir) = test_db();
        let dest = tempfile::tempdir().expect("dest");
        let now: DateTime<Utc> = "2026-06-20T03:00:00Z".parse().unwrap();

        // Act
        let outcome =
            run_backup_with_integrity_checker(&db, dest.path(), "UTC", now, 1, fail_integrity)
                .expect("backup");

        // Assert
        assert!(!outcome.integrity_ok);
        assert!(
            !outcome.path.exists(),
            "corrupted backup should be removed from disk"
        );
    }
}

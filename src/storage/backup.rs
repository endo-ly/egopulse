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
    max_generations: u32,
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

    prune_old_backups(dest_dir, max_generations)?;

    Ok(outcome)
}

/// Runs `PRAGMA integrity_check` against the supplied SQLite file.
fn run_pragma_integrity_check(path: &Path) -> Result<bool, StorageError> {
    let conn = Connection::open(path)?;
    let result: String = conn.query_row("PRAGMA integrity_check;", [], |row| row.get(0))?;
    Ok(result == "ok")
}

/// Removes the oldest `egopulse-*.db` files beyond `max_generations`.
///
/// Files are sorted by name (descending timestamp) and the oldest overflow is
/// deleted. Non-matching files (notes, manual copies, subdirectories) are
/// preserved. A missing or empty directory is a no-op.
///
/// # Errors
///
/// Returns [`StorageError`] only when an existing entry cannot be removed.
pub(crate) fn prune_old_backups(dir: &Path, max_generations: u32) -> Result<usize, StorageError> {
    if !dir.exists() {
        return Ok(0);
    }

    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            if is_backup_filename(name) {
                names.push(name.to_string());
            }
        }
    }

    names.sort_unstable_by(|a, b| b.cmp(a));

    let mut removed = 0usize;
    for name in names.into_iter().skip(max_generations as usize) {
        std::fs::remove_file(dir.join(name))?;
        removed += 1;
    }
    Ok(removed)
}

/// Returns `true` when `name` matches the canonical backup file pattern.
fn is_backup_filename(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("egopulse-") else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".db") else {
        return false;
    };
    stem.len() == 15
        && stem.as_bytes().iter().enumerate().all(|(i, b)| {
            if i == 8 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
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

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"stub").expect("touch");
    }

    #[test]
    fn prune_old_backups_deletes_oldest_beyond_max() {
        // Arrange: 5 backups dated 06/01..06/05
        let dir = tempfile::tempdir().expect("dir");
        for day in 1..=5 {
            touch(dir.path(), &format!("egopulse-2026060{day}-030000.db"));
        }

        // Act
        let removed = prune_old_backups(dir.path(), 3).expect("prune");

        // Assert: 2 oldest removed, 3 newest retained
        assert_eq!(removed, 2);
        for day in 3..=5 {
            assert!(
                dir.path()
                    .join(format!("egopulse-2026060{day}-030000.db"))
                    .exists(),
                "newest {day} should remain"
            );
        }
        for day in 1..=2 {
            assert!(
                !dir.path()
                    .join(format!("egopulse-2026060{day}-030000.db"))
                    .exists(),
                "oldest {day} should be deleted"
            );
        }
    }

    #[test]
    fn prune_old_backups_keeps_all_when_below_max() {
        // Arrange
        let dir = tempfile::tempdir().expect("dir");
        for day in 1..=3 {
            touch(dir.path(), &format!("egopulse-2026060{day}-030000.db"));
        }

        // Act
        let removed = prune_old_backups(dir.path(), 5).expect("prune");

        // Assert
        assert_eq!(removed, 0);
        for day in 1..=3 {
            assert!(
                dir.path()
                    .join(format!("egopulse-2026060{day}-030000.db"))
                    .exists(),
                "{day} should remain"
            );
        }
    }

    #[test]
    fn prune_old_backups_handles_missing_or_empty_dir() {
        // Arrange
        let missing = tempfile::tempdir().expect("missing");
        let path_outside = missing.path().join("does_not_exist");
        let empty = tempfile::tempdir().expect("empty");

        // Act
        let removed_missing = prune_old_backups(&path_outside, 3).expect("prune");
        let removed_empty = prune_old_backups(empty.path(), 3).expect("prune");

        // Assert
        assert_eq!(removed_missing, 0);
        assert_eq!(removed_empty, 0);
    }

    #[test]
    fn prune_old_backups_ignores_non_backup_files() {
        // Arrange: 3 valid backups, plus noise
        let dir = tempfile::tempdir().expect("dir");
        for day in 1..=3 {
            touch(dir.path(), &format!("egopulse-2026060{day}-030000.db"));
        }
        touch(dir.path(), "notes.txt");
        touch(dir.path(), "egopulse-manual.db");
        std::fs::create_dir(dir.path().join("old")).expect("mkdir");

        // Act
        let removed = prune_old_backups(dir.path(), 2).expect("prune");

        // Assert: only the oldest valid backup is removed
        assert_eq!(removed, 1);
        assert!(
            !dir.path().join("egopulse-20260601-030000.db").exists(),
            "oldest valid backup removed"
        );
        assert!(dir.path().join("notes.txt").exists(), "notes preserved");
        assert!(
            dir.path().join("egopulse-manual.db").exists(),
            "manual backup preserved"
        );
        assert!(dir.path().join("old").exists(), "subdir preserved");
    }
}

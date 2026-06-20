//! SQLite DB backup module.
//!
//! Provides timestamp-based backup file naming, snapshot creation via
//! `VACUUM INTO`, generation-based pruning, and the next-run schedule
//! calculator used by the periodic backup scheduler.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::Connection;
use tracing::warn;

use crate::config::BackupConfig;
use crate::error::StorageError;
use crate::sleep::scheduler::{resolve_gap, try_date};
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

/// Returns the next UTC instant at which the periodic backup should run.
///
/// When `last_run_at` is `None`, the candidate is the next `time` occurrence
/// (today if still in the future, otherwise tomorrow). When `last_run_at` is
/// `Some`, the candidate is `last_run_at + interval_days` at `time`; if that
/// has already passed, later days are probed up to `interval_days + 1` times.
///
/// Returns `None` when the config is disabled, the timezone is invalid, or
/// the `time` string cannot be parsed as `HH:MM`. DST gaps and folds are
/// resolved via the shared [`try_date`] / [`resolve_gap`] helpers so that
/// backup scheduling matches the sleep scheduler's semantics.
pub(crate) fn compute_next_backup_run(
    config: &BackupConfig,
    timezone: &str,
    now: DateTime<Utc>,
    last_run_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    if !config.enabled {
        return None;
    }
    let tz = timezone.parse::<Tz>().ok()?;
    let time = parse_hhmm(&config.time)?;
    let local_now = now.with_timezone(&tz);

    let base_date = match last_run_at {
        None => local_now.date_naive(),
        Some(last) => {
            let local_last = last.with_timezone(&tz);
            local_last.date_naive() + Duration::days(config.interval_days as i64)
        }
    };

    if let Some(instant) = try_date(tz, base_date, time, &local_now) {
        return Some(instant);
    }

    let cap = config.interval_days.max(1) as i64 + 1;
    let mut candidate = base_date;
    for _ in 0..cap {
        candidate += Duration::days(1);
        if let Some(instant) = try_date(tz, candidate, time, &local_now) {
            return Some(instant);
        }
    }

    None
}

fn parse_hhmm(schedule: &str) -> Option<NaiveTime> {
    let (h, m) = schedule.split_once(':')?;
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    NaiveTime::from_hms_opt(hour, minute, 0)
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

    fn backup_config(interval_days: u32, time: &str) -> BackupConfig {
        BackupConfig {
            enabled: true,
            interval_days,
            time: time.to_string(),
            max_generations: 12,
        }
    }

    fn disabled_config() -> BackupConfig {
        BackupConfig {
            enabled: false,
            ..BackupConfig::default()
        }
    }

    #[test]
    fn compute_next_backup_run_first_run_returns_today_or_tomorrow() {
        // Arrange
        let config = backup_config(1, "14:00");
        let tz = "Asia/Tokyo";

        // Sub-case (a): now = 13:00 JST → today 14:00 JST is future
        let now_a: DateTime<Utc> = "2026-01-15T04:00:00Z".parse().unwrap();
        // Sub-case (b): now = 15:00 JST → today 14:00 JST passed → tomorrow
        let now_b: DateTime<Utc> = "2026-01-15T06:00:00Z".parse().unwrap();

        // Act
        let next_a = compute_next_backup_run(&config, tz, now_a, None).unwrap();
        let next_b = compute_next_backup_run(&config, tz, now_b, None).unwrap();

        // Assert
        assert_eq!(
            next_a,
            "2026-01-15T05:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            next_b,
            "2026-01-16T05:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn compute_next_backup_run_with_last_run_uses_interval() {
        // Arrange: 7-day cadence, last run 2026-06-14 03:00 UTC
        let config = backup_config(7, "03:00");
        let tz = "UTC";
        let now: DateTime<Utc> = "2026-06-16T12:00:00Z".parse().unwrap();
        let last_run: DateTime<Utc> = "2026-06-14T03:00:00Z".parse().unwrap();

        // Act
        let next = compute_next_backup_run(&config, tz, now, Some(last_run)).unwrap();

        // Assert: last_run + 7 days = 2026-06-21 03:00 UTC
        assert_eq!(
            next,
            "2026-06-21T03:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn compute_next_backup_run_returns_none_when_disabled() {
        // Arrange
        let config = disabled_config();
        let now: DateTime<Utc> = "2026-06-20T03:00:00Z".parse().unwrap();

        // Act
        let next = compute_next_backup_run(&config, "UTC", now, None);

        // Assert
        assert!(next.is_none());
    }

    #[test]
    fn compute_next_backup_run_returns_none_for_invalid_time_format() {
        // Arrange
        let cfg_25_99 = BackupConfig {
            time: "25:99".to_string(),
            ..backup_config(1, "03:00")
        };
        let cfg_abc = BackupConfig {
            time: "abc".to_string(),
            ..backup_config(1, "03:00")
        };
        let now: DateTime<Utc> = "2026-06-20T03:00:00Z".parse().unwrap();

        // Act
        let next_25_99 = compute_next_backup_run(&cfg_25_99, "UTC", now, None);
        let next_abc = compute_next_backup_run(&cfg_abc, "UTC", now, None);

        // Assert
        assert!(next_25_99.is_none(), "25:99 should be rejected");
        assert!(next_abc.is_none(), "abc should be rejected");
    }

    #[test]
    fn compute_next_backup_run_handles_dst_gap() {
        // Arrange: America/New_York, DST starts 2026-03-08 02:00 EST → 03:00 EDT.
        // Local 02:30 does not exist; should move to 03:00 EDT.
        let config = backup_config(1, "02:30");
        let tz = "America/New_York";
        let now: DateTime<Utc> = "2026-03-08T06:00:00Z".parse().unwrap();

        // Act
        let next = compute_next_backup_run(&config, tz, now, None).unwrap();

        // Assert: 03:00 EDT = 07:00 UTC
        assert_eq!(
            next,
            "2026-03-08T07:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn compute_next_backup_run_handles_dst_fold() {
        // Arrange: America/New_York, DST ends 2026-11-01 02:00 EDT → 01:00 EST.
        // Local 01:30 happens twice; earliest instant wins (01:30 EDT = 05:30 UTC).
        let config = backup_config(1, "01:30");
        let tz = "America/New_York";
        let now: DateTime<Utc> = "2026-11-01T04:00:00Z".parse().unwrap();

        // Act
        let next = compute_next_backup_run(&config, tz, now, None).unwrap();

        // Assert: 01:30 EDT = 05:30 UTC
        assert_eq!(
            next,
            "2026-11-01T05:30:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }
}

//! Periodic SQLite DB backup scheduler.
//!
//! Computes the next run instant from `BackupConfig`, sleeps in real time, and
//! invokes `run_backup` via the storage pool. Each successful run records its
//! timestamp into `db_meta.backup_last_run`, which feeds the next interval
//! calculation. A `Clock` trait is injected so the scheduler can be exercised
//! deterministically from tests without freezing the real clock.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use tokio::time::sleep;
use tracing::{info, warn};

use crate::error::EgoPulseError;
use crate::runtime::AppState;
use crate::storage::backup::{
    BackupOutcome, compute_next_backup_run, get_backup_last_run, run_backup, upsert_backup_last_run,
};
use crate::storage::call_blocking;

/// Wall-clock abstraction used by the backup scheduler.
///
/// Real production code uses [`RealClock`]; tests inject [`MockClock`] (via
/// `Arc<dyn Clock>`) so the next-run calculation can be driven deterministically
/// while the scheduler still awaits `tokio::time::sleep` in real time.
pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub(crate) struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Runs the periodic backup scheduler using the real system clock.
///
/// Returns when `shutdown` is cancelled or the scheduler is disabled.
pub(crate) async fn run_backup_scheduler_loop(
    state: AppState,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), EgoPulseError> {
    run_backup_scheduler_loop_with_clock(state, Arc::new(RealClock), shutdown).await
}

/// Clock-injectable scheduler entry point used by tests.
///
/// # Errors
///
/// Returns [`EgoPulseError`] when the underlying storage operations fail
/// unrecoverably; transient backup failures are logged and the loop continues.
pub(crate) async fn run_backup_scheduler_loop_with_clock(
    state: AppState,
    clock: Arc<dyn Clock>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), EgoPulseError> {
    if !state.config.db.backup.scheduler_enabled() {
        info!("backup scheduler: disabled, exiting loop");
        return Ok(());
    }

    loop {
        let now = clock.now();
        let last_run = call_blocking(Arc::clone(&state.db), get_backup_last_run).await?;
        let next = match compute_next_backup_run(
            &state.config.db.backup,
            &state.config.timezone,
            now,
            last_run,
        ) {
            Some(t) => t,
            None => {
                info!("backup scheduler: no next run, exiting loop");
                return Ok(());
            }
        };

        let delay = (next - now).to_std().unwrap_or(StdDuration::ZERO);
        info!(
            next_run = %next.to_rfc3339(),
            delay_secs = delay.as_secs(),
            "backup scheduler: waiting"
        );
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("backup scheduler: shutdown requested, exiting loop");
                return Ok(());
            }
            _ = sleep(delay) => {}
        }

        match run_periodic_backup_once(&state, clock.now()).await {
            Ok(outcome) if outcome.integrity_ok => {
                info!(path = %outcome.path.display(), "backup scheduler: created");
            }
            Ok(_) => {
                warn!("backup scheduler: snapshot integrity check failed");
            }
            Err(error) => warn!(%error, "backup scheduler: failed"),
        }
    }
}

/// Executes a single periodic backup and records the timestamp.
///
/// Backs up the main `egopulse.db` first, then the `secret.db` if the secret
/// store is enabled. A secret-db backup failure is logged as a warning but
/// does not prevent the `backup_last_run` timestamp from being recorded.
///
/// Split out of the loop so unit tests can exercise the persistence side
/// effect without driving the full scheduler cadence.
///
/// # Errors
///
/// Returns [`EgoPulseError`] when either the backup or the timestamp write fails.
pub(crate) async fn run_periodic_backup_once(
    state: &AppState,
    now: DateTime<Utc>,
) -> Result<BackupOutcome, EgoPulseError> {
    let max_generations = state.config.db.backup.max_generations;
    let dest_dir = state.config.backup_dir();
    let tz = state.config.timezone.clone();

    let outcome = call_blocking(Arc::clone(&state.db), move |db| {
        run_backup(db, &dest_dir, &tz, now, max_generations, "egopulse")
    })
    .await?;

    if let Some(secret_db) = &state.secret_db {
        let dest_dir = state.config.backup_dir();
        let tz = state.config.timezone.clone();
        if let Err(error) = call_blocking(Arc::clone(secret_db), move |db| {
            run_backup(db, &dest_dir, &tz, now, max_generations, "secret")
        })
        .await
        {
            warn!(%error, "secret db backup failed");
        }
    }

    call_blocking(Arc::clone(&state.db), move |db| {
        upsert_backup_last_run(db, now)
    })
    .await?;

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackupConfig, DatabaseConfig};
    use crate::test_util::{build_state_with_config, test_config};
    use chrono::TimeZone;

    /// `MockClock` returns a fixed instant so tests can drive the next-run
    /// calculation deterministically without freezing the real clock.
    pub(crate) struct MockClock {
        now: std::sync::Mutex<DateTime<Utc>>,
    }

    impl MockClock {
        pub(crate) fn new(now: DateTime<Utc>) -> Self {
            Self {
                now: std::sync::Mutex::new(now),
            }
        }
    }

    impl Clock for MockClock {
        fn now(&self) -> DateTime<Utc> {
            *self.now.lock().expect("mockclock poisoned")
        }
    }

    fn state_with_backup(state_root: &str, backup: BackupConfig) -> AppState {
        let mut config = test_config(state_root);
        config.db = DatabaseConfig { backup };
        build_state_with_config(config, None, None, None, None)
    }

    #[tokio::test]
    async fn periodic_backup_writes_last_run_to_db_meta() {
        // Arrange
        let dir = tempfile::tempdir().expect("dir");
        let state_root = dir.path().to_str().expect("utf8");
        let backup = BackupConfig {
            enabled: true,
            interval_days: 7,
            time: "03:00".to_string(),
            max_generations: 12,
        };
        let state = state_with_backup(state_root, backup);
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 3, 0, 0).unwrap();

        // Act
        let outcome = run_periodic_backup_once(&state, now).await.expect("backup");

        // Assert
        assert!(outcome.integrity_ok);
        let stored = call_blocking(Arc::clone(&state.db), get_backup_last_run)
            .await
            .expect("read")
            .expect("last_run present");
        assert_eq!(stored, now);
    }

    #[tokio::test]
    async fn run_backup_scheduler_loop_with_clock_executes_backup_when_delay_elapses() {
        // Arrange: MockClock starts 500ms before the target 03:00:00 UTC.
        // The scheduler will compute delay = 500ms in real time and dispatch.
        let dir = tempfile::tempdir().expect("dir");
        let state_root = dir.path().to_str().expect("utf8");
        let backup = BackupConfig {
            enabled: true,
            interval_days: 1,
            time: "03:00".to_string(),
            max_generations: 12,
        };
        let state = state_with_backup(state_root, backup);
        let mock_start = Utc.with_ymd_and_hms(2026, 6, 20, 2, 59, 59).unwrap()
            + chrono::Duration::milliseconds(500);
        let clock: Arc<dyn Clock> = Arc::new(MockClock::new(mock_start));

        // Act: spawn scheduler and poll for the last_run record to be persisted.
        // Waiting on `db_meta.backup_last_run` (not just the file) ensures the
        // scheduler has finished its full cycle, avoiding an abort between the
        // VACUUM INTO write and the upsert.
        let task_state = state.clone();
        let task_clock = Arc::clone(&clock);
        let handle = tokio::spawn(async move {
            run_backup_scheduler_loop_with_clock(
                task_state,
                task_clock,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
        });

        let db_for_poll = Arc::clone(&state.db);
        let completed = tokio::time::timeout(StdDuration::from_secs(5), async {
            loop {
                if let Ok(Some(_)) =
                    call_blocking(Arc::clone(&db_for_poll), get_backup_last_run).await
                {
                    return;
                }
                sleep(StdDuration::from_millis(50)).await;
            }
        })
        .await;
        handle.abort();

        // Assert
        assert!(
            completed.is_ok(),
            "last_run should be persisted within timeout"
        );
    }

    #[tokio::test]
    async fn run_backup_scheduler_loop_exits_immediately_when_disabled() {
        // Arrange
        let dir = tempfile::tempdir().expect("dir");
        let state_root = dir.path().to_str().expect("utf8");
        let backup = BackupConfig {
            enabled: false,
            ..BackupConfig::default()
        };
        let state = state_with_backup(state_root, backup);
        let backup_dir = state.config.backup_dir();

        // Act
        let result =
            run_backup_scheduler_loop(state.clone(), tokio_util::sync::CancellationToken::new())
                .await;

        // Assert
        assert!(result.is_ok(), "disabled scheduler should exit cleanly");
        assert!(
            !backup_dir.exists() || backup_dir.read_dir().unwrap().count() == 0,
            "no backup file should be created"
        );
    }

    fn backup_enabled_config() -> BackupConfig {
        BackupConfig {
            enabled: true,
            interval_days: 7,
            time: "03:00".to_string(),
            max_generations: 12,
        }
    }

    fn count_files_starting_with(dir: &std::path::Path, prefix: &str) -> usize {
        std::fs::read_dir(dir)
            .expect("backup dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(prefix))
            })
            .count()
    }

    #[tokio::test]
    async fn backup_creates_secret_db_snapshot_when_present() {
        // Arrange
        let dir = tempfile::tempdir().expect("dir");
        let state_root = dir.path().to_str().expect("utf8");
        let mut state = state_with_backup(state_root, backup_enabled_config());
        let secret_path = dir.path().join("runtime").join("secret.db");
        let secret_db =
            Arc::new(crate::storage::Database::new_secret(&secret_path).expect("secret db"));
        state.secret_db = Some(secret_db);
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 3, 0, 0).unwrap();

        // Act
        let outcome = run_periodic_backup_once(&state, now).await.expect("backup");

        // Assert
        assert!(outcome.integrity_ok);
        let backup_dir = state.config.backup_dir();
        assert_eq!(
            count_files_starting_with(&backup_dir, "egopulse-"),
            1,
            "egopulse backup should exist"
        );
        assert_eq!(
            count_files_starting_with(&backup_dir, "secret-"),
            1,
            "secret backup should exist"
        );
    }

    #[tokio::test]
    async fn backup_skips_secret_db_when_not_present() {
        // Arrange
        let dir = tempfile::tempdir().expect("dir");
        let state_root = dir.path().to_str().expect("utf8");
        let state = state_with_backup(state_root, backup_enabled_config());
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 3, 0, 0).unwrap();

        // Act
        let outcome = run_periodic_backup_once(&state, now).await.expect("backup");

        // Assert
        assert!(outcome.integrity_ok);
        let backup_dir = state.config.backup_dir();
        assert_eq!(
            count_files_starting_with(&backup_dir, "egopulse-"),
            1,
            "egopulse backup should exist"
        );
        assert_eq!(
            count_files_starting_with(&backup_dir, "secret-"),
            0,
            "no secret backup should exist"
        );
    }
}

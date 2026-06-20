//! SQLite DB backup module.
//!
//! Provides timestamp-based backup file naming, snapshot creation via
//! `VACUUM INTO`, generation-based pruning, and the next-run schedule
//! calculator used by the periodic backup scheduler.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}

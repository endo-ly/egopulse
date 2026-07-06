use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

/// Explicit delivery destination for pulse notifications.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeliverySpec {
    pub channel: String,
    pub external_chat_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PulseDefinition {
    pub default_delivery: Option<DeliverySpec>,
    pub intentions: Vec<TemporalIntention>,
    pub body: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TemporalIntention {
    pub id: String,
    pub enabled: bool,
    pub schedule: TemporalSchedule,
    pub attention: String,
    pub delivery: Option<DeliverySpec>,
}

#[derive(Clone, Debug)]
pub(crate) enum TemporalSchedule {
    Daily {
        at: String,
    },
    Weekly {
        day: String,
        at: String,
    },
    /// Fire every `interval_days` days, anchored to the most recent successful
    /// activation. Re-evaluates as due each day once the interval has elapsed
    /// until a run succeeds; see `docs/pulse.md` §4.
    Interval {
        interval_days: u32,
        at: String,
    },
}

#[derive(Debug, Error)]
pub(crate) enum PulseParseError {
    #[error("pulse_parse_error: {agent_id}: {detail}")]
    ParseFailed { agent_id: String, detail: String },
    #[error("pulse_unsafe_agent_id: {agent_id}")]
    UnsafeAgentId { agent_id: String },
    #[error("pulse_duplicate_intention_id: {agent_id}: {id}")]
    DuplicateIntentionId { agent_id: String, id: String },
    #[error("pulse_invalid_schedule: {agent_id}: intention={intention_id} {detail}")]
    InvalidSchedule {
        agent_id: String,
        intention_id: String,
        detail: String,
    },
    #[error("pulse_invalid_delivery: {agent_id}: {detail}")]
    InvalidDelivery { agent_id: String, detail: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PulseFrontMatter {
    #[allow(dead_code)]
    version: u32,
    #[serde(default)]
    intentions: Vec<IntentionRaw>,
    default_delivery: Option<DeliveryRaw>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentionRaw {
    id: String,
    #[serde(default = "default_true")]
    enabled: bool,
    schedule: ScheduleRaw,
    attention: String,
    delivery: Option<DeliveryRaw>,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleRaw {
    kind: String,
    at: String,
    day: Option<String>,
    interval_days: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryRaw {
    channel: String,
    external_chat_id: String,
}

const VALID_DELIVERY_CHANNELS: &[&str] = &["discord", "telegram"];

fn validate_delivery_raw(
    agent_id: &str,
    raw: &DeliveryRaw,
    source: &str,
) -> Result<DeliverySpec, PulseParseError> {
    let channel = raw.channel.trim().to_ascii_lowercase();
    if !VALID_DELIVERY_CHANNELS.contains(&channel.as_str()) {
        return Err(PulseParseError::InvalidDelivery {
            agent_id: agent_id.to_string(),
            detail: format!(
                "{source}: invalid delivery channel: {channel}, expected one of {}",
                VALID_DELIVERY_CHANNELS.join(", ")
            ),
        });
    }
    let external_chat_id = raw.external_chat_id.trim().to_string();
    if external_chat_id.is_empty() {
        return Err(PulseParseError::InvalidDelivery {
            agent_id: agent_id.to_string(),
            detail: format!("{source}: delivery external_chat_id must not be empty"),
        });
    }
    Ok(DeliverySpec {
        channel,
        external_chat_id,
    })
}

#[cfg(test)]
pub(crate) fn parse_pulse_definition(content: &str) -> Result<PulseDefinition, PulseParseError> {
    parse_pulse_definition_inner(content, "")
}

fn parse_pulse_definition_inner(
    content: &str,
    agent_id: &str,
) -> Result<PulseDefinition, PulseParseError> {
    let content = content.replace("\r\n", "\n");
    let trimmed = content.trim();

    if trimmed.is_empty() {
        return Ok(PulseDefinition {
            default_delivery: None,
            intentions: Vec::new(),
            body: String::new(),
        });
    }

    let Some(rest) = trimmed.strip_prefix("---") else {
        return Err(PulseParseError::ParseFailed {
            agent_id: agent_id.to_string(),
            detail: "PULSE.md content must start with YAML front matter".to_string(),
        });
    };

    let Some(rest) = rest.strip_prefix('\n') else {
        return Err(PulseParseError::ParseFailed {
            agent_id: agent_id.to_string(),
            detail: "PULSE.md front matter opening marker must be followed by a newline"
                .to_string(),
        });
    };

    let Some(end) = rest.find("\n---") else {
        return Err(PulseParseError::ParseFailed {
            agent_id: agent_id.to_string(),
            detail: "PULSE.md front matter closing marker is missing".to_string(),
        });
    };

    let frontmatter_str = &rest[..end];
    let body = rest[end + 4..].trim().to_string();

    let fm: PulseFrontMatter =
        yaml_serde::from_str(frontmatter_str).map_err(|e| PulseParseError::ParseFailed {
            agent_id: agent_id.to_string(),
            detail: e.to_string(),
        })?;

    let default_delivery = fm
        .default_delivery
        .as_ref()
        .map(|raw| validate_delivery_raw(agent_id, raw, "default_delivery"))
        .transpose()?;

    let mut intentions = Vec::with_capacity(fm.intentions.len());
    let mut seen_ids = HashSet::new();

    for raw in fm.intentions {
        if seen_ids.contains(&raw.id) {
            return Err(PulseParseError::DuplicateIntentionId {
                agent_id: agent_id.to_string(),
                id: raw.id,
            });
        }
        seen_ids.insert(raw.id.clone());

        let schedule = validate_and_build_schedule(agent_id, &raw)?;
        let delivery = raw
            .delivery
            .as_ref()
            .map(|d| validate_delivery_raw(agent_id, d, &format!("intention '{}'", raw.id)))
            .transpose()?;
        intentions.push(TemporalIntention {
            id: raw.id,
            enabled: raw.enabled,
            schedule,
            attention: raw.attention,
            delivery,
        });
    }

    Ok(PulseDefinition {
        default_delivery,
        intentions,
        body,
    })
}

fn validate_and_build_schedule(
    agent_id: &str,
    raw: &IntentionRaw,
) -> Result<TemporalSchedule, PulseParseError> {
    match raw.schedule.kind.as_str() {
        "daily" => {
            validate_hhmm(agent_id, &raw.id, &raw.schedule.at)?;
            Ok(TemporalSchedule::Daily {
                at: raw.schedule.at.clone(),
            })
        }
        "weekly" => {
            let day = raw
                .schedule
                .day
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            validate_weekday(agent_id, &raw.id, &day)?;
            validate_hhmm(agent_id, &raw.id, &raw.schedule.at)?;
            Ok(TemporalSchedule::Weekly {
                day,
                at: raw.schedule.at.clone(),
            })
        }
        "interval" => {
            let interval_days = raw.schedule.interval_days.unwrap_or(0);
            validate_interval_days(agent_id, &raw.id, interval_days)?;
            validate_hhmm(agent_id, &raw.id, &raw.schedule.at)?;
            Ok(TemporalSchedule::Interval {
                interval_days,
                at: raw.schedule.at.clone(),
            })
        }
        "once" => Err(PulseParseError::InvalidSchedule {
            agent_id: agent_id.to_string(),
            intention_id: raw.id.clone(),
            detail: "once schedule is no longer supported; use daily or weekly instead".to_string(),
        }),
        other => Err(PulseParseError::InvalidSchedule {
            agent_id: agent_id.to_string(),
            intention_id: raw.id.clone(),
            detail: format!("unknown schedule kind: {other}"),
        }),
    }
}

fn validate_hhmm(agent_id: &str, intention_id: &str, at: &str) -> Result<(), PulseParseError> {
    let parts: Vec<&str> = at.split(':').collect();
    if parts.len() != 2 {
        return Err(PulseParseError::InvalidSchedule {
            agent_id: agent_id.to_string(),
            intention_id: intention_id.to_string(),
            detail: format!("invalid time format: {at}, expected HH:MM"),
        });
    }
    let hour: u32 = parts[0].parse().unwrap_or(24);
    let minute: u32 = parts[1].parse().unwrap_or(60);
    if hour > 23 || minute > 59 {
        return Err(PulseParseError::InvalidSchedule {
            agent_id: agent_id.to_string(),
            intention_id: intention_id.to_string(),
            detail: format!("invalid time: {at}, hour must be 0-23 and minute must be 0-59"),
        });
    }
    Ok(())
}

fn validate_interval_days(
    agent_id: &str,
    intention_id: &str,
    interval_days: u32,
) -> Result<(), PulseParseError> {
    if interval_days == 0 {
        return Err(PulseParseError::InvalidSchedule {
            agent_id: agent_id.to_string(),
            intention_id: intention_id.to_string(),
            detail: "interval_days must be >= 1".to_string(),
        });
    }
    Ok(())
}

const VALID_WEEKDAYS: &[&str] = &["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

fn validate_weekday(agent_id: &str, intention_id: &str, day: &str) -> Result<(), PulseParseError> {
    if !VALID_WEEKDAYS.contains(&day) {
        return Err(PulseParseError::InvalidSchedule {
            agent_id: agent_id.to_string(),
            intention_id: intention_id.to_string(),
            detail: format!("invalid weekday: {day}, expected one of mon,tue,wed,thu,fri,sat,sun"),
        });
    }
    Ok(())
}

fn is_safe_agent_id(id: &str) -> bool {
    !id.is_empty()
        && !id.trim().is_empty()
        && !id.contains("..")
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains(':')
}

/// Format a [`TemporalSchedule`] as a human-readable English string.
///
/// # Examples
/// - `Daily { at: "08:00" }` → `"daily 08:00"`
/// - `Weekly { day: "sun", at: "21:00" }` → `"weekly sun 21:00"`
/// - `Interval { interval_days: 3, at: "09:00" }` → `"every 3 days 09:00"`
pub(crate) fn format_schedule(schedule: &TemporalSchedule) -> String {
    match schedule {
        TemporalSchedule::Daily { at } => format!("daily {at}"),
        TemporalSchedule::Weekly { day, at } => format!("weekly {day} {at}"),
        TemporalSchedule::Interval { interval_days, at } => {
            format!("every {interval_days} days {at}")
        }
    }
}

pub(crate) fn load_pulse_definition(
    state_root: &Path,
    agent_id: &str,
) -> Result<PulseDefinition, PulseParseError> {
    if !is_safe_agent_id(agent_id) {
        return Err(PulseParseError::UnsafeAgentId {
            agent_id: agent_id.to_string(),
        });
    }

    let path = state_root.join("agents").join(agent_id).join("PULSE.md");

    if !path.exists() {
        return Ok(PulseDefinition {
            default_delivery: None,
            intentions: Vec::new(),
            body: String::new(),
        });
    }

    let content = std::fs::read_to_string(&path).map_err(|e| PulseParseError::ParseFailed {
        agent_id: agent_id.to_string(),
        detail: format!("failed to read PULSE.md: {e}"),
    })?;

    parse_pulse_definition_inner(&content, agent_id)
}

// ===========================================================================
// Due Resolver
// ===========================================================================

use chrono::{DateTime, Datelike, LocalResult, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

/// Result of a due check for a single intention.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DueCheck {
    /// Whether the intention is currently due.
    pub due: bool,
    /// Unique key identifying the current evaluation period for deduplication.
    pub due_key: String,
}

/// Check if a temporal intention is due at the given time and produce its deduplication key.
///
/// `agent_id` identifies the agent for due_key generation.
/// `now` is the current UTC time.
/// `timezone` is the IANA timezone for daily/weekly/interval evaluation (e.g. "Asia/Tokyo").
/// `last_success_at` is the most recent successful activation time for this
/// `(agent_id, intention_id)`. Only `interval` schedules consult it; `daily`
/// and `weekly` ignore it.
pub(crate) fn check_due(
    agent_id: &str,
    intention: &TemporalIntention,
    now: DateTime<Utc>,
    timezone: &str,
    last_success_at: Option<DateTime<Utc>>,
) -> DueCheck {
    let due_key = generate_due_key(agent_id, intention, now, timezone);
    let due = match &intention.schedule {
        TemporalSchedule::Daily { at } => is_daily_due(at, now, timezone),
        TemporalSchedule::Weekly { day, at } => is_weekly_due(day, at, now, timezone),
        TemporalSchedule::Interval { interval_days, at } => {
            is_interval_due(*interval_days, at, now, timezone, last_success_at)
        }
    };
    DueCheck { due, due_key }
}

/// Generate the deduplication key for a given intention at the current evaluation time.
///
/// Format per schedule kind:
/// - daily:    `{agent_id}:{intention_id}:{YYYY-MM-DD}` (local date)
/// - weekly:   `{agent_id}:{intention_id}:{YYYY-WNN}`   (ISO week)
/// - interval: `{agent_id}:{intention_id}:{YYYY-MM-DD}` (local evaluation date;
///   advances each day so a failed run can be retried on the next day while
///   the interval window remains open)
pub(crate) fn generate_due_key(
    agent_id: &str,
    intention: &TemporalIntention,
    now: DateTime<Utc>,
    timezone: &str,
) -> String {
    let tz: Tz = timezone.parse().unwrap_or(Tz::UTC);
    let local_now = now.with_timezone(&tz);
    match &intention.schedule {
        TemporalSchedule::Daily { .. } | TemporalSchedule::Interval { .. } => {
            format!(
                "{agent_id}:{}:{}",
                intention.id,
                local_now.format("%Y-%m-%d")
            )
        }
        TemporalSchedule::Weekly { .. } => {
            let iso = local_now.iso_week();
            format!(
                "{agent_id}:{}:{}-W{:02}",
                intention.id,
                iso.year(),
                iso.week()
            )
        }
    }
}

/// Evaluate daily schedule: parse `HH:MM`, construct today's local time in the
/// configured timezone, and check if `now` has passed it.
///
/// DST handling:
/// - **Gap** (non-existent local time, e.g. spring forward): treated as not due (skip).
/// - **Fold** (ambiguous local time, e.g. fall back): uses the earlier occurrence.
fn is_daily_due(at: &str, now: DateTime<Utc>, timezone: &str) -> bool {
    let time = match parse_hhmm(at) {
        Some(t) => t,
        None => {
            tracing::warn!("invalid time format in daily schedule: {at}");
            return false;
        }
    };
    let tz: Tz = match timezone.parse() {
        Ok(tz) => tz,
        Err(e) => {
            tracing::warn!("invalid timezone \"{timezone}\": {e}");
            return false;
        }
    };

    let local_now = now.with_timezone(&tz);
    let naive_dt = local_now.date_naive().and_time(time);

    let target = match tz.from_local_datetime(&naive_dt) {
        LocalResult::None => {
            tracing::debug!("skipping daily intention: local time {at} falls in DST gap");
            return false;
        }
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(earliest, _latest) => earliest,
    };

    now >= target.with_timezone(&Utc)
}

/// Evaluate weekly schedule: first check if the current local weekday matches
/// the configured `day`, then delegate to the daily time check.
fn is_weekly_due(day: &str, at: &str, now: DateTime<Utc>, timezone: &str) -> bool {
    let target_weekday = match parse_weekday(day) {
        Some(w) => w,
        None => {
            tracing::warn!("invalid weekday in weekly schedule: {day}");
            return false;
        }
    };
    let tz: Tz = match timezone.parse() {
        Ok(tz) => tz,
        Err(e) => {
            tracing::warn!("invalid timezone \"{timezone}\": {e}");
            return false;
        }
    };

    let local_now = now.with_timezone(&tz);
    if local_now.weekday() != target_weekday {
        return false;
    }

    is_daily_due(at, now, timezone)
}

/// Evaluate interval schedule: fire when the local date has reached
/// `last_success_date + interval_days`, then delegate to the daily time
/// check.
///
/// - `last_success_at = None` (first activation): only the time-of-day check applies.
/// - Once due, the same condition stays true on subsequent days until a run
///   succeeds, because `last_success_at` only advances on success. The
///   per-day deduplication is handled by the `due_key` (local evaluation
///   date), so a failed run is retried on the following day.
fn is_interval_due(
    interval_days: u32,
    at: &str,
    now: DateTime<Utc>,
    timezone: &str,
    last_success_at: Option<DateTime<Utc>>,
) -> bool {
    let tz: Tz = match timezone.parse() {
        Ok(tz) => tz,
        Err(e) => {
            tracing::warn!("invalid timezone \"{timezone}\": {e}");
            return false;
        }
    };

    let local_today = now.with_timezone(&tz).date_naive();
    if let Some(last_success) = last_success_at {
        let target_date = last_success.with_timezone(&tz).date_naive()
            + chrono::Duration::days(interval_days as i64);
        if local_today < target_date {
            return false;
        }
    }

    is_daily_due(at, now, timezone)
}

/// Parse a `HH:MM` time string into a [`NaiveTime`].
fn parse_hhmm(at: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(at, "%H:%M").ok()
}

/// Map a lowercase weekday abbreviation to [`Weekday`].
fn parse_weekday(day: &str) -> Option<Weekday> {
    match day {
        "mon" => Some(Weekday::Mon),
        "tue" => Some(Weekday::Tue),
        "wed" => Some(Weekday::Wed),
        "thu" => Some(Weekday::Thu),
        "fri" => Some(Weekday::Fri),
        "sat" => Some(Weekday::Sat),
        "sun" => Some(Weekday::Sun),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_schedule_daily() {
        let schedule = TemporalSchedule::Daily {
            at: "08:00".to_string(),
        };
        assert_eq!(format_schedule(&schedule), "daily 08:00");
    }

    #[test]
    fn format_schedule_weekly() {
        let schedule = TemporalSchedule::Weekly {
            day: "sun".to_string(),
            at: "21:00".to_string(),
        };
        assert_eq!(format_schedule(&schedule), "weekly sun 21:00");
    }

    #[test]
    fn format_schedule_interval() {
        let schedule = TemporalSchedule::Interval {
            interval_days: 3,
            at: "09:00".to_string(),
        };
        assert_eq!(format_schedule(&schedule), "every 3 days 09:00");
    }

    fn valid_pulse_md() -> String {
        "\
---
version: 1
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: |
      Check today's schedule and unresolved items.
  - id: weekly_reflection
    schedule:
      kind: weekly
      day: sun
      at: \"21:00\"
    attention: |
      Reflect on the week.
---

# PULSE

## Notes

- Don't notify for trivial changes.
"
        .to_string()
    }

    #[test]
    fn parse_pulse_definition_loads_front_matter_and_body() {
        let content = valid_pulse_md();

        let result = parse_pulse_definition(&content).expect("should parse successfully");

        assert_eq!(result.intentions.len(), 2);
        assert_eq!(result.intentions[0].id, "morning_review");
        assert!(result.body.contains("# PULSE"));
        assert!(result.body.contains("Don't notify for trivial changes."));
    }

    #[test]
    fn parse_daily_and_weekly_intentions() {
        let content = valid_pulse_md();

        let result = parse_pulse_definition(&content).expect("should parse successfully");

        assert!(matches!(
            &result.intentions[0].schedule,
            TemporalSchedule::Daily { at } if at == "09:00"
        ));
        assert!(matches!(
            &result.intentions[1].schedule,
            TemporalSchedule::Weekly { day, at } if day == "sun" && at == "21:00"
        ));

        assert!(
            result.intentions[0]
                .attention
                .contains("Check today's schedule")
        );
        assert!(
            result.intentions[1]
                .attention
                .contains("Reflect on the week")
        );
    }

    #[test]
    fn parse_rejects_once_schedule() {
        let content = "\
---
version: 1
intentions:
  - id: event_check
    schedule:
      kind: once
      at: \"2026-05-12T18:00:00+09:00\"
    attention: test
---

body
";
        let err = parse_pulse_definition(content).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidSchedule { ref intention_id, .. } if intention_id == "event_check"),
            "expected InvalidSchedule for once, got: {err}"
        );
    }

    #[test]
    fn parse_interval_intention() {
        let content = "\
---
version: 1
intentions:
  - id: periodic_report
    schedule:
      kind: interval
      interval_days: 3
      at: \"09:00\"
    attention: test
---

body
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert_eq!(result.intentions.len(), 1);
        assert!(matches!(
            &result.intentions[0].schedule,
            TemporalSchedule::Interval { interval_days, at }
                if *interval_days == 3 && at == "09:00"
        ));
    }

    #[test]
    fn parse_rejects_zero_interval_days() {
        let content = "\
---
version: 1
intentions:
  - id: periodic_report
    schedule:
      kind: interval
      interval_days: 0
      at: \"09:00\"
    attention: test
---

body
";
        let err = parse_pulse_definition(content).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidSchedule { ref intention_id, .. } if intention_id == "periodic_report"),
            "expected InvalidSchedule for interval_days=0, got: {err}"
        );
    }

    #[test]
    fn parse_rejects_interval_with_missing_interval_days() {
        let content = "\
---
version: 1
intentions:
  - id: periodic_report
    schedule:
      kind: interval
      at: \"09:00\"
    attention: test
---

body
";
        let err = parse_pulse_definition(content).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidSchedule { ref intention_id, .. } if intention_id == "periodic_report"),
            "missing interval_days should be rejected, got: {err}"
        );
    }

    #[test]
    fn parse_rejects_duplicate_intention_ids() {
        let content = "\
---
version: 1
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: first
  - id: morning_review
    schedule:
      kind: daily
      at: \"10:00\"
    attention: second
---

body
";

        let result = parse_pulse_definition(content);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PulseParseError::DuplicateIntentionId { ref id, .. } if id == "morning_review"),
            "expected DuplicateIntentionId, got: {err}"
        );
    }

    #[test]
    fn parse_rejects_invalid_hhmm_and_weekday() {
        let invalid_time = "\
---
version: 1
intentions:
  - id: bad_time
    schedule:
      kind: daily
      at: \"25:00\"
    attention: test
---

body
";
        let err = parse_pulse_definition(invalid_time).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidSchedule { ref intention_id, .. } if intention_id == "bad_time"),
            "expected InvalidSchedule for bad time, got: {err}"
        );

        let invalid_weekday = "\
---
version: 1
intentions:
  - id: bad_day
    schedule:
      kind: weekly
      day: xyz
      at: \"09:00\"
    attention: test
---

body
";
        let err = parse_pulse_definition(invalid_weekday).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidSchedule { ref intention_id, .. } if intention_id == "bad_day"),
            "expected InvalidSchedule for bad weekday, got: {err}"
        );
    }

    #[test]
    fn load_missing_pulse_definition_returns_empty() {
        let dir = tempfile::tempdir().unwrap();

        let result = load_pulse_definition(dir.path(), "nonexistent").unwrap();

        assert!(result.intentions.is_empty());
        assert!(result.body.is_empty());
    }

    #[test]
    fn load_rejects_unsafe_agent_id() {
        let dir = tempfile::tempdir().unwrap();

        let err = load_pulse_definition(dir.path(), "../etc").unwrap_err();
        assert!(
            matches!(err, PulseParseError::UnsafeAgentId { ref agent_id } if agent_id == "../etc"),
            "expected UnsafeAgentId, got: {err}"
        );

        let err = load_pulse_definition(dir.path(), "foo/bar").unwrap_err();
        assert!(
            matches!(err, PulseParseError::UnsafeAgentId { ref agent_id } if agent_id == "foo/bar"),
            "expected UnsafeAgentId, got: {err}"
        );

        let err = load_pulse_definition(dir.path(), "").unwrap_err();
        assert!(
            matches!(err, PulseParseError::UnsafeAgentId { ref agent_id } if agent_id.is_empty()),
            "expected UnsafeAgentId for empty, got: {err}"
        );

        let err = load_pulse_definition(dir.path(), "foo\\bar").unwrap_err();
        assert!(
            matches!(err, PulseParseError::UnsafeAgentId { .. }),
            "expected UnsafeAgentId for backslash, got: {err}"
        );

        let err = load_pulse_definition(dir.path(), "foo:bar").unwrap_err();
        assert!(
            matches!(err, PulseParseError::UnsafeAgentId { .. }),
            "expected UnsafeAgentId for colon, got: {err}"
        );
    }

    #[test]
    fn scheduler_warns_and_continues_on_pulse_parse_error() {
        let invalid_yaml = "\
---
version: not_a_number
---

body
";
        let result = parse_pulse_definition(invalid_yaml);
        assert!(result.is_err(), "invalid YAML should return an error");

        let empty = "";
        let result = parse_pulse_definition(empty).unwrap();
        assert!(result.intentions.is_empty());

        let no_frontmatter = "# Just a heading\nSome text without front matter";
        let result = parse_pulse_definition(no_frontmatter);
        assert!(
            result.is_err(),
            "non-empty PULSE.md without front matter should be rejected"
        );
    }

    // --- Due Resolver tests ---

    fn make_daily(at: &str) -> TemporalIntention {
        TemporalIntention {
            id: "test_intention".to_string(),
            enabled: true,
            schedule: TemporalSchedule::Daily { at: at.to_string() },
            attention: String::new(),
            delivery: None,
        }
    }

    fn make_weekly(day: &str, at: &str) -> TemporalIntention {
        TemporalIntention {
            id: "test_intention".to_string(),
            enabled: true,
            schedule: TemporalSchedule::Weekly {
                day: day.to_string(),
                at: at.to_string(),
            },
            attention: String::new(),
            delivery: None,
        }
    }

    fn make_interval(interval_days: u32, at: &str) -> TemporalIntention {
        TemporalIntention {
            id: "test_intention".to_string(),
            enabled: true,
            schedule: TemporalSchedule::Interval {
                interval_days,
                at: at.to_string(),
            },
            attention: String::new(),
            delivery: None,
        }
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn daily_due_after_local_time() {
        let intention = make_daily("09:00");
        let now = utc(2026, 5, 10, 0, 1, 0);
        let result = check_due("lyre", &intention, now, "Asia/Tokyo", None);
        assert!(result.due);
    }

    #[test]
    fn daily_not_due_before_local_time() {
        let intention = make_daily("09:00");
        let now = utc(2026, 5, 9, 23, 59, 0);
        let result = check_due("lyre", &intention, now, "Asia/Tokyo", None);
        assert!(!result.due);
    }

    #[test]
    fn weekly_due_only_on_matching_day() {
        let intention = make_weekly("sun", "21:00");
        let sunday = utc(2026, 5, 10, 12, 1, 0);
        assert!(check_due("lyre", &intention, sunday, "Asia/Tokyo", None).due);
        let saturday = utc(2026, 5, 9, 12, 1, 0);
        assert!(!check_due("lyre", &intention, saturday, "Asia/Tokyo", None).due);
    }

    #[test]
    fn due_key_daily_uses_local_date() {
        let intention = TemporalIntention {
            id: "morning_review".to_string(),
            enabled: true,
            schedule: TemporalSchedule::Daily {
                at: "09:00".to_string(),
            },
            attention: String::new(),
            delivery: None,
        };
        let now = utc(2026, 5, 10, 0, 0, 0);
        let key = generate_due_key("lyre", &intention, now, "Asia/Tokyo");
        assert_eq!(key, "lyre:morning_review:2026-05-10");
    }

    #[test]
    fn due_key_weekly_uses_iso_week() {
        let intention = TemporalIntention {
            id: "weekly_reflection".to_string(),
            enabled: true,
            schedule: TemporalSchedule::Weekly {
                day: "sun".to_string(),
                at: "21:00".to_string(),
            },
            attention: String::new(),
            delivery: None,
        };
        let now = utc(2026, 5, 10, 12, 0, 0);
        let key = generate_due_key("kitara", &intention, now, "Asia/Tokyo");
        assert_eq!(key, "kitara:weekly_reflection:2026-W19");
    }

    // --- interval schedule tests ---

    #[test]
    fn interval_due_on_first_run_without_last_success() {
        // interval=3, at=09:00 JST. last_success=None → only time-of-day applies.
        let intention = make_interval(3, "09:00");
        let before = utc(2026, 7, 5, 23, 59, 0); // 08:59 JST
        assert!(!check_due("lyre", &intention, before, "Asia/Tokyo", None).due);
        let after = utc(2026, 7, 6, 0, 1, 0); // 09:01 JST
        assert!(check_due("lyre", &intention, after, "Asia/Tokyo", None).due);
    }

    #[test]
    fn interval_not_due_before_interval_elapsed() {
        // last_success = 2026-07-03 09:01 JST, interval=3 → next target = 2026-07-06
        let intention = make_interval(3, "09:00");
        let last_success = utc(2026, 7, 3, 0, 1, 0); // 2026-07-03 09:01 JST

        // 2026-07-05 09:01 JST: interval not yet elapsed → not due
        let within_interval = utc(2026, 7, 5, 0, 1, 0);
        assert!(
            !check_due(
                "lyre",
                &intention,
                within_interval,
                "Asia/Tokyo",
                Some(last_success)
            )
            .due
        );
    }

    #[test]
    fn interval_due_on_target_day_after_last_success() {
        // last_success = 2026-07-03 09:01 JST, interval=3 → target = 2026-07-06
        let intention = make_interval(3, "09:00");
        let last_success = utc(2026, 7, 3, 0, 1, 0); // 2026-07-03 09:01 JST

        // 2026-07-06 08:59 JST: target day but before at → not due
        let before_at = utc(2026, 7, 5, 23, 59, 0);
        assert!(
            !check_due(
                "lyre",
                &intention,
                before_at,
                "Asia/Tokyo",
                Some(last_success)
            )
            .due
        );

        // 2026-07-06 09:01 JST: due
        let on_target = utc(2026, 7, 6, 0, 1, 0);
        assert!(
            check_due(
                "lyre",
                &intention,
                on_target,
                "Asia/Tokyo",
                Some(last_success)
            )
            .due
        );
    }

    #[test]
    fn interval_remains_due_each_day_until_success() {
        // Failure recovery: last_success stays at 2026-07-03, so every day
        // from 2026-07-06 onward is due (one run per day via due_key).
        let intention = make_interval(3, "09:00");
        let last_success = utc(2026, 7, 3, 0, 1, 0);

        // 2026-07-06 09:01 JST (target day)
        assert!(
            check_due(
                "lyre",
                &intention,
                utc(2026, 7, 6, 0, 1, 0),
                "Asia/Tokyo",
                Some(last_success)
            )
            .due
        );
        // 2026-07-07 09:01 JST (still past target, last_success unchanged)
        assert!(
            check_due(
                "lyre",
                &intention,
                utc(2026, 7, 7, 0, 1, 0),
                "Asia/Tokyo",
                Some(last_success)
            )
            .due
        );
    }

    #[test]
    fn interval_due_advances_when_last_success_passes_target() {
        // A success on 2026-07-06 shifts the next target to 2026-07-09.
        let intention = make_interval(3, "09:00");
        let last_success = utc(2026, 7, 6, 0, 1, 0); // success on target day

        // 2026-07-08 09:01 JST: new interval not yet elapsed → not due
        assert!(
            !check_due(
                "lyre",
                &intention,
                utc(2026, 7, 8, 0, 1, 0),
                "Asia/Tokyo",
                Some(last_success)
            )
            .due
        );
        // 2026-07-09 09:01 JST: new target reached → due
        assert!(
            check_due(
                "lyre",
                &intention,
                utc(2026, 7, 9, 0, 1, 0),
                "Asia/Tokyo",
                Some(last_success)
            )
            .due
        );
    }

    #[test]
    fn due_key_interval_uses_local_evaluation_date() {
        let intention = make_interval(3, "09:00");
        let now = utc(2026, 7, 6, 0, 0, 0); // 2026-07-06 09:00 JST
        let key = generate_due_key("lyre", &intention, now, "Asia/Tokyo");
        assert_eq!(key, "lyre:test_intention:2026-07-06");

        // due_key advances each evaluation day, enabling a next-day retry
        // after a failed run within the same open interval window.
        let next_day = utc(2026, 7, 7, 0, 0, 0);
        let key = generate_due_key("lyre", &intention, next_day, "Asia/Tokyo");
        assert_eq!(key, "lyre:test_intention:2026-07-07");
    }

    #[test]
    fn due_resolver_handles_dst_gap_and_fold() {
        let gap_intention = make_daily("02:30");
        let now_after_gap = utc(2026, 3, 8, 7, 30, 0);
        let result = check_due(
            "test",
            &gap_intention,
            now_after_gap,
            "America/New_York",
            None,
        );
        assert!(
            !result.due,
            "intention at 02:30 should not be due during DST gap"
        );
        assert!(result.due_key.contains("2026-03-08"));

        let fold_intention = make_daily("01:30");
        let now_after_earlier = utc(2026, 11, 1, 5, 31, 0);
        let result = check_due(
            "test",
            &fold_intention,
            now_after_earlier,
            "America/New_York",
            None,
        );
        assert!(result.due);

        let now_before_earlier = utc(2026, 11, 1, 5, 29, 0);
        let result = check_due(
            "test",
            &fold_intention,
            now_before_earlier,
            "America/New_York",
            None,
        );
        assert!(!result.due);
    }

    // --- enabled field tests ---

    #[test]
    fn parse_enabled_defaults_to_true_when_omitted() {
        let content = "\
---
version: 1
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
---

body
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert_eq!(result.intentions.len(), 1);
        assert!(result.intentions[0].enabled);
    }

    #[test]
    fn parse_explicitly_disabled_intention() {
        let content = "\
---
version: 1
intentions:
  - id: morning_review
    enabled: false
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
---

body
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert_eq!(result.intentions.len(), 1);
        assert!(!result.intentions[0].enabled);
    }

    #[test]
    fn parse_mixed_enabled_and_disabled_intentions() {
        let content = "\
---
version: 1
intentions:
  - id: active_one
    schedule:
      kind: daily
      at: \"09:00\"
    attention: active
  - id: paused_one
    enabled: false
    schedule:
      kind: weekly
      day: sun
      at: \"21:00\"
    attention: paused
---

body
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert_eq!(result.intentions.len(), 2);
        assert!(result.intentions[0].enabled);
        assert!(!result.intentions[1].enabled);
        assert_eq!(result.intentions[0].id, "active_one");
        assert_eq!(result.intentions[1].id, "paused_one");
    }

    #[test]
    fn parse_explicitly_enabled_intention() {
        let content = "\
---
version: 1
intentions:
  - id: morning_review
    enabled: true
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
---

body
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert_eq!(result.intentions.len(), 1);
        assert!(result.intentions[0].enabled);
    }

    // --- delivery tests ---

    #[test]
    fn parse_intention_delivery() {
        let content = "\
---
version: 1
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
    delivery:
      channel: discord
      external_chat_id: \"123456789\"
---

body
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert_eq!(result.intentions.len(), 1);
        let delivery = result.intentions[0].delivery.as_ref().expect("delivery");
        assert_eq!(delivery.channel, "discord");
        assert_eq!(delivery.external_chat_id, "123456789");
    }

    #[test]
    fn parse_default_delivery() {
        let content = "\
---
version: 1
default_delivery:
  channel: telegram
  external_chat_id: \"987654321\"
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
---

body
";
        let result = parse_pulse_definition(content).expect("should parse");
        let dd = result.default_delivery.as_ref().expect("default_delivery");
        assert_eq!(dd.channel, "telegram");
        assert_eq!(dd.external_chat_id, "987654321");
        assert!(result.intentions[0].delivery.is_none());
    }

    #[test]
    fn parse_delivery_optional_on_intention() {
        let content = "\
---
version: 1
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
---

body
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert!(result.intentions[0].delivery.is_none());
    }

    #[test]
    fn parse_default_delivery_optional() {
        let content = valid_pulse_md();
        let result = parse_pulse_definition(&content).expect("should parse");
        assert!(result.default_delivery.is_none());
    }

    #[test]
    fn parse_rejects_invalid_channel() {
        let content = "\
---
version: 1
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
    delivery:
      channel: web
      external_chat_id: \"abc\"
---

body
";
        let err = parse_pulse_definition(content).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidDelivery { .. }),
            "expected InvalidDelivery, got: {err}"
        );
    }

    #[test]
    fn parse_rejects_empty_external_chat_id() {
        let content = "\
---
version: 1
default_delivery:
  channel: discord
  external_chat_id: \"\"
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
---

body
";
        let err = parse_pulse_definition(content).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidDelivery { .. }),
            "expected InvalidDelivery, got: {err}"
        );
    }

    #[test]
    fn parse_both_delivery_sources() {
        let content = "\
---
version: 1
default_delivery:
  channel: discord
  external_chat_id: \"111\"
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
    delivery:
      channel: telegram
      external_chat_id: \"222\"
  - id: evening_review
    schedule:
      kind: daily
      at: \"21:00\"
    attention: test
---

body
";
        let result = parse_pulse_definition(content).expect("should parse");
        let dd = result.default_delivery.as_ref().expect("default_delivery");
        assert_eq!(dd.channel, "discord");

        let first = &result.intentions[0];
        let d = first.delivery.as_ref().expect("intention delivery");
        assert_eq!(d.channel, "telegram");
        assert_eq!(d.external_chat_id, "222");

        assert!(result.intentions[1].delivery.is_none());
    }

    #[test]
    fn parse_delivery_without_front_matter_is_rejected() {
        let content = "# Just a heading\nSome text without front matter";
        let err = parse_pulse_definition(content).unwrap_err();
        assert!(
            matches!(err, PulseParseError::ParseFailed { .. }),
            "expected ParseFailed, got: {err}"
        );
    }
}

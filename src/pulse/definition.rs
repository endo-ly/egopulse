use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Explicit delivery destination for pulse notifications.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeliverySpec {
    pub channel: String,
    pub external_chat_id: String,
}

/// The full Pulse document for one agent (`agents/{agent_id}/PULSE.yaml`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PulseDefinition {
    /// Agent-level switch. When `false` the scheduler skips the agent entirely
    /// and the `pulse` tool only reports the disabled state.
    pub enabled: bool,
    pub default_delivery: Option<DeliverySpec>,
    pub intentions: Vec<TemporalIntention>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TemporalIntention {
    pub id: String,
    pub enabled: bool,
    pub schedule: TemporalSchedule,
    pub attention: String,
    pub delivery: Option<DeliverySpec>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
    /// Fire once at a fixed local `date` and `at`. A successful activation
    /// archives the intention; a failed or skipped run leaves it in place so
    /// the agent can inspect and repair it conversationally.
    Once {
        date: String,
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
    #[error("pulse_save_error: {agent_id}: {detail}")]
    SaveFailed { agent_id: String, detail: String },
}

// ===========================================================================
// YAML file format
// ===========================================================================

/// Delivery destination as written in / read from `PULSE.yaml`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeliveryRaw {
    channel: String,
    external_chat_id: String,
}

/// Schedule as written in / read from `PULSE.yaml`. Flat shape: `kind`
/// determines which of the optional fields must be present.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScheduleRaw {
    kind: String,
    #[serde(default)]
    at: String,
    #[serde(default)]
    day: Option<String>,
    #[serde(default)]
    interval_days: Option<u32>,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IntentionRaw {
    id: String,
    #[serde(default = "default_true")]
    enabled: bool,
    schedule: ScheduleRaw,
    attention: String,
    #[serde(default)]
    delivery: Option<DeliveryRaw>,
}

/// `PULSE.yaml` document shape. `enabled` defaults to `true` so a hand-written
/// minimal file only needs `intentions`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PulseDocumentRaw {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_delivery: Option<DeliveryRaw>,
    #[serde(default)]
    intentions: Vec<IntentionRaw>,
}

fn default_true() -> bool {
    true
}

pub(crate) const VALID_DELIVERY_CHANNELS: &[&str] = &["discord", "telegram"];
const VALID_WEEKDAYS: &[&str] = &["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

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

fn validate_date(
    agent_id: &str,
    intention_id: &str,
    date: &str,
) -> Result<NaiveDate, PulseParseError> {
    NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| PulseParseError::InvalidSchedule {
        agent_id: agent_id.to_string(),
        intention_id: intention_id.to_string(),
        detail: format!("invalid date format: {date}, expected YYYY-MM-DD"),
    })
}

fn validate_and_build_schedule(
    agent_id: &str,
    raw: &IntentionRaw,
) -> Result<TemporalSchedule, PulseParseError> {
    let intention_id = &raw.id;
    match raw.schedule.kind.as_str() {
        "daily" => {
            validate_hhmm(agent_id, intention_id, &raw.schedule.at)?;
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
            validate_weekday(agent_id, intention_id, &day)?;
            validate_hhmm(agent_id, intention_id, &raw.schedule.at)?;
            Ok(TemporalSchedule::Weekly {
                day,
                at: raw.schedule.at.clone(),
            })
        }
        "interval" => {
            let interval_days = raw.schedule.interval_days.unwrap_or(0);
            validate_interval_days(agent_id, intention_id, interval_days)?;
            validate_hhmm(agent_id, intention_id, &raw.schedule.at)?;
            Ok(TemporalSchedule::Interval {
                interval_days,
                at: raw.schedule.at.clone(),
            })
        }
        "once" => {
            let date = raw.schedule.date.as_deref().unwrap_or("").to_string();
            validate_date(agent_id, intention_id, &date)?;
            validate_hhmm(agent_id, intention_id, &raw.schedule.at)?;
            Ok(TemporalSchedule::Once {
                date,
                at: raw.schedule.at.clone(),
            })
        }
        other => Err(PulseParseError::InvalidSchedule {
            agent_id: agent_id.to_string(),
            intention_id: intention_id.to_string(),
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
            enabled: true,
            default_delivery: None,
            intentions: Vec::new(),
        });
    }

    let doc: PulseDocumentRaw =
        yaml_serde::from_str(trimmed).map_err(|e| PulseParseError::ParseFailed {
            agent_id: agent_id.to_string(),
            detail: e.to_string(),
        })?;

    let default_delivery = doc
        .default_delivery
        .as_ref()
        .map(|raw| validate_delivery_raw(agent_id, raw, "default_delivery"))
        .transpose()?;

    let mut intentions = Vec::with_capacity(doc.intentions.len());
    let mut seen_ids = HashSet::new();

    for raw in &doc.intentions {
        if seen_ids.contains(&raw.id) {
            return Err(PulseParseError::DuplicateIntentionId {
                agent_id: agent_id.to_string(),
                id: raw.id.clone(),
            });
        }
        seen_ids.insert(raw.id.clone());

        let schedule = validate_and_build_schedule(agent_id, raw)?;
        let delivery = raw
            .delivery
            .as_ref()
            .map(|d| validate_delivery_raw(agent_id, d, &format!("intention '{}'", raw.id)))
            .transpose()?;
        intentions.push(TemporalIntention {
            id: raw.id.clone(),
            enabled: raw.enabled,
            schedule,
            attention: raw.attention.clone(),
            delivery,
        });
    }

    Ok(PulseDefinition {
        enabled: doc.enabled,
        default_delivery,
        intentions,
    })
}

fn schedule_raw(schedule: &TemporalSchedule) -> ScheduleRaw {
    match schedule {
        TemporalSchedule::Daily { at } => ScheduleRaw {
            kind: "daily".to_string(),
            at: at.clone(),
            day: None,
            interval_days: None,
            date: None,
        },
        TemporalSchedule::Weekly { day, at } => ScheduleRaw {
            kind: "weekly".to_string(),
            at: at.clone(),
            day: Some(day.clone()),
            interval_days: None,
            date: None,
        },
        TemporalSchedule::Interval { interval_days, at } => ScheduleRaw {
            kind: "interval".to_string(),
            at: at.clone(),
            day: None,
            interval_days: Some(*interval_days),
            date: None,
        },
        TemporalSchedule::Once { date, at } => ScheduleRaw {
            kind: "once".to_string(),
            at: at.clone(),
            day: None,
            interval_days: None,
            date: Some(date.clone()),
        },
    }
}

fn intention_raw(intention: &TemporalIntention) -> IntentionRaw {
    IntentionRaw {
        id: intention.id.clone(),
        enabled: intention.enabled,
        schedule: schedule_raw(&intention.schedule),
        attention: intention.attention.clone(),
        delivery: intention.delivery.as_ref().map(|d| DeliveryRaw {
            channel: d.channel.clone(),
            external_chat_id: d.external_chat_id.clone(),
        }),
    }
}

fn document_raw(definition: &PulseDefinition) -> PulseDocumentRaw {
    PulseDocumentRaw {
        enabled: definition.enabled,
        default_delivery: definition.default_delivery.as_ref().map(|d| DeliveryRaw {
            channel: d.channel.clone(),
            external_chat_id: d.external_chat_id.clone(),
        }),
        intentions: definition.intentions.iter().map(intention_raw).collect(),
    }
}

/// Path of an agent's `PULSE.yaml`.
fn pulse_file_path(state_root: &Path, agent_id: &str) -> PathBuf {
    state_root.join("agents").join(agent_id).join("PULSE.yaml")
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
/// - `Once { date: "2026-05-11", at: "09:00" }` → `"once 2026-05-11 09:00"`
pub(crate) fn format_schedule(schedule: &TemporalSchedule) -> String {
    match schedule {
        TemporalSchedule::Daily { at } => format!("daily {at}"),
        TemporalSchedule::Weekly { day, at } => format!("weekly {day} {at}"),
        TemporalSchedule::Interval { interval_days, at } => {
            format!("every {interval_days} days {at}")
        }
        TemporalSchedule::Once { date, at } => format!("once {date} {at}"),
    }
}

/// Suggest a readable intention id derived from the schedule, e.g.
/// `daily-0900`, `weekly-sun-2100`, `every-3d-0900`, `once-20260511-0900`.
pub(crate) fn suggest_intention_id(schedule: &TemporalSchedule) -> String {
    let hhmm = |at: &str| at.replace(':', "");
    match schedule {
        TemporalSchedule::Daily { at } => format!("daily-{}", hhmm(at)),
        TemporalSchedule::Weekly { day, at } => format!("weekly-{day}-{}", hhmm(at)),
        TemporalSchedule::Interval { interval_days, at } => {
            format!("every-{interval_days}d-{}", hhmm(at))
        }
        TemporalSchedule::Once { date, at } => {
            format!("once-{}-{}", date.replace('-', ""), hhmm(at))
        }
    }
}

// ===========================================================================
// Coordination
// ===========================================================================

/// Process-wide gate serializing read-modify-write cycles on agent
/// `PULSE.yaml` files. The files are process-local resources, so one gate for
/// every writer (tool operations and the scheduler's archive step) is
/// sufficient. Hold the guard across load → mutate → save without awaiting.
static PULSE_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Wakes the scheduler after a successful save, so a `once` intention added
/// while the scheduler sleeps still fires at its exact time.
static PULSE_CHANGED: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();

/// Acquire the exclusive PULSE.yaml write gate.
pub(crate) fn pulse_file_lock() -> std::sync::MutexGuard<'static, ()> {
    PULSE_FILE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Notification source the scheduler listens to for definition changes.
pub(crate) fn pulse_changed() -> &'static tokio::sync::Notify {
    PULSE_CHANGED.get_or_init(tokio::sync::Notify::new)
}

// ===========================================================================
// Load / Save
// ===========================================================================

/// Load and parse an agent's `PULSE.yaml`. A missing file yields an empty,
/// enabled definition — Pulse is per-agent and simply unset until first use.
pub(crate) fn load_pulse_definition(
    state_root: &Path,
    agent_id: &str,
) -> Result<PulseDefinition, PulseParseError> {
    if !is_safe_agent_id(agent_id) {
        return Err(PulseParseError::UnsafeAgentId {
            agent_id: agent_id.to_string(),
        });
    }

    let path = pulse_file_path(state_root, agent_id);

    if !path.exists() {
        return Ok(PulseDefinition {
            enabled: true,
            default_delivery: None,
            intentions: Vec::new(),
        });
    }

    let content = std::fs::read_to_string(&path).map_err(|e| PulseParseError::ParseFailed {
        agent_id: agent_id.to_string(),
        detail: format!("failed to read PULSE.yaml: {e}"),
    })?;

    parse_pulse_definition_inner(&content, agent_id)
}

/// Serialize, validate (round-trip through the parser), and atomically write
/// an agent's `PULSE.yaml`. The round-trip guarantees a file written through
/// this function always parses back cleanly.
pub(crate) fn save_pulse_definition(
    state_root: &Path,
    agent_id: &str,
    definition: &PulseDefinition,
) -> Result<(), PulseParseError> {
    if !is_safe_agent_id(agent_id) {
        return Err(PulseParseError::UnsafeAgentId {
            agent_id: agent_id.to_string(),
        });
    }

    let yaml = yaml_serde::to_string(&document_raw(definition)).map_err(|e| {
        PulseParseError::SaveFailed {
            agent_id: agent_id.to_string(),
            detail: format!("failed to serialize PULSE.yaml: {e}"),
        }
    })?;

    // Validation round-trip: a file we write must parse back cleanly.
    parse_pulse_definition_inner(&yaml, agent_id)?;

    let path = pulse_file_path(state_root, agent_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| PulseParseError::SaveFailed {
            agent_id: agent_id.to_string(),
            detail: format!("failed to create agent directory: {e}"),
        })?;
    }

    // Atomic replace: write to a uniquely named sibling temp file, then rename
    // over the target. Unique names keep concurrent saves from clobbering each
    // other's temp file.
    let tmp = path.with_extension(format!("yaml.tmp.{}", uuid::Uuid::new_v4()));
    if let Err(e) = std::fs::write(&tmp, &yaml) {
        let _ = std::fs::remove_file(&tmp);
        return Err(PulseParseError::SaveFailed {
            agent_id: agent_id.to_string(),
            detail: format!("failed to write PULSE.yaml: {e}"),
        });
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(PulseParseError::SaveFailed {
            agent_id: agent_id.to_string(),
            detail: format!("failed to replace PULSE.yaml: {e}"),
        });
    }

    pulse_changed().notify_one();
    Ok(())
}

/// Remove one intention from the agent's `PULSE.yaml` (used to archive a fired
/// `once` intention). Only removes when the current file still holds the exact
/// intention that was executed, so an edit made during the activation (e.g. a
/// reschedule to tomorrow) is preserved. Callers must hold
/// [`pulse_file_lock`] across this call. Returns whether the intention was
/// found unchanged and removed.
pub(crate) fn archive_intention(
    state_root: &Path,
    agent_id: &str,
    expected: &TemporalIntention,
) -> Result<bool, PulseParseError> {
    let mut definition = load_pulse_definition(state_root, agent_id)?;
    let Some(current) = definition
        .intentions
        .iter()
        .find(|intention| intention.id == expected.id)
    else {
        return Ok(false);
    };
    if current != expected {
        return Ok(false);
    }
    definition
        .intentions
        .retain(|intention| intention.id != expected.id);
    save_pulse_definition(state_root, agent_id, &definition)?;
    Ok(true)
}

// ===========================================================================
// Due Resolver
// ===========================================================================

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
/// `timezone` is the IANA timezone for daily/weekly/interval/once evaluation (e.g. "Asia/Tokyo").
/// `last_success_at` is the most recent successful activation time for this
/// `(agent_id, intention_id)`. Only `interval` schedules consult it; the other
/// kinds ignore it.
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
        TemporalSchedule::Once { date, at } => is_once_due(date, at, now, timezone),
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
/// - once:     `{agent_id}:{intention_id}:{YYYY-MM-DD}` (the configured date;
///   stable across ticks, consumed by the first run regardless of status)
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
        TemporalSchedule::Once { date, .. } => {
            format!("{agent_id}:{}:{date}", intention.id)
        }
    }
}

/// Resolve a local calendar date + time in `timezone` to a UTC instant.
///
/// DST handling: a non-existent local time (gap) yields `None`; an ambiguous
/// local time (fold) resolves to the earlier occurrence.
fn local_datetime(tz: &Tz, date: chrono::NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    let naive_dt = date.and_time(time);
    match tz.from_local_datetime(&naive_dt) {
        LocalResult::None => None,
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(earliest, _latest) => Some(earliest.with_timezone(&Utc)),
    }
}

fn parse_timezone(timezone: &str) -> Option<Tz> {
    match timezone.parse::<Tz>() {
        Ok(tz) => Some(tz),
        Err(e) => {
            tracing::warn!("invalid timezone \"{timezone}\": {e}");
            None
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
    let Some(tz) = parse_timezone(timezone) else {
        return false;
    };

    let local_now = now.with_timezone(&tz);
    match local_datetime(&tz, local_now.date_naive(), time) {
        Some(target) => now >= target,
        None => {
            tracing::debug!("skipping daily intention: local time {at} falls in DST gap");
            false
        }
    }
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

    let Some(tz) = parse_timezone(timezone) else {
        return false;
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
    let Some(tz) = parse_timezone(timezone) else {
        return false;
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

/// Evaluate once schedule: due only on the configured local date, after `at`.
///
/// A past date is simply never due again; the gate's `due_key` (configured
/// date) is consumed by the first run regardless of its status.
fn is_once_due(date: &str, at: &str, now: DateTime<Utc>, timezone: &str) -> bool {
    let Some(target_date) = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok() else {
        tracing::warn!("invalid date format in once schedule: {date}");
        return false;
    };
    let Some(time) = parse_hhmm(at) else {
        tracing::warn!("invalid time format in once schedule: {at}");
        return false;
    };
    let Some(tz) = parse_timezone(timezone) else {
        return false;
    };

    let local_now = now.with_timezone(&tz);
    if local_now.date_naive() != target_date {
        return false;
    }

    matches!(local_datetime(&tz, target_date, time), Some(target) if now >= target)
}

/// Estimate the next due instant for an intention, used by the `pulse` tool's
/// `list` output. Pure calendar estimate for daily/weekly; DB-anchored (via
/// `last_success_at`) for interval; fixed for once. Returns `None` when no
/// future instant exists (e.g. a stale `once`).
pub(crate) fn estimate_next_due(
    intention: &TemporalIntention,
    now: DateTime<Utc>,
    timezone: &str,
    last_success_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let tz: Tz = timezone.parse().ok()?;
    let local_now = now.with_timezone(&tz);
    let today = local_now.date_naive();

    let at_time = |at: &str| parse_hhmm(at);

    match &intention.schedule {
        TemporalSchedule::Daily { at } => {
            let time = at_time(at)?;
            let candidate_date = if local_now.time() < time {
                today
            } else {
                today.succ_opt()?
            };
            local_datetime(&tz, candidate_date, time)
        }
        TemporalSchedule::Weekly { day, at } => {
            let target_weekday = parse_weekday(day)?;
            let time = at_time(at)?;
            let days_ahead = (target_weekday.num_days_from_monday() + 7
                - today.weekday().num_days_from_monday())
                % 7;
            let candidate_date = today + chrono::Duration::days(days_ahead as i64);
            let mut candidate = local_datetime(&tz, candidate_date, time)?;
            if candidate <= now {
                let next_date = candidate_date + chrono::Duration::days(7);
                candidate = local_datetime(&tz, next_date, time)?;
            }
            Some(candidate)
        }
        TemporalSchedule::Interval { interval_days, at } => {
            let time = at_time(at)?;
            let target_date = last_success_at.map(|s| {
                s.with_timezone(&tz).date_naive() + chrono::Duration::days(*interval_days as i64)
            });
            // The intention is due at the first `at` on or after the target
            // date (or today when the window is already open / first run).
            let candidate_date = target_date.map(|d| d.max(today)).unwrap_or(today);
            let candidate = local_datetime(&tz, candidate_date, time)?;
            if candidate <= now {
                // Window already open today: due on the next scan.
                Some(now)
            } else {
                Some(candidate)
            }
        }
        TemporalSchedule::Once { date, at } => {
            let target_date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
            let time = at_time(at)?;
            let candidate = local_datetime(&tz, target_date, time)?;
            (candidate > now).then_some(candidate)
        }
    }
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
    fn format_schedule_covers_all_schedule_kinds() {
        let cases = [
            (
                TemporalSchedule::Daily {
                    at: "08:00".to_string(),
                },
                "daily 08:00",
            ),
            (
                TemporalSchedule::Weekly {
                    day: "sun".to_string(),
                    at: "21:00".to_string(),
                },
                "weekly sun 21:00",
            ),
            (
                TemporalSchedule::Interval {
                    interval_days: 3,
                    at: "09:00".to_string(),
                },
                "every 3 days 09:00",
            ),
            (
                TemporalSchedule::Once {
                    date: "2026-05-11".to_string(),
                    at: "09:00".to_string(),
                },
                "once 2026-05-11 09:00",
            ),
        ];

        for (schedule, expected) in cases {
            assert_eq!(format_schedule(&schedule), expected);
        }
    }

    #[test]
    fn suggest_intention_id_derives_from_schedule() {
        let cases = [
            (
                TemporalSchedule::Daily {
                    at: "09:00".to_string(),
                },
                "daily-0900",
            ),
            (
                TemporalSchedule::Weekly {
                    day: "sun".to_string(),
                    at: "21:00".to_string(),
                },
                "weekly-sun-2100",
            ),
            (
                TemporalSchedule::Interval {
                    interval_days: 3,
                    at: "09:00".to_string(),
                },
                "every-3d-0900",
            ),
            (
                TemporalSchedule::Once {
                    date: "2026-05-11".to_string(),
                    at: "09:00".to_string(),
                },
                "once-20260511-0900",
            ),
        ];

        for (schedule, expected) in cases {
            assert_eq!(suggest_intention_id(&schedule), expected);
        }
    }

    fn valid_pulse_yaml() -> String {
        "\
enabled: true
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
"
        .to_string()
    }

    #[test]
    fn parse_pulse_definition_loads_document() {
        let content = valid_pulse_yaml();

        let result = parse_pulse_definition(&content).expect("should parse successfully");

        assert!(result.enabled);
        assert_eq!(result.intentions.len(), 2);
        assert_eq!(result.intentions[0].id, "morning_review");
    }

    #[test]
    fn parse_daily_and_weekly_intentions() {
        let content = valid_pulse_yaml();

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
    fn parse_once_intention() {
        let content = "\
intentions:
  - id: dentist_reminder
    schedule:
      kind: once
      date: \"2026-05-12\"
      at: \"18:00\"
    attention: Leave for the dentist.
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert_eq!(result.intentions.len(), 1);
        assert!(matches!(
            &result.intentions[0].schedule,
            TemporalSchedule::Once { date, at } if date == "2026-05-12" && at == "18:00"
        ));
    }

    #[test]
    fn parse_rejects_once_with_missing_or_invalid_date() {
        let missing_date = "\
intentions:
  - id: event_check
    schedule:
      kind: once
      at: \"18:00\"
    attention: test
";
        let err = parse_pulse_definition(missing_date).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidSchedule { ref intention_id, .. } if intention_id == "event_check"),
            "expected InvalidSchedule for missing once date, got: {err}"
        );

        let invalid_date = "\
intentions:
  - id: event_check
    schedule:
      kind: once
      date: \"2026/05/12\"
      at: \"18:00\"
    attention: test
";
        let err = parse_pulse_definition(invalid_date).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidSchedule { ref intention_id, .. } if intention_id == "event_check"),
            "expected InvalidSchedule for invalid once date, got: {err}"
        );
    }

    #[test]
    fn parse_interval_intention() {
        let content = "\
intentions:
  - id: periodic_report
    schedule:
      kind: interval
      interval_days: 3
      at: \"09:00\"
    attention: test
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
intentions:
  - id: periodic_report
    schedule:
      kind: interval
      interval_days: 0
      at: \"09:00\"
    attention: test
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
intentions:
  - id: periodic_report
    schedule:
      kind: interval
      at: \"09:00\"
    attention: test
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
intentions:
  - id: bad_time
    schedule:
      kind: daily
      at: \"25:00\"
    attention: test
";
        let err = parse_pulse_definition(invalid_time).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidSchedule { ref intention_id, .. } if intention_id == "bad_time"),
            "expected InvalidSchedule for bad time, got: {err}"
        );

        let invalid_weekday = "\
intentions:
  - id: bad_day
    schedule:
      kind: weekly
      day: xyz
      at: \"09:00\"
    attention: test
";
        let err = parse_pulse_definition(invalid_weekday).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidSchedule { ref intention_id, .. } if intention_id == "bad_day"),
            "expected InvalidSchedule for bad weekday, got: {err}"
        );
    }

    #[test]
    fn parse_agent_enabled_defaults_true_and_overrides_false() {
        let content = "\
enabled: false
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert!(
            !result.enabled,
            "agent-level enabled: false should be parsed"
        );

        let default_doc = parse_pulse_definition("intentions: []").expect("should parse");
        assert!(
            default_doc.enabled,
            "agent-level enabled should default to true"
        );
    }

    #[test]
    fn parse_intention_enabled_defaults_and_overrides() {
        let content = "\
intentions:
  - id: default_enabled
    schedule:
      kind: daily
      at: \"09:00\"
    attention: active
  - id: explicitly_disabled
    enabled: false
    schedule:
      kind: weekly
      day: sun
      at: \"21:00\"
    attention: paused
  - id: explicitly_enabled
    enabled: true
    schedule:
      kind: daily
      at: \"10:00\"
    attention: active
";
        let result = parse_pulse_definition(content).expect("should parse");
        assert_eq!(result.intentions.len(), 3);
        assert_eq!(result.intentions[0].id, "default_enabled");
        assert_eq!(result.intentions[1].id, "explicitly_disabled");
        assert_eq!(result.intentions[2].id, "explicitly_enabled");
        assert!(result.intentions[0].enabled);
        assert!(!result.intentions[1].enabled);
        assert!(result.intentions[2].enabled);
    }

    #[test]
    fn parse_rejects_invalid_channel() {
        let content = "\
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
    delivery:
      channel: web
      external_chat_id: \"abc\"
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
default_delivery:
  channel: discord
  external_chat_id: \"\"
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"09:00\"
    attention: test
";
        let err = parse_pulse_definition(content).unwrap_err();
        assert!(
            matches!(err, PulseParseError::InvalidDelivery { .. }),
            "expected InvalidDelivery, got: {err}"
        );
    }

    #[test]
    fn parse_delivery_sources_and_optional_values() {
        let content = "\
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
";
        let result = parse_pulse_definition(content).expect("should parse");
        let dd = result.default_delivery.as_ref().expect("default_delivery");
        assert_eq!(dd.channel, "discord");

        let first = &result.intentions[0];
        let d = first.delivery.as_ref().expect("intention delivery");
        assert_eq!(d.channel, "telegram");
        assert_eq!(d.external_chat_id, "222");

        assert!(result.intentions[1].delivery.is_none());

        let without_delivery = parse_pulse_definition(&valid_pulse_yaml()).expect("parse");
        assert!(without_delivery.default_delivery.is_none());
        assert!(without_delivery.intentions[0].delivery.is_none());
    }

    #[test]
    fn parse_non_yaml_content_is_rejected() {
        let result = parse_pulse_definition("# Just a heading\nSome plain text");
        assert!(
            result.is_err(),
            "plain non-YAML text should be rejected as ParseFailed"
        );
    }

    #[test]
    fn parse_empty_content_yields_default_document() {
        let result = parse_pulse_definition("").expect("should parse");
        assert!(result.enabled);
        assert!(result.intentions.is_empty());
    }

    #[test]
    fn load_missing_pulse_definition_returns_empty() {
        let dir = tempfile::tempdir().unwrap();

        let result = load_pulse_definition(dir.path(), "nonexistent").unwrap();

        assert!(result.enabled);
        assert!(result.intentions.is_empty());
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

    // --- save / load roundtrip tests ---

    fn sample_document() -> PulseDefinition {
        PulseDefinition {
            enabled: false,
            default_delivery: Some(DeliverySpec {
                channel: "discord".to_string(),
                external_chat_id: "111".to_string(),
            }),
            intentions: vec![
                TemporalIntention {
                    id: "morning_review".to_string(),
                    enabled: true,
                    schedule: TemporalSchedule::Daily {
                        at: "09:00".to_string(),
                    },
                    attention: "Check the day.\nCarry over unresolved items.\n".to_string(),
                    delivery: None,
                },
                TemporalIntention {
                    id: "dentist_reminder".to_string(),
                    enabled: true,
                    schedule: TemporalSchedule::Once {
                        date: "2026-05-12".to_string(),
                        at: "18:00".to_string(),
                    },
                    attention: "Leave for the dentist.".to_string(),
                    delivery: Some(DeliverySpec {
                        channel: "telegram".to_string(),
                        external_chat_id: "222".to_string(),
                    }),
                },
            ],
        }
    }

    #[test]
    fn save_then_load_roundtrips_document() {
        let dir = tempfile::tempdir().unwrap();
        let document = sample_document();

        save_pulse_definition(dir.path(), "lyre", &document).expect("save");

        let path = dir.path().join("agents").join("lyre").join("PULSE.yaml");
        assert!(path.exists(), "PULSE.yaml should be written");
        assert!(
            !dir.path()
                .join("agents")
                .join("lyre")
                .join("PULSE.yaml.tmp")
                .exists(),
            "temp file should be renamed away"
        );

        let loaded = load_pulse_definition(dir.path(), "lyre").expect("load");
        assert_eq!(loaded.enabled, document.enabled);
        assert_eq!(
            loaded.intentions[0].attention, document.intentions[0].attention,
            "multi-line attention must roundtrip"
        );
        assert_eq!(
            loaded.default_delivery, document.default_delivery,
            "default_delivery should roundtrip"
        );
        assert_eq!(loaded.intentions.len(), document.intentions.len());
        assert_eq!(loaded.intentions[0].id, "morning_review");
        assert!(matches!(
            &loaded.intentions[1].schedule,
            TemporalSchedule::Once { date, at } if date == "2026-05-12" && at == "18:00"
        ));
        assert_eq!(
            loaded.intentions[1].delivery, document.intentions[1].delivery,
            "intention delivery should roundtrip"
        );
    }

    #[test]
    fn save_rejects_unsafe_agent_id() {
        let dir = tempfile::tempdir().unwrap();
        let err = save_pulse_definition(dir.path(), "../escape", &sample_document()).unwrap_err();
        assert!(
            matches!(err, PulseParseError::UnsafeAgentId { .. }),
            "expected UnsafeAgentId, got: {err}"
        );
    }

    #[test]
    fn archive_intention_removes_only_target_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        save_pulse_definition(dir.path(), "lyre", &sample_document()).expect("save");
        let expected = sample_document().intentions[1].clone();

        let archived = archive_intention(dir.path(), "lyre", &expected).expect("archive");

        assert!(archived, "once intention should be archived");
        let loaded = load_pulse_definition(dir.path(), "lyre").expect("load");
        assert_eq!(loaded.intentions.len(), 1);
        assert_eq!(loaded.intentions[0].id, "morning_review");
    }

    #[test]
    fn archive_intention_returns_false_for_unknown_id() {
        let dir = tempfile::tempdir().unwrap();
        save_pulse_definition(dir.path(), "lyre", &sample_document()).expect("save");
        let expected = TemporalIntention {
            id: "nonexistent".to_string(),
            enabled: true,
            schedule: TemporalSchedule::Once {
                date: "2026-05-12".to_string(),
                at: "18:00".to_string(),
            },
            attention: "ghost".to_string(),
            delivery: None,
        };

        let archived = archive_intention(dir.path(), "lyre", &expected).expect("archive");

        assert!(!archived);
        let loaded = load_pulse_definition(dir.path(), "lyre").expect("load");
        assert_eq!(loaded.intentions.len(), 2, "nothing should be removed");
    }

    #[test]
    fn archive_intention_keeps_intention_edited_during_the_run() {
        // Arrange: the executed once intention is rescheduled to the next day
        // while the activation runs.
        let dir = tempfile::tempdir().unwrap();
        save_pulse_definition(dir.path(), "lyre", &sample_document()).expect("save");
        let executed = sample_document().intentions[1].clone();
        let mut current = sample_document();
        current.intentions[1].schedule = TemporalSchedule::Once {
            date: "2026-05-13".to_string(),
            at: "10:00".to_string(),
        };
        save_pulse_definition(dir.path(), "lyre", &current).expect("save edit");

        // Act
        let archived = archive_intention(dir.path(), "lyre", &executed).expect("archive");

        // Assert: the newer definition survives the stale activation
        assert!(!archived, "an edited intention must not be archived");
        let loaded = load_pulse_definition(dir.path(), "lyre").expect("load");
        assert_eq!(loaded.intentions.len(), 2);
        assert_eq!(
            loaded.intentions[1].schedule,
            TemporalSchedule::Once {
                date: "2026-05-13".to_string(),
                at: "10:00".to_string(),
            },
            "the rescheduled intention must survive"
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

    fn make_once(date: &str, at: &str) -> TemporalIntention {
        TemporalIntention {
            id: "test_intention".to_string(),
            enabled: true,
            schedule: TemporalSchedule::Once {
                date: date.to_string(),
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

    // --- once schedule tests ---

    #[test]
    fn once_due_only_on_target_date_after_at() {
        let intention = make_once("2026-05-12", "18:00");

        // 2026-05-12 17:59 JST: before at → not due
        assert!(
            !check_due(
                "lyre",
                &intention,
                utc(2026, 5, 12, 8, 59, 0),
                "Asia/Tokyo",
                None
            )
            .due
        );
        // 2026-05-12 18:01 JST: due
        assert!(
            check_due(
                "lyre",
                &intention,
                utc(2026, 5, 12, 9, 1, 0),
                "Asia/Tokyo",
                None
            )
            .due
        );
        // 2026-05-13 18:01 JST: past the target date → never due again
        assert!(
            !check_due(
                "lyre",
                &intention,
                utc(2026, 5, 13, 9, 1, 0),
                "Asia/Tokyo",
                None
            )
            .due
        );
    }

    #[test]
    fn due_key_once_uses_configured_date() {
        let intention = make_once("2026-05-12", "18:00");
        let key = generate_due_key("lyre", &intention, utc(2026, 5, 12, 9, 0, 0), "Asia/Tokyo");
        assert_eq!(key, "lyre:test_intention:2026-05-12");

        // due_key stays on the configured date regardless of the evaluation day,
        // so a failed run on the target day consumes the date (no silent retry).
        let next_day =
            generate_due_key("lyre", &intention, utc(2026, 5, 13, 9, 0, 0), "Asia/Tokyo");
        assert_eq!(next_day, "lyre:test_intention:2026-05-12");
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

    // --- next due estimate tests ---

    #[test]
    fn estimate_next_due_daily_same_or_next_day() {
        // 2026-05-10 08:59 JST → today 09:00 JST
        let intention = make_daily("09:00");
        let before = estimate_next_due(&intention, utc(2026, 5, 9, 23, 59, 0), "Asia/Tokyo", None);
        assert_eq!(before, Some(utc(2026, 5, 10, 0, 0, 0)));

        // 2026-05-10 09:01 JST → tomorrow 09:00 JST
        let after = estimate_next_due(&intention, utc(2026, 5, 10, 0, 1, 0), "Asia/Tokyo", None);
        assert_eq!(after, Some(utc(2026, 5, 11, 0, 0, 0)));
    }

    #[test]
    fn estimate_next_due_weekly_rolls_to_next_week_when_time_passed() {
        // 2026-05-10 is a Sunday. 21:00 JST on Sunday → today 21:00 JST
        let intention = make_weekly("sun", "21:00");
        let before = estimate_next_due(&intention, utc(2026, 5, 10, 5, 0, 0), "Asia/Tokyo", None);
        assert_eq!(before, Some(utc(2026, 5, 10, 12, 0, 0)));

        // After Sunday 21:00 JST → next Sunday 21:00 JST (2026-05-17)
        let after = estimate_next_due(&intention, utc(2026, 5, 10, 12, 1, 0), "Asia/Tokyo", None);
        assert_eq!(after, Some(utc(2026, 5, 17, 12, 0, 0)));
    }

    #[test]
    fn estimate_next_due_interval_uses_last_success_anchor() {
        // last_success 2026-07-03, interval 3 → target 2026-07-06, at 09:00 JST
        let intention = make_interval(3, "09:00");
        let last_success = utc(2026, 7, 3, 0, 1, 0);

        // Asked on 2026-07-04 10:00 JST → 2026-07-06 09:00 JST
        let during = estimate_next_due(
            &intention,
            utc(2026, 7, 4, 1, 0, 0),
            "Asia/Tokyo",
            Some(last_success),
        );
        assert_eq!(during, Some(utc(2026, 7, 6, 0, 0, 0)));

        // Asked after the window opened → due now
        let overdue = estimate_next_due(
            &intention,
            utc(2026, 7, 6, 0, 1, 0),
            "Asia/Tokyo",
            Some(last_success),
        );
        assert_eq!(overdue, Some(utc(2026, 7, 6, 0, 1, 0)));
    }

    #[test]
    fn estimate_next_due_once_future_only() {
        let intention = make_once("2026-05-12", "18:00");

        let future = estimate_next_due(&intention, utc(2026, 5, 10, 0, 0, 0), "Asia/Tokyo", None);
        assert_eq!(future, Some(utc(2026, 5, 12, 9, 0, 0)));

        let past = estimate_next_due(&intention, utc(2026, 5, 13, 0, 0, 0), "Asia/Tokyo", None);
        assert_eq!(past, None, "stale once intention has no next due");
    }

    #[test]
    fn estimate_next_due_ignores_disabled_intention_flag() {
        // The estimate is schedule-only; enablement is handled by callers.
        let mut intention = make_daily("09:00");
        intention.enabled = false;
        let estimate =
            estimate_next_due(&intention, utc(2026, 5, 9, 23, 59, 0), "Asia/Tokyo", None);
        assert_eq!(estimate, Some(utc(2026, 5, 10, 0, 0, 0)));
    }

    #[test]
    fn local_datetime_handles_midnight_boundary() {
        use chrono::Timelike;
        // Sanity: hour/min/sec of resolved time must match the requested HH:MM.
        let tz: Tz = "Asia/Tokyo".parse().expect("tz");
        let date = NaiveDate::from_ymd_opt(2026, 5, 10).expect("date");
        let time = parse_hhmm("23:59").expect("time");
        let resolved = local_datetime(&tz, date, time).expect("resolved");
        let local = resolved.with_timezone(&tz);
        assert_eq!(local.hour(), 23);
        assert_eq!(local.minute(), 59);
    }
}

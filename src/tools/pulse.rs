//! `pulse` tool — lets an agent manage its own Pulse configuration.
//!
//! All writes go through this tool: it loads `agents/{agent_id}/PULSE.yaml`,
//! mutates the structured [`crate::pulse::definition::PulseDefinition`], and
//! saves it with a parse round-trip and an atomic rename, so the agent can
//! never corrupt the file by a malformed write. The tool description below is
//! the agent-facing knowledge about Pulse; keep it in sync with
//! `docs/pulse.md`.

use std::sync::Arc;

use serde_json::json;

use crate::config::ConfigManager;
use crate::conversation::ConversationScope;
use crate::pulse::definition::{
    self, DeliverySpec, PulseDefinition, TemporalIntention, TemporalSchedule, check_due,
    estimate_next_due, format_schedule, generate_due_key, load_pulse_definition,
    save_pulse_definition, suggest_intention_id,
};
use crate::storage::Database;
use crate::tools::{Tool, ToolErrorKind, ToolExecutionContext, ToolResult};

const TOOL_DESCRIPTION: &str = "\
Manage this agent's Pulse: scheduled moments of attention that briefly activate you and, \
when something is worth reporting, deliver a short message to a chat. Use it whenever the \
user asks for recurring check-ins or reminders (\"every day at 9\", \"remind me tomorrow \
at 10\", \"weekly on Sunday evening\").

Pulse is attention direction, not a cron job: `attention` says what to look at, not what to \
execute. At the scheduled time you are briefly activated with the attention text plus \
recent conversation context; if there is nothing worth reporting you stay silent, \
otherwise your message is delivered and the user can reply to it.

Rules: `delivery` defaults to the current conversation (Discord/Telegram only; elsewhere \
pass it explicitly). A `once` intention must be in the future, fires at the specified \
time, and is removed automatically after a successful run. Edits take effect on the next scan; \
changing a schedule does not re-fire an occurrence that already ran today. Check `list` \
before adding and when investigating why a pulse did or did not fire. Do not edit \
PULSE.yaml directly; use this tool.";

pub(crate) struct PulseTool {
    config_manager: Arc<ConfigManager>,
    db: Arc<Database>,
}

impl PulseTool {
    pub(crate) fn new(config_manager: Arc<ConfigManager>, db: Arc<Database>) -> Self {
        Self { config_manager, db }
    }

    /// Explain when this intention will next fire, using the same two-stage
    /// judgment as the scheduler: the calendar estimate
    /// ([`estimate_next_due`]) combined with the run history the gate
    /// ([`Database::has_pulse_due_run`]) enforces. This keeps `list` and
    /// `add` from promising a firing the scheduler will suppress, and from
    /// missing one it will perform.
    ///
    /// # Errors
    /// Returns `StorageError` when run-history lookups fail.
    async fn next_due_text(
        &self,
        agent_id: &str,
        intention: &TemporalIntention,
        timezone: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, crate::error::StorageError> {
        let last_success_at = if matches!(intention.schedule, TemporalSchedule::Interval { .. }) {
            let agent_id = agent_id.to_string();
            let intention_id = intention.id.clone();
            let db = Arc::clone(&self.db);
            crate::storage::call_blocking(db, move |db| {
                db.get_last_success_started_at(&agent_id, &intention_id)
            })
            .await?
        } else {
            None
        };

        let current_key = generate_due_key(agent_id, intention, now, timezone);
        let consumed = {
            let agent_id = agent_id.to_string();
            let intention_id = intention.id.clone();
            let key = current_key.clone();
            let db = Arc::clone(&self.db);
            crate::storage::call_blocking(db, move |db| {
                db.has_pulse_due_run(&agent_id, &intention_id, &key)
            })
            .await?
        };

        if check_due(agent_id, intention, now, timezone, last_success_at).due && !consumed {
            return Ok("due now (pending next scan)".to_string());
        }
        if consumed && matches!(intention.schedule, TemporalSchedule::Once { .. }) {
            return Ok("already attempted (no automatic retry)".to_string());
        }

        match estimate_next_due(intention, now, timezone, last_success_at) {
            None => Ok("no upcoming due".to_string()),
            Some(candidate) => {
                let candidate_key = generate_due_key(agent_id, intention, candidate, timezone);
                if consumed && candidate_key == current_key {
                    Ok("already attempted for current period".to_string())
                } else if candidate <= now {
                    Ok("due now (pending next scan)".to_string())
                } else {
                    Ok(format!(
                        "next due {} ({timezone})",
                        format_local(candidate, timezone)
                    ))
                }
            }
        }
    }

    async fn op_list(&self, state_root: &str, agent_id: &str, timezone: &str) -> ToolResult {
        let document = match load_document(state_root, agent_id) {
            Ok(document) => document,
            Err(result) => return result,
        };

        let now = chrono::Utc::now();
        let mut lines = Vec::new();
        lines.push(format!(
            "Pulse for agent '{agent_id}': {}",
            if document.enabled {
                "enabled".to_string()
            } else {
                "disabled (no intention will fire until enabled)".to_string()
            }
        ));

        if document.intentions.is_empty() {
            lines.push("No intentions configured.".to_string());
        }

        for intention in &document.intentions {
            let state = if intention.enabled {
                "enabled"
            } else {
                "disabled"
            };
            let due_text = if intention.enabled {
                match self.next_due_text(agent_id, intention, timezone, now).await {
                    Ok(text) => format!(" | {text}"),
                    Err(e) => format!(" | error: run history lookup failed ({e})"),
                }
            } else {
                String::new()
            };
            lines.push(format!(
                "- {} [{}] {}{}",
                intention.id,
                state,
                format_schedule(&intention.schedule),
                due_text
            ));
            lines.push(format!("  attention: {}", intention.attention.trim()));
            if let Some(delivery) = intention
                .delivery
                .as_ref()
                .or(document.default_delivery.as_ref())
            {
                lines.push(format!(
                    "  delivery: {}:{}",
                    delivery.channel, delivery.external_chat_id
                ));
            }
        }

        ToolResult::success(lines.join("\n"))
    }

    async fn op_add(
        &self,
        input: &serde_json::Value,
        context: &ToolExecutionContext,
        state_root: &str,
        agent_id: &str,
        timezone: &str,
    ) -> ToolResult {
        let attention = match required_text(input, "attention") {
            Ok(attention) => attention,
            Err(result) => return result,
        };
        let delivery = match input.get("delivery") {
            Some(value) => match parse_explicit_delivery(value) {
                Ok(delivery) => delivery,
                Err(result) => return result,
            },
            None => match default_delivery_from_context(context) {
                Ok(delivery) => delivery,
                Err(result) => return result,
            },
        };
        let schedule = match parse_schedule_input(input) {
            Ok(schedule) => schedule,
            Err(result) => return result,
        };
        if let Err(result) = ensure_once_is_future(&schedule, chrono::Utc::now(), timezone) {
            return result;
        }

        let explicit_id = optional_text(input, "id");

        // Load → mutate → save atomically under the process-wide write gate so
        // concurrent operations cannot lose each other's intentions.
        let (id, intention) = {
            let _guard = definition::pulse_file_lock();
            let mut document = match load_document(state_root, agent_id) {
                Ok(document) => document,
                Err(result) => return result,
            };
            let id = match explicit_id {
                Some(id) => {
                    if document
                        .intentions
                        .iter()
                        .any(|intention| intention.id == id)
                    {
                        return ToolResult::error_with_kind(
                            ToolErrorKind::InvalidInput,
                            format!(
                                "pulse: intention id '{id}' already exists. Use op=update to \
                                 change it, or pick another id. Current ids: {}",
                                ids_line(&document)
                            ),
                        );
                    }
                    id
                }
                None => unique_suggested_id(&document, &schedule),
            };

            let intention = TemporalIntention {
                id: id.clone(),
                enabled: true,
                schedule: schedule.clone(),
                attention,
                delivery: Some(delivery),
            };
            document.intentions.push(intention.clone());
            if let Err(result) = save_document(state_root, agent_id, &document) {
                return result;
            }
            (id, intention)
        };

        let due_text = match self
            .next_due_text(agent_id, &intention, timezone, chrono::Utc::now())
            .await
        {
            Ok(text) => format!("{text}."),
            Err(e) => format!("run history lookup failed ({e}), so the next due is unknown."),
        };

        ToolResult::success(format!(
            "Added intention '{id}' ({}). {due_text} It takes effect on the next scan.",
            format_schedule(&schedule)
        ))
    }

    async fn op_update(
        &self,
        input: &serde_json::Value,
        state_root: &str,
        agent_id: &str,
        timezone: &str,
    ) -> ToolResult {
        let _guard = definition::pulse_file_lock();
        let mut document = match load_document(state_root, agent_id) {
            Ok(document) => document,
            Err(result) => return result,
        };
        let id = match require_id(input) {
            Ok(id) => id,
            Err(result) => return result,
        };
        let Some(intention) = document
            .intentions
            .iter_mut()
            .find(|intention| intention.id == id)
        else {
            return ToolResult::error_with_kind(
                ToolErrorKind::NotFound,
                format!(
                    "pulse: no intention '{id}'. Current ids: {}",
                    ids_line(&document)
                ),
            );
        };

        let mut changed = Vec::new();
        if input.get("schedule").is_some() {
            let schedule = match parse_schedule_input(input) {
                Ok(schedule) => schedule,
                Err(result) => return result,
            };
            if let Err(result) = ensure_once_is_future(&schedule, chrono::Utc::now(), timezone) {
                return result;
            }
            changed.push(format_schedule(&schedule));
            intention.schedule = schedule;
        }
        if let Some(attention) = input.get("attention").and_then(|v| v.as_str()) {
            let trimmed = attention.trim();
            if trimmed.is_empty() {
                return ToolResult::error_with_kind(
                    ToolErrorKind::InvalidInput,
                    "pulse: attention must not be empty".to_string(),
                );
            }
            intention.attention = trimmed.to_string();
            changed.push("attention".to_string());
        }
        if let Some(delivery_value) = input.get("delivery") {
            let delivery = match parse_explicit_delivery(delivery_value) {
                Ok(delivery) => delivery,
                Err(result) => return result,
            };
            let text = format!("{}:{}", delivery.channel, delivery.external_chat_id);
            intention.delivery = Some(delivery);
            changed.push(format!("delivery {text}"));
        }

        let unchanged_summary = document
            .intentions
            .iter()
            .find(|intention| intention.id == id)
            .map(|intention| format_schedule(&intention.schedule))
            .unwrap_or_default();
        if let Err(result) = save_document(state_root, agent_id, &document) {
            return result;
        }

        if changed.is_empty() {
            ToolResult::success(format!(
                "Intention '{id}' unchanged (nothing to update). Schedule: {unchanged_summary}"
            ))
        } else {
            ToolResult::success(format!(
                "Updated intention '{id}' ({}).",
                changed.join(", ")
            ))
        }
    }

    async fn op_remove(
        &self,
        input: &serde_json::Value,
        state_root: &str,
        agent_id: &str,
    ) -> ToolResult {
        let _guard = definition::pulse_file_lock();
        let mut document = match load_document(state_root, agent_id) {
            Ok(document) => document,
            Err(result) => return result,
        };
        let id = match require_id(input) {
            Ok(id) => id,
            Err(result) => return result,
        };
        let before = document.intentions.len();
        document.intentions.retain(|intention| intention.id != id);
        if document.intentions.len() == before {
            return ToolResult::error_with_kind(
                ToolErrorKind::NotFound,
                format!(
                    "pulse: no intention '{id}'. Current ids: {}",
                    ids_line(&document)
                ),
            );
        }
        if let Err(result) = save_document(state_root, agent_id, &document) {
            return result;
        }
        ToolResult::success(format!(
            "Removed intention '{id}'. {} intention(s) remain.",
            document.intentions.len()
        ))
    }

    async fn op_set_enabled(
        &self,
        input: &serde_json::Value,
        enable: bool,
        state_root: &str,
        agent_id: &str,
    ) -> ToolResult {
        let _guard = definition::pulse_file_lock();
        let mut document = match load_document(state_root, agent_id) {
            Ok(document) => document,
            Err(result) => return result,
        };

        let action = if enable { "enabled" } else { "disabled" };
        match optional_text(input, "id") {
            Some(id) => {
                let Some(intention) = document
                    .intentions
                    .iter_mut()
                    .find(|intention| intention.id == id)
                else {
                    return ToolResult::error_with_kind(
                        ToolErrorKind::NotFound,
                        format!(
                            "pulse: no intention '{id}'. Current ids: {}",
                            ids_line(&document)
                        ),
                    );
                };
                intention.enabled = enable;
                if let Err(result) = save_document(state_root, agent_id, &document) {
                    return result;
                }
                ToolResult::success(format!("Intention '{id}' {action}."))
            }
            None => {
                document.enabled = enable;
                if let Err(result) = save_document(state_root, agent_id, &document) {
                    return result;
                }
                ToolResult::success(format!("Pulse {action} for agent '{agent_id}'."))
            }
        }
    }
}

#[async_trait::async_trait]
impl Tool for PulseTool {
    fn name(&self) -> &str {
        "pulse"
    }

    fn definition(&self) -> crate::llm::ToolDefinition {
        crate::llm::ToolDefinition {
            name: "pulse".to_string(),
            description: TOOL_DESCRIPTION.to_string(),
            parameters: crate::tools::schema_object(
                json!({
                    "op": {
                        "type": "string",
                        "enum": ["list", "add", "update", "remove", "enable", "disable"],
                        "description": "Operation to perform"
                    },
                    "id": {
                        "type": "string",
                        "description": "Intention id. Required by update/remove; optional for enable/disable (omit it to toggle Pulse itself for this agent)"
                    },
                    "schedule": {
                        "type": "object",
                        "description": "Schedule definition (add/update). kind determines the other fields: daily needs at; weekly needs day + at; interval needs interval_days + at; once needs date + at",
                        "properties": {
                            "kind": {
                                "type": "string",
                                "enum": ["daily", "weekly", "interval", "once"],
                                "description": "Schedule kind"
                            },
                            "at": {
                                "type": "string",
                                "description": "Local time of day, HH:MM (e.g. \"09:00\")"
                            },
                            "day": {
                                "type": "string",
                                "enum": ["mon", "tue", "wed", "thu", "fri", "sat", "sun"],
                                "description": "Day of week (weekly only)"
                            },
                            "interval_days": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Days to wait after the last successful run: 1 = every day, 2 = every other day (interval only)"
                            },
                            "date": {
                                "type": "string",
                                "description": "Target date in YYYY-MM-DD (once only)"
                            }
                        }
                    },
                    "attention": {
                        "type": "string",
                        "description": "What to pay attention to at the scheduled time (add/update). Direction of attention, not an instruction to execute a task (e.g. \"check today's calendar and carryover items\")"
                    },
                    "delivery": {
                        "type": "object",
                        "description": "Where the notification is delivered (add/update). Defaults to the current conversation when it is on Discord/Telegram",
                        "properties": {
                            "channel": {
                                "type": "string",
                                "enum": ["discord", "telegram"],
                                "description": "Delivery channel"
                            },
                            "external_chat_id": {
                                "type": "string",
                                "description": "Channel-specific chat id to deliver to"
                            }
                        }
                    }
                }),
                &["op"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        if context.scope == ConversationScope::Secret {
            return ToolResult::error_with_kind(
                ToolErrorKind::InvalidInput,
                "pulse: the pulse tool is not available in secret conversations. Pulse \
                 definitions run and deliver in normal-scope conversations, so scheduling from \
                 here could leak secret context. Ask the user to set up the pulse from a normal \
                 conversation."
                    .to_string(),
            );
        }

        let Some(op) = input.get("op").and_then(|v| v.as_str()) else {
            return ToolResult::error_with_kind(
                ToolErrorKind::InvalidInput,
                "pulse: missing required parameter 'op'".to_string(),
            );
        };

        if context.agent_id.is_empty() {
            return ToolResult::error_with_kind(
                ToolErrorKind::InvalidInput,
                "pulse: no agent context available".to_string(),
            );
        }

        let snapshot = self.config_manager.current_blocking();
        let state_root = snapshot.config.state_root.clone();
        let timezone = snapshot.config.timezone.clone();
        let agent_id = context.agent_id.clone();

        match op {
            "list" => self.op_list(&state_root, &agent_id, &timezone).await,
            "add" => {
                self.op_add(&input, context, &state_root, &agent_id, &timezone)
                    .await
            }
            "update" => {
                self.op_update(&input, &state_root, &agent_id, &timezone)
                    .await
            }
            "remove" => self.op_remove(&input, &state_root, &agent_id).await,
            "enable" | "disable" => {
                let enable = op == "enable";
                self.op_set_enabled(&input, enable, &state_root, &agent_id)
                    .await
            }
            other => ToolResult::error_with_kind(
                ToolErrorKind::InvalidInput,
                format!("pulse: unknown op '{other}'"),
            ),
        }
    }
}

/// Load the agent's document, mapping parse failures to a readable report so
/// the agent can see (and help repair) a hand-edited broken file.
fn load_document(state_root: &str, agent_id: &str) -> Result<PulseDefinition, ToolResult> {
    load_pulse_definition(std::path::Path::new(state_root), agent_id).map_err(|e| {
        ToolResult::error(format!(
            "pulse: PULSE.yaml could not be parsed: {e}\n\
             The file was probably hand-edited. Inspect it with the read tool and fix or \
             rewrite it, or ask the user how to proceed."
        ))
    })
}

fn save_document(
    state_root: &str,
    agent_id: &str,
    document: &PulseDefinition,
) -> Result<(), ToolResult> {
    save_pulse_definition(std::path::Path::new(state_root), agent_id, document)
        .map_err(|e| ToolResult::error(format!("pulse: {e}")))
}

/// Parse an explicit `delivery` input object.
fn parse_explicit_delivery(value: &serde_json::Value) -> Result<DeliverySpec, ToolResult> {
    let channel = value
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let external_chat_id = value
        .get("external_chat_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !definition::VALID_DELIVERY_CHANNELS.contains(&channel.as_str())
        || external_chat_id.is_empty()
    {
        return Err(ToolResult::error_with_kind(
            ToolErrorKind::InvalidInput,
            format!(
                "pulse: delivery requires channel (one of {}) and a non-empty external_chat_id",
                definition::VALID_DELIVERY_CHANNELS.join(", ")
            ),
        ));
    }
    Ok(DeliverySpec {
        channel,
        external_chat_id,
    })
}

/// Resolve the default delivery destination: the current conversation, when
/// its channel can deliver outbound messages.
fn default_delivery_from_context(
    context: &ToolExecutionContext,
) -> Result<DeliverySpec, ToolResult> {
    let channel = context.channel.to_ascii_lowercase();
    if definition::VALID_DELIVERY_CHANNELS.contains(&channel.as_str()) {
        return Ok(DeliverySpec {
            channel,
            external_chat_id: context.surface_thread.clone(),
        });
    }
    Err(ToolResult::error_with_kind(
        ToolErrorKind::InvalidInput,
        format!(
            "pulse: notifications can only be delivered to Discord or Telegram, but this \
             conversation is on '{}'. Pass an explicit delivery (channel + external_chat_id), \
             or set the intention up from a Discord/Telegram chat.",
            context.channel
        ),
    ))
}

/// Parse the `schedule` input object into a [`TemporalSchedule`]. Field-level
/// validation happens on save via the parse round-trip.
fn parse_schedule_input(input: &serde_json::Value) -> Result<TemporalSchedule, ToolResult> {
    let Some(schedule) = input.get("schedule") else {
        return Err(ToolResult::error_with_kind(
            ToolErrorKind::InvalidInput,
            "pulse: missing required parameter 'schedule'".to_string(),
        ));
    };

    let get_str = |key: &str| {
        schedule
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };

    let kind = get_str("kind").unwrap_or_default();
    let at = get_str("at").unwrap_or_default();

    match kind.as_str() {
        "daily" => Ok(TemporalSchedule::Daily { at }),
        "weekly" => Ok(TemporalSchedule::Weekly {
            day: get_str("day").unwrap_or_default().to_ascii_lowercase(),
            at,
        }),
        "interval" => Ok(TemporalSchedule::Interval {
            interval_days: schedule
                .get("interval_days")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            at,
        }),
        "once" => Ok(TemporalSchedule::Once {
            date: get_str("date").unwrap_or_default(),
            at,
        }),
        other => Err(ToolResult::error_with_kind(
            ToolErrorKind::InvalidInput,
            format!(
                "pulse: unknown schedule kind '{other}', expected daily, weekly, interval, or once"
            ),
        )),
    }
}

/// Reject a `once` schedule whose target instant is already past. Format
/// errors are left to the save round-trip, which reports them precisely.
fn ensure_once_is_future(
    schedule: &TemporalSchedule,
    now: chrono::DateTime<chrono::Utc>,
    timezone: &str,
) -> Result<(), ToolResult> {
    use chrono::TimeZone;
    let TemporalSchedule::Once { date, at } = schedule else {
        return Ok(());
    };
    let (Ok(target_date), Ok(target_time)) = (
        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d"),
        chrono::NaiveTime::parse_from_str(at, "%H:%M"),
    ) else {
        return Ok(());
    };
    let Ok(tz) = timezone.parse::<chrono_tz::Tz>() else {
        return Ok(());
    };
    let target = match tz.from_local_datetime(&target_date.and_time(target_time)) {
        chrono::LocalResult::Single(dt) => dt.with_timezone(&chrono::Utc),
        chrono::LocalResult::Ambiguous(earliest, _) => earliest.with_timezone(&chrono::Utc),
        chrono::LocalResult::None => {
            return Err(ToolResult::error_with_kind(
                ToolErrorKind::InvalidInput,
                format!(
                    "pulse: once schedule {date} {at} does not exist in timezone {timezone} \
                     (DST transition). Choose a time that exists on that day."
                ),
            ));
        }
    };
    if target <= now {
        return Err(ToolResult::error_with_kind(
            ToolErrorKind::InvalidInput,
            format!(
                "pulse: once schedule must be in the future, but {date} {at} ({timezone}) is \
                 already past"
            ),
        ));
    }
    Ok(())
}

fn required_text(input: &serde_json::Value, key: &str) -> Result<String, ToolResult> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        _ => Err(ToolResult::error_with_kind(
            ToolErrorKind::InvalidInput,
            format!("pulse: missing required parameter '{key}'"),
        )),
    }
}

fn optional_text(input: &serde_json::Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn require_id(input: &serde_json::Value) -> Result<String, ToolResult> {
    optional_text(input, "id").ok_or_else(|| {
        ToolResult::error_with_kind(
            ToolErrorKind::InvalidInput,
            "pulse: missing required parameter 'id'".to_string(),
        )
    })
}

/// Format an instant as local `YYYY-MM-DD HH:MM` in the runtime timezone; a
/// plain RFC3339 fallback when the timezone is invalid.
fn format_local(instant: chrono::DateTime<chrono::Utc>, timezone: &str) -> String {
    match timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => instant
            .with_timezone(&tz)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        Err(_) => instant.to_rfc3339(),
    }
}

fn ids_line(document: &PulseDefinition) -> String {
    if document.intentions.is_empty() {
        "(none)".to_string()
    } else {
        document
            .intentions
            .iter()
            .map(|intention| intention.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn unique_suggested_id(document: &PulseDefinition, schedule: &TemporalSchedule) -> String {
    let base = suggest_intention_id(schedule);
    let mut candidate = base.clone();
    let mut suffix = 2;
    while document
        .intentions
        .iter()
        .any(|intention| intention.id == candidate)
    {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::build_sleep_app_state_with_path;
    use crate::test_util::test_config;
    use std::collections::HashMap;

    struct TestEnv {
        tool: Arc<PulseTool>,
        state_root: String,
        _dir: tempfile::TempDir,
        _state: Arc<crate::runtime::AppState>,
    }

    fn build_env() -> TestEnv {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8"));
        let state =
            Arc::new(build_sleep_app_state_with_path(config.clone(), None).expect("build state"));
        let tool = Arc::new(PulseTool::new(
            Arc::clone(&state.config_manager),
            Arc::clone(&state.db),
        ));
        TestEnv {
            tool,
            state_root: config.state_root.clone(),
            _dir: dir,
            _state: state,
        }
    }

    fn context() -> ToolExecutionContext {
        ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123456789012345678".to_string(),
            chat_type: "dm".to_string(),
            agent_id: "lyre".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            turn_id: String::new(),
            tool_call_id: String::new(),
            skill_env: Arc::new(std::sync::Mutex::new(HashMap::new())),
            scope: crate::conversation::ConversationScope::Normal,
            config_snapshot: None,
        }
    }

    #[test]
    fn pulse_definition_name_and_ops() {
        let env = build_env();
        let def = env.tool.definition();
        assert_eq!(def.name, "pulse");
        assert!(def.description.contains("attention direction"));
        assert!(
            def.description.contains("once"),
            "description must document the once schedule"
        );
    }

    #[tokio::test]
    async fn list_on_missing_file_reports_enabled_empty() {
        let env = build_env();
        let result = env.tool.execute(json!({"op": "list"}), &context()).await;
        assert!(!result.is_error);
        assert!(result.content.contains("Pulse for agent 'lyre': enabled"));
        assert!(result.content.contains("No intentions configured."));
    }

    #[tokio::test]
    async fn add_creates_file_with_delivery_defaulting_to_current_conversation() {
        let env = build_env();
        let result = env
            .tool
            .execute(
                json!({
                    "op": "add",
                    "schedule": {"kind": "daily", "at": "09:00"},
                    "attention": "Check today's schedule."
                }),
                &context(),
            )
            .await;
        assert!(!result.is_error, "add failed: {}", result.content);
        assert!(result.content.contains("Added intention 'daily-0900'"));
        assert!(result.content.contains("It takes effect on the next scan"));

        let loaded =
            load_pulse_definition(std::path::Path::new(&env.state_root), "lyre").expect("load");
        assert_eq!(loaded.intentions.len(), 1);
        let delivery = loaded.intentions[0].delivery.as_ref().expect("delivery");
        assert_eq!(delivery.channel, "discord");
        assert_eq!(delivery.external_chat_id, "123456789012345678");
    }

    #[tokio::test]
    async fn add_with_explicit_id_and_delivery_roundtrips() {
        let env = build_env();
        let result = env
            .tool
            .execute(
                json!({
                    "op": "add",
                    "id": "morning_review",
                    "schedule": {"kind": "daily", "at": "09:00"},
                    "attention": "Check today's schedule.",
                    "delivery": {"channel": "telegram", "external_chat_id": "987654321"}
                }),
                &context(),
            )
            .await;
        assert!(!result.is_error, "add failed: {}", result.content);

        let loaded =
            load_pulse_definition(std::path::Path::new(&env.state_root), "lyre").expect("load");
        assert_eq!(loaded.intentions[0].id, "morning_review");
        let delivery = loaded.intentions[0].delivery.as_ref().expect("delivery");
        assert_eq!(delivery.channel, "telegram");
    }

    #[tokio::test]
    async fn add_rejects_duplicate_explicit_id_and_dedupes_suggested_ids() {
        let env = build_env();
        let input = json!({
            "op": "add",
            "id": "daily-0900",
            "schedule": {"kind": "daily", "at": "09:00"},
            "attention": "first"
        });
        let first = env.tool.execute(input.clone(), &context()).await;
        assert!(!first.is_error);

        let explicit_dup = env.tool.execute(input, &context()).await;
        assert!(explicit_dup.is_error);
        assert!(explicit_dup.content.contains("already exists"));

        let auto = env
            .tool
            .execute(
                json!({
                    "op": "add",
                    "schedule": {"kind": "daily", "at": "09:00"},
                    "attention": "second"
                }),
                &context(),
            )
            .await;
        assert!(!auto.is_error, "auto id failed: {}", auto.content);
        assert!(auto.content.contains("'daily-0900-2'"));
    }

    #[tokio::test]
    async fn add_rejects_invalid_time_via_validation_roundtrip() {
        let env = build_env();
        let result = env
            .tool
            .execute(
                json!({
                    "op": "add",
                    "schedule": {"kind": "daily", "at": "25:00"},
                    "attention": "check"
                }),
                &context(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid time"));
        let loaded =
            load_pulse_definition(std::path::Path::new(&env.state_root), "lyre").expect("load");
        assert!(
            loaded.intentions.is_empty(),
            "invalid intention must not persist"
        );
    }

    #[tokio::test]
    async fn add_rejects_once_in_the_past_and_accepts_future() {
        let env = build_env();

        let past = env
            .tool
            .execute(
                json!({
                    "op": "add",
                    "schedule": {"kind": "once", "date": "2020-01-01", "at": "09:00"},
                    "attention": "reminder"
                }),
                &context(),
            )
            .await;
        assert!(past.is_error);
        assert!(past.content.contains("must be in the future"));

        // A date far in the future is accepted regardless of the real clock.
        let future = env
            .tool
            .execute(
                json!({
                    "op": "add",
                    "schedule": {"kind": "once", "date": "9999-12-31", "at": "09:00"},
                    "attention": "reminder"
                }),
                &context(),
            )
            .await;
        assert!(!future.is_error, "future once failed: {}", future.content);
    }

    #[tokio::test]
    async fn add_on_non_sendable_channel_requires_explicit_delivery() {
        let env = build_env();
        let mut web_context = context();
        web_context.channel = "web".to_string();
        web_context.surface_thread = "session-1".to_string();

        let result = env
            .tool
            .execute(
                json!({
                    "op": "add",
                    "schedule": {"kind": "daily", "at": "09:00"},
                    "attention": "check"
                }),
                &web_context,
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("Discord or Telegram"));
    }

    #[tokio::test]
    async fn update_remove_and_enable_disable_roundtrip() {
        let env = build_env();
        env.tool
            .execute(
                json!({
                    "op": "add",
                    "id": "morning_review",
                    "schedule": {"kind": "daily", "at": "09:00"},
                    "attention": "check"
                }),
                &context(),
            )
            .await;

        let update = env
            .tool
            .execute(
                json!({
                    "op": "update",
                    "id": "morning_review",
                    "schedule": {"kind": "daily", "at": "10:30"}
                }),
                &context(),
            )
            .await;
        assert!(!update.is_error, "update failed: {}", update.content);

        let disable_intention = env
            .tool
            .execute(json!({"op": "disable", "id": "morning_review"}), &context())
            .await;
        assert!(!disable_intention.is_error);
        let loaded =
            load_pulse_definition(std::path::Path::new(&env.state_root), "lyre").expect("load");
        assert!(!loaded.intentions[0].enabled);

        // Re-enable the intention, then toggle the agent-level switch: the
        // intention flag must be preserved.
        let enable_intention = env
            .tool
            .execute(json!({"op": "enable", "id": "morning_review"}), &context())
            .await;
        assert!(!enable_intention.is_error);

        let disable_agent = env.tool.execute(json!({"op": "disable"}), &context()).await;
        assert!(!disable_agent.is_error);
        let loaded =
            load_pulse_definition(std::path::Path::new(&env.state_root), "lyre").expect("load");
        assert!(!loaded.enabled);
        assert!(
            loaded.intentions[0].enabled,
            "intention flag must be untouched"
        );

        let enable_agent = env.tool.execute(json!({"op": "enable"}), &context()).await;
        assert!(!enable_agent.is_error);

        let remove = env
            .tool
            .execute(json!({"op": "remove", "id": "morning_review"}), &context())
            .await;
        assert!(!remove.is_error);
        let loaded =
            load_pulse_definition(std::path::Path::new(&env.state_root), "lyre").expect("load");
        assert!(loaded.intentions.is_empty());
        assert!(loaded.enabled, "agent-level enabled must survive removals");
    }

    #[tokio::test]
    async fn update_and_remove_report_unknown_ids() {
        let env = build_env();
        let update = env
            .tool
            .execute(
                json!({"op": "update", "id": "ghost", "attention": "x"}),
                &context(),
            )
            .await;
        assert!(update.is_error);
        assert!(update.content.contains("no intention 'ghost'"));

        let remove = env
            .tool
            .execute(json!({"op": "remove", "id": "ghost"}), &context())
            .await;
        assert!(remove.is_error);
        assert!(remove.content.contains("no intention 'ghost'"));
    }

    #[tokio::test]
    async fn list_reports_parse_error_of_hand_edited_file() {
        let env = build_env();
        let agent_dir = std::path::Path::new(&env.state_root)
            .join("agents")
            .join("lyre");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        std::fs::write(agent_dir.join("PULSE.yaml"), "intentions: [broken").expect("write");

        let result = env.tool.execute(json!({"op": "list"}), &context()).await;
        assert!(result.is_error);
        assert!(result.content.contains("could not be parsed"));
    }

    #[tokio::test]
    async fn list_shows_disabled_state_and_next_due() {
        let env = build_env();
        env.tool
            .execute(
                json!({
                    "op": "add",
                    "id": "daily_check",
                    "schedule": {"kind": "daily", "at": "00:00"},
                    "attention": "check"
                }),
                &context(),
            )
            .await;
        env.tool
            .execute(json!({"op": "disable", "id": "daily_check"}), &context())
            .await;
        env.tool.execute(json!({"op": "disable"}), &context()).await;

        let result = env.tool.execute(json!({"op": "list"}), &context()).await;
        assert!(!result.is_error);
        assert!(result.content.contains("Pulse for agent 'lyre': disabled"));
        assert!(result.content.contains("daily_check [disabled]"));
        assert!(
            !result.content.contains("due now"),
            "nothing may be announced as firing: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn missing_op_is_rejected() {
        let env = build_env();
        let result = env.tool.execute(json!({}), &context()).await;
        assert!(result.is_error);
        assert!(result.content.contains("'op'"));
    }

    #[tokio::test]
    async fn secret_conversations_cannot_use_the_pulse_tool() {
        // Arrange
        let env = build_env();
        let mut secret_context = context();
        secret_context.scope = crate::conversation::ConversationScope::Secret;

        // Act
        let result = env
            .tool
            .execute(json!({"op": "list"}), &secret_context)
            .await;

        // Assert: rejected without touching the shared definition file
        assert!(result.is_error);
        assert!(result.content.contains("secret conversations"));
        let loaded =
            load_pulse_definition(std::path::Path::new(&env.state_root), "lyre").expect("load");
        assert!(loaded.intentions.is_empty());
    }

    #[tokio::test]
    async fn successful_save_wakes_the_scheduler() {
        // Arrange: drain any stale permit left by other tests so this one only
        // passes when OUR save notifies.
        let env = build_env();
        let _ = tokio::time::timeout(
            std::time::Duration::ZERO,
            definition::pulse_changed().notified(),
        )
        .await;
        let waiter = definition::pulse_changed().notified();

        // Act
        let result = env
            .tool
            .execute(
                json!({
                    "op": "add",
                    "schedule": {"kind": "daily", "at": "09:00"},
                    "attention": "check"
                }),
                &context(),
            )
            .await;

        // Assert
        assert!(!result.is_error, "add failed: {}", result.content);
        tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
            .await
            .expect("a successful save must wake the scheduler");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_adds_all_persist() {
        // Arrange: five tasks racing on the same PULSE.yaml in parallel must
        // not lose each other's writes to the serialized read-modify-write
        // gate.
        let env = build_env();
        let mut handles = Vec::new();
        for i in 0..5 {
            let tool = Arc::clone(&env.tool);
            handles.push(tokio::spawn(async move {
                let input = json!({
                    "op": "add",
                    "id": format!("int-{i}"),
                    "schedule": {"kind": "daily", "at": "09:00"},
                    "attention": "race"
                });
                tool.execute(input, &context()).await
            }));
        }

        // Act
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.expect("task"));
        }

        // Assert
        for result in &results {
            assert!(!result.is_error, "add failed: {}", result.content);
        }
        let loaded =
            load_pulse_definition(std::path::Path::new(&env.state_root), "lyre").expect("load");
        let ids = loaded
            .intentions
            .iter()
            .map(|intention| intention.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(loaded.intentions.len(), 5, "no add may be lost: {ids:?}");
    }

    #[tokio::test]
    async fn update_rejects_empty_attention() {
        // Arrange
        let env = build_env();
        env.tool
            .execute(
                json!({
                    "op": "add",
                    "id": "daily_check",
                    "schedule": {"kind": "daily", "at": "09:00"},
                    "attention": "check"
                }),
                &context(),
            )
            .await;

        // Act
        let result = env
            .tool
            .execute(
                json!({"op": "update", "id": "daily_check", "attention": "   "}),
                &context(),
            )
            .await;

        // Assert
        assert!(result.is_error);
        assert!(result.content.contains("attention must not be empty"));
    }

    // --- next-due display: scheduler-consistent judgment (fixed clock) ---

    fn fixed_now() -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc
            .with_ymd_and_hms(2026, 5, 10, 18, 0, 0)
            .single()
            .expect("valid utc")
    }

    fn daily_intention(id: &str, at: &str) -> TemporalIntention {
        TemporalIntention {
            id: id.to_string(),
            enabled: true,
            schedule: TemporalSchedule::Daily { at: at.to_string() },
            attention: "check".to_string(),
            delivery: None,
        }
    }

    fn interval_intention(id: &str, interval_days: u32) -> TemporalIntention {
        TemporalIntention {
            id: id.to_string(),
            enabled: true,
            schedule: TemporalSchedule::Interval {
                interval_days,
                at: "09:00".to_string(),
            },
            attention: "check".to_string(),
            delivery: None,
        }
    }

    fn once_intention(id: &str, date: &str) -> TemporalIntention {
        TemporalIntention {
            id: id.to_string(),
            enabled: true,
            schedule: TemporalSchedule::Once {
                date: date.to_string(),
                at: "09:00".to_string(),
            },
            attention: "check".to_string(),
            delivery: None,
        }
    }

    #[tokio::test]
    async fn due_now_when_time_passed_and_period_unconsumed() {
        // Arrange
        let env = build_env();

        // Act + Assert: 09:00 has passed with no run today — the scheduler
        // fires on the next scan, so the display must say so (review case 1).
        let text = env
            .tool
            .next_due_text(
                "lyre",
                &daily_intention("daily_check", "09:00"),
                "UTC",
                fixed_now(),
            )
            .await
            .expect("next due");
        assert_eq!(text, "due now (pending next scan)");

        // Act + Assert: before 09:00 the future same-day candidate is the next
        // due, not "due now" (review case 3).
        let morning = fixed_now() - chrono::Duration::hours(10);
        let text = env
            .tool
            .next_due_text(
                "lyre",
                &daily_intention("daily_check", "09:00"),
                "UTC",
                morning,
            )
            .await
            .expect("next due");
        assert_eq!(text, "next due 2026-05-10 09:00 (UTC)");
    }

    #[tokio::test]
    async fn consumed_period_shows_next_occurrence_or_already_attempted() {
        // Arrange: today's 09:00 already ran (review cases 2 and 4)
        let env = build_env();
        env._state
            .db
            .try_create_pulse_run(
                "run-1",
                "lyre",
                "daily_check",
                "lyre:daily_check:2026-05-10",
            )
            .expect("insert run");

        // Act + Assert: the next day's key is unconsumed, so it is the next due
        let text = env
            .tool
            .next_due_text(
                "lyre",
                &daily_intention("daily_check", "09:00"),
                "UTC",
                fixed_now(),
            )
            .await
            .expect("next due");
        assert_eq!(text, "next due 2026-05-11 09:00 (UTC)");

        // Act + Assert: a same-day move to 21:00 lands in the consumed period
        let text = env
            .tool
            .next_due_text(
                "lyre",
                &daily_intention("daily_check", "21:00"),
                "UTC",
                fixed_now(),
            )
            .await
            .expect("next due");
        assert_eq!(text, "already attempted for current period");
    }

    #[tokio::test]
    async fn interval_display_reflects_run_history() {
        // Arrange (review cases 6 and 7)
        let env = build_env();
        env._state
            .db
            .try_create_pulse_run(
                "run-failed",
                "lyre",
                "failed_check",
                "lyre:failed_check:2026-05-10",
            )
            .expect("insert run");
        env._state
            .db
            .update_pulse_run_failed("run-failed", "activation failed")
            .expect("mark failed");
        env._state
            .db
            .try_create_pulse_run(
                "run-success",
                "lyre",
                "window_check",
                "lyre:window_check:2026-05-10",
            )
            .expect("insert run");
        {
            let conn = env._state.db.get_conn().expect("pool");
            conn.execute(
                "UPDATE pulse_runs SET started_at = ?1 WHERE id = ?2",
                rusqlite::params!["2026-05-10T09:00:00+00:00", "run-success"],
            )
            .expect("backdate started_at");
        }
        env._state
            .db
            .update_pulse_run_success(
                "run-success",
                None,
                None,
                crate::storage::PulseOutputKind::Silent,
                "PULSE_OK",
            )
            .expect("mark success");

        // Act + Assert: a failed attempt still consumes the day — no "due now"
        let text = env
            .tool
            .next_due_text(
                "lyre",
                &interval_intention("failed_check", 1),
                "UTC",
                fixed_now(),
            )
            .await
            .expect("next due");
        assert_eq!(text, "already attempted for current period");

        // Act + Assert: a success anchors the next window three days out
        let text = env
            .tool
            .next_due_text(
                "lyre",
                &interval_intention("window_check", 3),
                "UTC",
                fixed_now(),
            )
            .await
            .expect("next due");
        assert_eq!(text, "next due 2026-05-13 09:00 (UTC)");
    }

    #[tokio::test]
    async fn once_display_reflects_attempt_state() {
        // Arrange (review cases 9 and 10)
        let env = build_env();
        env._state
            .db
            .try_create_pulse_run("run-1", "lyre", "reminder", "lyre:reminder:2026-05-10")
            .expect("insert run");

        // Act + Assert: an attempted once never retries
        let text = env
            .tool
            .next_due_text(
                "lyre",
                &once_intention("reminder", "2026-05-10"),
                "UTC",
                fixed_now(),
            )
            .await
            .expect("next due");
        assert_eq!(text, "already attempted (no automatic retry)");

        // Act + Assert: an expired, never-run once has no upcoming due
        let text = env
            .tool
            .next_due_text(
                "lyre",
                &once_intention("expired", "2020-01-01"),
                "UTC",
                fixed_now(),
            )
            .await
            .expect("next due");
        assert_eq!(text, "no upcoming due");
    }

    #[test]
    fn once_schedule_rejects_nonexistent_dst_time() {
        // Arrange: 02:30 does not exist on the US spring-forward day.
        let schedule = TemporalSchedule::Once {
            date: "2026-03-08".to_string(),
            at: "02:30".to_string(),
        };

        // Act
        let result = ensure_once_is_future(&schedule, chrono::Utc::now(), "America/New_York");

        // Assert
        let error = result.expect_err("a nonexistent local time must be rejected");
        assert!(
            error.content.contains("does not exist"),
            "{}",
            error.content
        );
    }
}

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct PulseDefinition {
    pub intentions: Vec<TemporalIntention>,
    pub body: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct TemporalIntention {
    pub id: String,
    pub schedule: TemporalSchedule,
    pub attention: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum TemporalSchedule {
    Daily { at: String },
    Weekly { day: String, at: String },
    Once { at: String },
}

#[derive(Debug, Error)]
#[allow(dead_code)]
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
}

#[derive(Deserialize)]
struct PulseFrontMatter {
    #[allow(dead_code)]
    version: u32,
    #[serde(default)]
    intentions: Vec<IntentionRaw>,
}

#[derive(Deserialize)]
struct IntentionRaw {
    id: String,
    schedule: ScheduleRaw,
    attention: String,
}

#[derive(Deserialize)]
struct ScheduleRaw {
    kind: String,
    at: String,
    day: Option<String>,
}

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
            intentions: Vec::new(),
            body: String::new(),
        });
    }

    let Some(rest) = trimmed.strip_prefix("---") else {
        return Ok(PulseDefinition {
            intentions: Vec::new(),
            body: trimmed.to_string(),
        });
    };

    let Some(rest) = rest.strip_prefix('\n') else {
        return Ok(PulseDefinition {
            intentions: Vec::new(),
            body: trimmed.to_string(),
        });
    };

    let Some(end) = rest.find("\n---") else {
        return Ok(PulseDefinition {
            intentions: Vec::new(),
            body: trimmed.to_string(),
        });
    };

    let frontmatter_str = &rest[..end];
    let body = rest[end + 4..].trim().to_string();

    let fm: PulseFrontMatter =
        yaml_serde::from_str(frontmatter_str).map_err(|e| PulseParseError::ParseFailed {
            agent_id: agent_id.to_string(),
            detail: e.to_string(),
        })?;

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
        intentions.push(TemporalIntention {
            id: raw.id,
            schedule,
            attention: raw.attention,
        });
    }

    Ok(PulseDefinition { intentions, body })
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
        "once" => {
            validate_rfc3339(agent_id, &raw.id, &raw.schedule.at)?;
            Ok(TemporalSchedule::Once {
                at: raw.schedule.at.clone(),
            })
        }
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

fn validate_rfc3339(agent_id: &str, intention_id: &str, at: &str) -> Result<(), PulseParseError> {
    if chrono::DateTime::parse_from_rfc3339(at).is_err() {
        return Err(PulseParseError::InvalidSchedule {
            agent_id: agent_id.to_string(),
            intention_id: intention_id.to_string(),
            detail: format!("invalid RFC3339 datetime: {at}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
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
  - id: event_check
    schedule:
      kind: once
      at: \"2026-05-12T18:00:00+09:00\"
    attention: |
      Check the event.
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

        assert_eq!(result.intentions.len(), 3);
        assert_eq!(result.intentions[0].id, "morning_review");
        assert!(result.body.contains("# PULSE"));
        assert!(result.body.contains("Don't notify for trivial changes."));
    }

    #[test]
    fn parse_daily_weekly_once_intentions() {
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
        assert!(matches!(
            &result.intentions[2].schedule,
            TemporalSchedule::Once { at } if at == "2026-05-12T18:00:00+09:00"
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
        assert!(result.intentions[2].attention.contains("Check the event"));
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
        let result = parse_pulse_definition(no_frontmatter).unwrap();
        assert!(result.intentions.is_empty());
        assert!(result.body.contains("# Just a heading"));
    }
}

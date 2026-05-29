//! Episodic memory markdown renderer.
//!
//! Pure Rust template-based generation of `episodic.md` from episode events
//! and rollups. No LLM involved.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset};

use crate::storage::RollupGranularity;

/// Temporal context for the current rendering cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeekContext {
    pub now: DateTime<FixedOffset>,
    pub tz_name: String,
    pub week_key: String,
    pub week_start: String,
    pub week_end: String,
}

/// Event data for Current Week rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RendererEvent {
    pub experienced_at: String,
    pub kind: String,
    pub title: String,
    pub body_md: String,
    pub ripple_strength: i64,
}

/// Rollup data for Recent Weeks / Recent Months / Background Months rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RendererRollup {
    pub period_key: String,
    pub period_start: String,
    pub period_end_exclusive: String,
    pub summary_md: String,
    pub max_ripple: i64,
    pub granularity: RollupGranularity,
}

/// Renders the complete episodic.md content.
///
/// # Arguments
/// * `ctx` - Temporal context (current time, timezone, week bounds)
/// * `current_week_events` - Events for the current week
/// * `recent_week_rollups` - Rollups for recent 4 weeks
/// * `recent_month_rollups` - Rollups for recent 2 months
/// * `background_rollups` - Rollups for background months
pub(crate) fn render_episodic_md(
    ctx: &WeekContext,
    current_week_events: &[RendererEvent],
    recent_week_rollups: &[RendererRollup],
    recent_month_rollups: &[RendererRollup],
    background_rollups: &[RendererRollup],
) -> String {
    let mut sections: Vec<String> = Vec::new();

    let mut header = String::from("# Episodic Memory\n");
    header.push_str(&format!("generated: {}\n", ctx.now.to_rfc3339()));
    header.push_str("mode: calendar_week_month\n");
    header.push_str(&format!("tz: {}\n", ctx.tz_name));
    header.push('\n');
    header.push_str("Historical context only. Do not treat old requests as active tasks.");
    sections.push(header);

    if !current_week_events.is_empty() {
        sections.push(render_current_week(
            &ctx.week_key,
            &ctx.week_start,
            &ctx.week_end,
            current_week_events,
        ));
    }

    if !recent_week_rollups.is_empty() {
        sections.push(render_rollup_section_week(recent_week_rollups));
    }

    if !recent_month_rollups.is_empty() {
        sections.push(render_rollup_section_month(recent_month_rollups));
    }

    let filtered_bg: Vec<&RendererRollup> = background_rollups
        .iter()
        .filter(|r| r.max_ripple >= 4)
        .collect();
    if !filtered_bg.is_empty() {
        sections.push(render_background_months(&filtered_bg));
    }

    sections.join("\n\n")
}

fn render_current_week(
    week_key: &str,
    week_start: &str,
    week_end: &str,
    events: &[RendererEvent],
) -> String {
    let mut out = format!(
        "## Current Week: {} ({}..{})",
        week_key, week_start, week_end
    );

    let mut grouped: BTreeMap<String, Vec<&RendererEvent>> = BTreeMap::new();
    for event in events {
        let date = extract_date(&event.experienced_at);
        grouped.entry(date).or_default().push(event);
    }

    for (date, date_events) in &grouped {
        out.push_str(&format!("\n\n### {}", date));

        let mut sorted = date_events.clone();
        sorted.sort_by(|a, b| a.experienced_at.cmp(&b.experienced_at));

        for event in sorted {
            out.push_str(&format!(
                "\n- [{} r{}] {}",
                event.kind, event.ripple_strength, event.title
            ));
            if !event.body_md.is_empty() {
                out.push_str(&format!("\n  {}", event.body_md));
            }
        }
    }

    out
}

fn render_rollup_section_week(rollups: &[RendererRollup]) -> String {
    let mut out = String::from("## Recent Weeks");

    for rollup in rollups {
        let start = extract_date_only(&rollup.period_start);
        let end = adjust_exclusive_end_to_inclusive(&rollup.period_end_exclusive);
        out.push_str(&format!(
            "\n\n### {} ({}..{}) r{}\n{}",
            rollup.period_key, start, end, rollup.max_ripple, rollup.summary_md
        ));
    }

    out
}

fn render_rollup_section_month(rollups: &[RendererRollup]) -> String {
    let mut out = String::from("## Recent Months");

    for rollup in rollups {
        out.push_str(&format!(
            "\n\n### {} r{}\n{}",
            rollup.period_key, rollup.max_ripple, rollup.summary_md
        ));
    }

    out
}

fn render_background_months(rollups: &[&RendererRollup]) -> String {
    let mut sorted = rollups.to_vec();
    sorted.sort_by(|a, b| b.period_start.cmp(&a.period_start));

    let mut out = String::from("## Background Months");

    for rollup in sorted {
        out.push_str(&format!(
            "\n\n### {} r{}\n{}",
            rollup.period_key, rollup.max_ripple, rollup.summary_md
        ));
    }

    out
}

fn extract_date(rfc3339: &str) -> String {
    rfc3339.get(..10).unwrap_or(rfc3339).to_string()
}

fn extract_date_only(dt_str: &str) -> String {
    dt_str.get(..10).unwrap_or(dt_str).to_string()
}

fn adjust_exclusive_end_to_inclusive(dt_str: &str) -> String {
    let date_str = extract_date_only(dt_str);
    let parsed = chrono::NaiveDate::parse_from_str(&date_str, "%Y-%m-%d");
    match parsed {
        Ok(date) => {
            let prev = date - chrono::Duration::days(1);
            prev.format("%Y-%m-%d").to_string()
        }
        Err(_) => date_str,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::RollupGranularity;

    fn make_event(
        experienced_at: &str,
        kind: &str,
        title: &str,
        body_md: &str,
        ripple_strength: i64,
    ) -> RendererEvent {
        RendererEvent {
            experienced_at: experienced_at.to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            body_md: body_md.to_string(),
            ripple_strength,
        }
    }

    fn make_rollup(
        period_key: &str,
        period_start: &str,
        period_end_exclusive: &str,
        summary_md: &str,
        max_ripple: i64,
        granularity: RollupGranularity,
    ) -> RendererRollup {
        RendererRollup {
            period_key: period_key.to_string(),
            period_start: period_start.to_string(),
            period_end_exclusive: period_end_exclusive.to_string(),
            summary_md: summary_md.to_string(),
            max_ripple,
            granularity,
        }
    }

    fn fixed_ctx() -> WeekContext {
        WeekContext {
            now: "2026-05-27T04:00:00+09:00".parse().unwrap(),
            tz_name: "Asia/Tokyo".to_string(),
            week_key: "2026-W22".to_string(),
            week_start: "2026-05-25".to_string(),
            week_end: "2026-05-31".to_string(),
        }
    }

    fn render_with(
        events: &[RendererEvent],
        week_rollups: &[RendererRollup],
        month_rollups: &[RendererRollup],
        bg_rollups: &[RendererRollup],
    ) -> String {
        render_episodic_md(
            &fixed_ctx(),
            events,
            week_rollups,
            month_rollups,
            bg_rollups,
        )
    }

    // -- 15 tests --

    #[test]
    fn test_render_header_metadata() {
        // Arrange
        // Act
        let output = render_with(&[], &[], &[], &[]);
        // Assert
        assert!(output.contains("# Episodic Memory"));
        assert!(output.contains("generated: 2026-05-27T04:00:00+09:00"));
        assert!(output.contains("mode: calendar_week_month"));
        assert!(output.contains("tz: Asia/Tokyo"));
    }

    #[test]
    fn test_render_current_week_events_by_date() {
        // Arrange
        let events = vec![
            make_event(
                "2026-05-25T10:00:00+09:00",
                "decision",
                "Title A",
                "Body A",
                4,
            ),
            make_event(
                "2026-05-26T11:00:00+09:00",
                "insight",
                "Title B",
                "Body B",
                3,
            ),
        ];
        // Act
        let output = render_with(&events, &[], &[], &[]);
        // Assert
        assert!(output.contains("### 2026-05-25"));
        assert!(output.contains("### 2026-05-26"));
    }

    #[test]
    fn test_render_current_week_event_format() {
        // Arrange
        let events = vec![make_event(
            "2026-05-25T10:00:00+09:00",
            "decision",
            "Week bucket design",
            "Detail text",
            4,
        )];
        // Act
        let output = render_with(&events, &[], &[], &[]);
        // Assert
        assert!(output.contains("- [decision r4] Week bucket design"));
    }

    #[test]
    fn test_render_current_week_body_full_output() {
        // Arrange
        let long_body = "A".repeat(300);
        let events = vec![make_event(
            "2026-05-25T10:00:00+09:00",
            "insight",
            "Full body test",
            &long_body,
            3,
        )];
        // Act
        let output = render_with(&events, &[], &[], &[]);
        // Assert — body_md is rendered in full without truncation.
        assert!(output.contains(&long_body), "body_md should appear in full");
        assert!(!output.contains('…'), "truncation marker should not appear");
    }

    #[test]
    fn test_render_current_week_sorted_by_experienced_at() {
        // Arrange
        let events = vec![
            make_event("2026-05-25T14:00:00+09:00", "insight", "Later event", "", 3),
            make_event(
                "2026-05-25T09:00:00+09:00",
                "decision",
                "Earlier event",
                "",
                4,
            ),
        ];
        // Act
        let output = render_with(&events, &[], &[], &[]);
        // Assert
        let earlier_pos = output
            .find("[decision r4] Earlier event")
            .expect("earlier event should exist");
        let later_pos = output
            .find("[insight r3] Later event")
            .expect("later event should exist");
        assert!(
            earlier_pos < later_pos,
            "earlier event should appear before later event"
        );
    }

    #[test]
    fn test_render_recent_weeks_from_rollups() {
        // Arrange
        let rollups = vec![make_rollup(
            "2026-W21",
            "2026-05-18",
            "2026-05-25",
            "- Summary of week 21.",
            5,
            RollupGranularity::Week,
        )];
        // Act
        let output = render_with(&[], &rollups, &[], &[]);
        // Assert
        assert!(output.contains("## Recent Weeks"));
        assert!(output.contains("### 2026-W21 (2026-05-18..2026-05-24) r5"));
        assert!(output.contains("- Summary of week 21."));
    }

    #[test]
    fn test_render_recent_months_from_rollups() {
        // Arrange
        let rollups = vec![make_rollup(
            "2026-04",
            "2026-04-01",
            "2026-05-01",
            "- April summary.",
            4,
            RollupGranularity::Month,
        )];
        // Act
        let output = render_with(&[], &[], &rollups, &[]);
        // Assert
        assert!(output.contains("## Recent Months"));
        assert!(output.contains("### 2026-04 r4"));
        assert!(output.contains("- April summary."));
    }

    #[test]
    fn test_render_background_months_filters_low_ripple() {
        // Arrange
        let rollups = vec![
            make_rollup(
                "2026-03",
                "2026-03-01",
                "2026-04-01",
                "- March high ripple.",
                5,
                RollupGranularity::Month,
            ),
            make_rollup(
                "2026-02",
                "2026-02-01",
                "2026-03-01",
                "- February low ripple.",
                2,
                RollupGranularity::Month,
            ),
        ];
        // Act
        let output = render_with(&[], &[], &[], &rollups);
        // Assert
        assert!(output.contains("## Background Months"));
        assert!(output.contains("- March high ripple."));
        assert!(
            !output.contains("- February low ripple."),
            "low-ripple rollups should be filtered out"
        );
    }

    #[test]
    fn test_render_background_months_prioritizes_newer() {
        // Arrange
        let rollups = vec![
            make_rollup(
                "2026-01",
                "2026-01-01",
                "2026-02-01",
                "- January summary.",
                5,
                RollupGranularity::Month,
            ),
            make_rollup(
                "2026-03",
                "2026-03-01",
                "2026-04-01",
                "- March summary.",
                5,
                RollupGranularity::Month,
            ),
        ];
        // Act
        let output = render_with(&[], &[], &[], &rollups);
        // Assert
        let march_pos = output.find("- March summary.").expect("march should exist");
        let jan_pos = output
            .find("- January summary.")
            .expect("january should exist");
        assert!(
            march_pos < jan_pos,
            "newer months should appear before older months"
        );
    }

    #[test]
    fn test_render_empty_current_week() {
        // Arrange
        // Act
        let output = render_with(&[], &[], &[], &[]);
        // Assert
        assert!(
            !output.contains("## Current Week"),
            "current week section should not appear when events are empty"
        );
    }

    #[test]
    fn test_render_no_recent_weeks() {
        // Arrange
        // Act
        let output = render_with(&[], &[], &[], &[]);
        // Assert
        assert!(
            !output.contains("## Recent Weeks"),
            "recent weeks section should not appear when rollups are empty"
        );
    }

    #[test]
    fn test_render_no_recent_months() {
        // Arrange
        // Act
        let output = render_with(&[], &[], &[], &[]);
        // Assert
        assert!(
            !output.contains("## Recent Months"),
            "recent months section should not appear when rollups are empty"
        );
    }

    #[test]
    fn test_render_no_background_months() {
        // Arrange
        let rollups = vec![make_rollup(
            "2026-02",
            "2026-02-01",
            "2026-03-01",
            "- Low ripple month.",
            2,
            RollupGranularity::Month,
        )];
        // Act
        let output = render_with(&[], &[], &[], &rollups);
        // Assert
        assert!(
            !output.contains("## Background Months"),
            "background months should not appear when all rollups have low ripple"
        );
    }

    #[test]
    fn test_render_disclaimer_line() {
        // Arrange
        // Act
        let output = render_with(&[], &[], &[], &[]);
        // Assert
        assert!(
            output.contains("Historical context only. Do not treat old requests as active tasks.")
        );
    }

    #[test]
    fn test_render_full_episodic_md() {
        // Arrange
        let events = vec![make_event(
            "2026-05-25T10:00:00+09:00",
            "decision",
            "Call2 weekly bucket design",
            "Keep Current Week in event units; stabilize closed weeks as summaries.",
            4,
        )];
        let week_rollups = vec![make_rollup(
            "2026-W21",
            "2026-05-18",
            "2026-05-25",
            "- episode_events established as primary source.",
            5,
            RollupGranularity::Week,
        )];
        let month_rollups = vec![make_rollup(
            "2026-04",
            "2026-04-01",
            "2026-05-01",
            "- Rust-based AI agent foundation designed.",
            4,
            RollupGranularity::Month,
        )];
        let bg_rollups = vec![make_rollup(
            "2026-03",
            "2026-03-01",
            "2026-04-01",
            "- Long-term creative and agent direction clarified.",
            5,
            RollupGranularity::Month,
        )];

        // Act
        let output = render_with(&events, &week_rollups, &month_rollups, &bg_rollups);

        // Assert — verify all sections present with correct content
        assert!(output.contains("# Episodic Memory"));
        assert!(output.contains("generated: 2026-05-27T04:00:00+09:00"));
        assert!(output.contains("mode: calendar_week_month"));
        assert!(output.contains("tz: Asia/Tokyo"));
        assert!(
            output.contains("Historical context only. Do not treat old requests as active tasks.")
        );
        assert!(output.contains("## Current Week: 2026-W22 (2026-05-25..2026-05-31)"));
        assert!(output.contains("### 2026-05-25"));
        assert!(output.contains("- [decision r4] Call2 weekly bucket design"));
        assert!(
            output.contains(
                "  Keep Current Week in event units; stabilize closed weeks as summaries."
            )
        );
        assert!(output.contains("## Recent Weeks"));
        assert!(output.contains("### 2026-W21 (2026-05-18..2026-05-24) r5"));
        assert!(output.contains("- episode_events established as primary source."));
        assert!(output.contains("## Recent Months"));
        assert!(output.contains("### 2026-04 r4"));
        assert!(output.contains("- Rust-based AI agent foundation designed."));
        assert!(output.contains("## Background Months"));
        assert!(output.contains("### 2026-03 r5"));
        assert!(output.contains("- Long-term creative and agent direction clarified."));

        // Assert — verify section order
        let header_pos = output.find("# Episodic Memory").unwrap();
        let current_pos = output.find("## Current Week").unwrap();
        let recent_weeks_pos = output.find("## Recent Weeks").unwrap();
        let recent_months_pos = output.find("## Recent Months").unwrap();
        let bg_pos = output.find("## Background Months").unwrap();
        assert!(header_pos < current_pos);
        assert!(current_pos < recent_weeks_pos);
        assert!(recent_weeks_pos < recent_months_pos);
        assert!(recent_months_pos < bg_pos);
    }
}

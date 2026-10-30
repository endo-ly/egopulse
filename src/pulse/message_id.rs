//! Message ID namespace for the conversation rows a Pulse notification persists.
//!
//! A notified activation is stored as a synthetic input, zero or more tool
//! phases (narration with its tool calls, then a tool result summary), and the
//! final output. Every row carries the activation's pulse run ID and its role,
//! so readers of the stored history (the WebUI) can fold the activation trace
//! under the output that the channel actually delivered.

const NAMESPACE: &str = "pulse";
const INPUT: &str = "input";
const STEP: &str = "step";
const RESULT: &str = "result";
const OUTPUT: &str = "output";

/// Role of a persisted row within one Pulse activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PulseRole {
    /// Synthetic input that injects the intention into the session.
    Input,
    /// Tool phase narration, its tool calls, and its tool result summary.
    Step,
    /// Final output delivered to the channel.
    Output,
}

impl PulseRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Input => INPUT,
            Self::Step => STEP,
            Self::Output => OUTPUT,
        }
    }
}

/// A Pulse row identified from its message ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PulseMessage<'a> {
    pub run_id: &'a str,
    pub role: PulseRole,
}

/// ID for the synthetic input row of `run_id`.
pub(crate) fn input(run_id: &str) -> String {
    format!("{NAMESPACE}:{run_id}:{INPUT}")
}

/// ID for a tool phase's assistant row of `run_id`; its tool calls are anchored to it.
pub(crate) fn step(run_id: &str) -> String {
    format!("{NAMESPACE}:{run_id}:{STEP}:{}", uuid::Uuid::new_v4())
}

/// ID for a tool phase's tool result summary row of `run_id`.
pub(crate) fn tool_result(run_id: &str) -> String {
    format!("{NAMESPACE}:{run_id}:{RESULT}:{}", uuid::Uuid::new_v4())
}

/// ID for the final output row of `run_id`.
pub(crate) fn output(run_id: &str) -> String {
    format!("{NAMESPACE}:{run_id}:{OUTPUT}")
}

/// Identifies the Pulse activation and role a message ID belongs to.
///
/// Returns `None` for rows outside the Pulse namespace. A tool call belongs
/// to the activation of its parent message ID.
pub(crate) fn parse(message_id: &str) -> Option<PulseMessage<'_>> {
    let rest = message_id.strip_prefix(NAMESPACE)?.strip_prefix(':')?;
    let (run_id, kind) = rest.split_once(':')?;
    if run_id.is_empty() {
        return None;
    }
    let role = match kind.split_once(':') {
        None if kind == INPUT => PulseRole::Input,
        None if kind == OUTPUT => PulseRole::Output,
        Some((STEP | RESULT, suffix)) if !suffix.is_empty() => PulseRole::Step,
        _ => return None,
    };
    Some(PulseMessage { run_id, role })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_parse_back_to_their_run_and_role() {
        // Arrange
        let run_id = "0b6d3c1e-run";
        let cases = [
            (input(run_id), PulseRole::Input),
            (step(run_id), PulseRole::Step),
            (tool_result(run_id), PulseRole::Step),
            (output(run_id), PulseRole::Output),
        ];

        for (id, role) in cases {
            // Act
            let parsed = parse(&id);

            // Assert
            assert_eq!(parsed, Some(PulseMessage { run_id, role }), "{id}");
        }
    }

    #[test]
    fn ids_outside_the_pulse_namespace_do_not_parse() {
        // Arrange
        let ids = [
            uuid::Uuid::new_v4().to_string(),
            "web:3f0c".to_string(),
            "pulse".to_string(),
            "pulse::input".to_string(),
            "pulse:run:step".to_string(),
            "pulse:run:unknown".to_string(),
            "pulse-out-0b6d3c1e".to_string(),
        ];

        for id in ids {
            // Act
            let parsed = parse(&id);

            // Assert
            assert_eq!(parsed, None, "{id}");
        }
    }
}

use serde_json::Value;

pub(crate) fn format_webhook_payload(receiver_id: &str, payload: &Value) -> String {
    if is_egograph_pipelines(payload) {
        format_egograph(receiver_id, payload)
    } else {
        format_generic(receiver_id, payload)
    }
}

fn is_egograph_pipelines(payload: &Value) -> bool {
    let source_match = payload
        .get("source")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s == "urn:egograph:pipelines");

    let type_match = payload
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s == "egograph.pipelines.workflow_failed");

    source_match || type_match
}

fn format_egograph(receiver_id: &str, payload: &Value) -> String {
    let mut lines = Vec::new();
    lines.push(format!("External webhook event from {receiver_id}."));
    lines.push(String::new());

    if let Some(event_type) = payload.get("type").and_then(|v| v.as_str()) {
        lines.push(format!("type: {event_type}"));
    }

    if let Some(data) = payload.get("data") {
        for key in &["workflow_id", "run_id", "error_message", "custom_message"] {
            if let Some(val) = data.get(key).and_then(|v| v.as_str()) {
                lines.push(format!("{key}: {val}"));
            }
        }
    }

    lines.push(String::new());
    lines.push(
        "Please inspect the failure, identify the likely cause, assess whether user action is required, and report the recommended next action."
            .to_string(),
    );

    lines.join("\n")
}

fn format_generic(receiver_id: &str, payload: &Value) -> String {
    let pretty = serde_json::to_string_pretty(payload).unwrap_or_else(|_| "{}".to_string());

    format!(
        "External webhook event from {receiver_id}.\n\nPayload:\n{pretty}\n\nPlease inspect this event and take the appropriate action."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_egograph_pipeline_failure_for_agent_action() {
        let payload = json!({
            "source": "urn:egograph:pipelines",
            "type": "egograph.pipelines.workflow_failed",
            "data": {
                "workflow_id": "spotify_ingest_workflow",
                "run_id": "722e2f38-def8-4bba-9283-bfe07459935c",
                "error_message": "AuthenticationError: Spotify refresh token revoked",
                "custom_message": "認証でエラーが発生しました。再認証スクリプトを実行してください: uv run python scripts/spotify_auth.py"
            }
        });

        let formatted = format_webhook_payload("egograph", &payload);

        assert!(formatted.contains("External webhook event from egograph."));
        assert!(formatted.contains("type: egograph.pipelines.workflow_failed"));
        assert!(formatted.contains("workflow_id: spotify_ingest_workflow"));
        assert!(formatted.contains("run_id: 722e2f38-def8-4bba-9283-bfe07459935c"));
        assert!(
            formatted.contains("error_message: AuthenticationError: Spotify refresh token revoked")
        );
        assert!(formatted.contains("custom_message:"));
        assert!(formatted.contains("likely cause"));
        assert!(formatted.contains("user action is required"));
        assert!(formatted.contains("recommended next action"));
    }

    #[test]
    fn formats_unknown_json_payload_as_generic_webhook_event() {
        let payload = json!({
            "action": "opened",
            "number": 42,
            "repository": {
                "name": "egopulse"
            }
        });

        let formatted = format_webhook_payload("github", &payload);

        assert!(formatted.contains("External webhook event from github."));
        assert!(formatted.contains("\"action\": \"opened\""));
        assert!(formatted.contains("\"number\": 42"));
        assert!(formatted.contains("\"repository\""));
        assert!(formatted.contains("Please inspect this event"));
    }
}

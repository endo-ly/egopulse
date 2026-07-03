//! Agent listing API endpoint for the WebUI sidebar.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use super::WebState;

#[derive(Debug, Serialize)]
pub(super) struct AgentInfo {
    id: String,
    label: String,
    is_default: bool,
    active: bool,
}

pub(super) async fn list_agents(
    State(state): State<WebState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let config = &state.app_state.config;
    let default_agent = &config.default_agent;

    let mut agents: Vec<AgentInfo> = config
        .agents
        .iter()
        .map(|(id, agent_config)| AgentInfo {
            id: id.to_string(),
            label: agent_config.label.clone(),
            is_default: id == default_agent,
            active: state.app_state.active_turns.is_active(id.as_str()),
        })
        .collect();
    agents.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(Json(serde_json::json!({"ok": true, "agents": agents})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State as AxumState;

    use crate::channels::web::{RunHub, WebState};
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::test_util::build_state_with_provider;
    use std::sync::Arc;

    struct DummyLlm;

    #[async_trait::async_trait]
    impl LlmProvider for DummyLlm {
        fn provider_name(&self) -> &str {
            "dummy"
        }

        fn model_name(&self) -> &str {
            "dummy"
        }

        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<MessagesResponse, LlmError> {
            panic!("handler tests should not call LLM")
        }
    }

    fn test_web_state(dir: &tempfile::TempDir) -> WebState {
        let state_root = dir.path().to_string_lossy().to_string();
        let app_state = build_state_with_provider(&state_root, Box::new(DummyLlm));
        WebState {
            app_state: Arc::new(app_state),
            config_path: None,
            run_hub: RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    #[tokio::test]
    async fn api_agents_returns_configured_agents_with_active_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        web_state.app_state.active_turns.begin_turn("default");

        let result = list_agents(AxumState(web_state)).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));

        let agents = body["agents"].as_array().expect("agents array");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["id"], "default");
        assert_eq!(agents[0]["label"], "Default Agent");
        assert_eq!(agents[0]["is_default"], true);
        assert_eq!(agents[0]["active"], true);
    }

    #[tokio::test]
    async fn api_agents_active_false_when_no_turn_in_flight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let result = list_agents(AxumState(web_state)).await.expect("ok");
        let body = result.0;
        let agents = body["agents"].as_array().expect("agents array");
        assert_eq!(agents[0]["active"], false);
    }
}

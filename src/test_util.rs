//! テスト全体で共有されるヘルパー関数。
//!
//! 各モジュールの `#[cfg(test)]` ブロックから `crate::test_util::*` で利用する。

use std::sync::Arc;

use crate::assets::AssetStore;
use crate::channels::adapter::ChannelRegistry;
use crate::config::{
    AgentConfig, AgentId, ChannelConfig, ChannelName, Config, ProviderConfig, ProviderId,
    secret_ref::ResolvedValue,
};
use crate::runtime::AppState;
use crate::skills::SkillManager;
use crate::storage::Database;
use crate::tools::ToolRegistry;

/// テスト用の最小 Config を生成する。
///
/// OpenAI プロバイダ (`sk-test`) を含み、`state_root` 配下に一時ディレクトリを置く。
pub(crate) fn test_config(state_root: &str) -> Config {
    Config {
        default_provider: ProviderId::new("openai"),
        default_model: Some("gpt-4o-mini".to_string()),
        providers: std::collections::HashMap::from([(
            ProviderId::new("openai"),
            ProviderConfig {
                label: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: Some(ResolvedValue::Literal("sk-test".to_string())),
                default_model: "gpt-4o-mini".to_string(),
                models: std::collections::HashMap::from([(
                    "gpt-4o-mini".to_string(),
                    crate::config::ModelConfig::default(),
                )]),
            },
        )]),
        state_root: state_root.to_string(),
        log_level: "info".to_string(),
        compaction_timeout_secs: 180,
        max_history_messages: 50,
        compact_keep_recent: 20,
        default_context_window_tokens: 32768,
        compaction_threshold_ratio: 0.80,
        compaction_target_ratio: 0.40,
        channels: std::collections::HashMap::from([(
            ChannelName::new("web"),
            ChannelConfig {
                enabled: Some(true),
                host: Some("127.0.0.1".to_string()),
                port: Some(10961),
                ..Default::default()
            },
        )]),
        default_agent: AgentId::new("default"),
        agents: std::collections::HashMap::from([(
            AgentId::new("default"),
            AgentConfig {
                label: "Default Agent".to_string(),
                ..Default::default()
            },
        )]),
    }
}

/// テスト用 AppState を構築する。LLM プロバイダを注入可能。
pub(crate) fn build_state_with_provider(
    state_root: &str,
    llm: Box<dyn crate::llm::LlmProvider>,
) -> AppState {
    let config = test_config(state_root);
    let skills = Arc::new(SkillManager::from_dirs(
        config.user_skills_dir().expect("user_skills_dir"),
        config.skills_dir().expect("skills_dir"),
    ));
    AppState {
        db: Arc::new(Database::new(&config.db_path()).expect("db")),
        config: config.clone(),
        config_path: None,
        llm_override: Some(Arc::from(llm)),
        channels: Arc::new(ChannelRegistry::new()),
        skills: Arc::clone(&skills),
        tools: Arc::new(ToolRegistry::new(&config, skills)),
        mcp_manager: None,
        assets: Arc::new(AssetStore::new(&config.assets_dir()).expect("assets")),
        soul_agents: Arc::new(crate::soul_agents::SoulAgentsLoader::new(&config)),
        llm_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

/// テスト用 CLI SurfaceContext。
pub(crate) fn cli_context(session: &str) -> crate::agent_loop::SurfaceContext {
    crate::agent_loop::SurfaceContext {
        channel: "cli".to_string(),
        surface_user: "local_user".to_string(),
        surface_thread: session.to_string(),
        chat_type: "cli".to_string(),
        agent_id: "default".to_string(),
    }
}

/// テスト用 ToolExecutionContext。
pub(crate) fn test_tool_context() -> crate::tools::ToolExecutionContext {
    crate::tools::ToolExecutionContext {
        chat_id: 1,
        channel: "cli".to_string(),
        surface_thread: "demo".to_string(),
        chat_type: "cli".to_string(),
    }
}

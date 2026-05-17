//! アプリケーション設定の読み込みと検証。
//!
//! YAML 設定ファイルから provider ベースの設定を構築し、
//! channel ごとの override を実効 LLM 設定へ解決する。

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use secrecy::ExposeSecret;
use serial_test::serial;

use super::{Config, default_state_root, default_workspace_dir};
use crate::error::ConfigError;
use crate::test_env::EnvVarGuard;

fn write_config(temp_dir: &tempfile::TempDir, body: &str) -> PathBuf {
    let file_path = temp_dir.path().join("egopulse.config.yaml");
    let mut file = std::fs::File::create(&file_path).expect("create config");
    writeln!(file, "{body}").expect("write config");
    file_path
}

fn sample_config() -> &'static str {
    r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
    models:
      gpt-4o-mini: {}
      gpt-5: {}
  local:
    label: Local OpenAI-compatible
    base_url: http://127.0.0.1:1234/v1
    default_model: qwen2.5
channels:
  web:
    enabled: true
    auth_token: web-secret
  discord:
    enabled: false"#
}

#[test]
#[serial]
fn home_directory_unresolved_error_displays_correctly() {
    let error = ConfigError::HomeDirectoryUnresolved;
    let message = error.to_string();
    assert!(message.contains("home_directory_unresolved"));
}

#[test]
#[serial]
fn loads_provider_based_config() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());

    let config = Config::load(Some(&file_path)).expect("load config");

    assert_eq!(config.default_provider.as_str(), "openai");
    assert_eq!(config.global_provider().label, "OpenAI");
    assert_eq!(
        PathBuf::from(&config.state_root),
        default_state_root().unwrap()
    );
    assert_eq!(
        config.workspace_dir().unwrap(),
        default_workspace_dir().unwrap()
    );
    assert_eq!(
        config.skills_dir().unwrap(),
        default_state_root().unwrap().join("skills")
    );
    assert!(config.web_enabled());
    assert_eq!(config.web_auth_token(), Some("web-secret"));

    let web_llm = config
        .resolve_llm_for_agent_channel(&config.default_agent, "web")
        .expect("web llm");
    assert_eq!(web_llm.provider, "openai");
    assert_eq!(web_llm.model, "gpt-4o-mini");
    assert_eq!(web_llm.base_url, "https://api.openai.com/v1");
    assert_eq!(
        web_llm.api_key.as_ref().map(ExposeSecret::expose_secret),
        Some("sk-openai")
    );
}

#[test]
#[serial]
fn allows_missing_api_key_for_local_provider() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: local
providers:
  local:
    label: Local
    base_url: http://127.0.0.1:1234/v1
    default_model: qwen2.5
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let config = Config::load(Some(&file_path)).expect("load local config");
    let resolved = config
        .resolve_llm_for_agent_channel(&config.default_agent, "web")
        .expect("resolved llm");
    assert!(resolved.api_key.is_none());
}

#[test]
#[serial]
fn rejects_missing_remote_api_key() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let error = Config::load(Some(&file_path)).expect_err("missing api key");
    assert!(matches!(
        error,
        ConfigError::MissingProviderApiKey { provider } if provider == "openai"
    ));
}

#[test]
#[serial]
fn rejects_unknown_agent_provider() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
agents:
  alice:
    label: Alice
    provider: missing"#,
    );

    let error = Config::load(Some(&file_path)).expect_err("invalid provider");
    assert!(
        matches!(&error, ConfigError::InvalidProviderReference { provider } if provider == "missing"),
        "expected InvalidProviderReference, got {error:?}"
    );
}

#[test]
#[serial]
fn load_allow_missing_api_key_accepts_incomplete_remote_provider() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let config = Config::load_allow_missing_api_key(Some(&file_path)).expect("allow missing key");
    assert!(
        config
            .resolve_llm_for_agent_channel(&config.default_agent, "web")
            .expect("resolved")
            .api_key
            .is_none()
    );
}

#[test]
#[serial]
fn default_model_in_yaml_overrides_provider_default() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_model: gpt-5
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");

    // config.default_model preserves the YAML-level override as Some
    assert_eq!(config.default_model, Some("gpt-5".to_string()));

    // resolve_global_llm uses config.default_model
    let global = config.resolve_global_llm();
    assert_eq!(global.model, "gpt-5");

    // channel without model override also falls back to config.default_model
    let web_llm = config
        .resolve_llm_for_agent_channel(&config.default_agent, "web")
        .expect("web llm");
    assert_eq!(web_llm.model, "gpt-5");
}

#[test]
#[serial]
fn default_model_falls_back_to_provider_default() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");

    assert_eq!(config.default_model, None);
    let global = config.resolve_global_llm();
    assert_eq!(global.model, "gpt-4o-mini");
}

#[test]
#[serial]
fn soul_path_returns_state_root_soul_md() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    assert_eq!(
        config.soul_path(),
        PathBuf::from(&config.state_root).join("SOUL.md")
    );
}

#[test]
#[serial]
fn agents_path_returns_state_root_agents_md() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    assert_eq!(
        config.agents_path(),
        PathBuf::from(&config.state_root).join("AGENTS.md")
    );
}

#[test]
#[serial]
fn groups_dir_returns_runtime_groups() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    assert_eq!(
        config.groups_dir(),
        PathBuf::from(&config.state_root)
            .join("runtime")
            .join("groups")
    );
}

#[test]
#[serial]
fn souls_dir_returns_state_root_souls() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    assert_eq!(
        config.souls_dir(),
        PathBuf::from(&config.state_root).join("souls")
    );
}

#[test]
#[serial]
fn channel_soul_path_reads_from_config() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
    soul_path: work"#,
    );
    let config = Config::load(Some(&file_path)).expect("load config");

    let web = config.channels.get("web").expect("web channel");
    assert_eq!(web.soul_path.as_deref(), Some("work"));
}

#[test]
#[serial]
fn channel_soul_path_none_when_unset() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    let web = config.channels.get("web").expect("web channel");
    assert!(web.soul_path.is_none());
}

#[test]
#[serial]
fn model_resolution_chain_agent_overrides_global() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_model: gpt-5
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
default_agent: alice
agents:
  alice:
    label: Alice
  bob:
    label: Bob
    model: gpt-4o"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");

    // agent.model (bob) overrides config.default_model
    let bob_llm = config
        .resolve_llm_for_agent_channel(&super::AgentId::new("bob"), "web")
        .expect("bob llm");
    assert_eq!(bob_llm.model, "gpt-4o");

    // agent without model → config.default_model
    let alice_llm = config
        .resolve_llm_for_agent_channel(&super::AgentId::new("alice"), "web")
        .expect("alice llm");
    assert_eq!(alice_llm.model, "gpt-5");
}

#[test]
fn provider_id_normalizes_case() {
    let id = super::ProviderId::new("OpenAI");
    assert_eq!(id.as_str(), "openai");
}

#[test]
fn channel_name_trims_whitespace() {
    let name = super::ChannelName::new(" Web ");
    assert_eq!(name.as_str(), "web");
}

#[test]
#[serial]
fn loads_agents_with_default_agent() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
default_agent: alice
agents:
  alice:
    label: Alice"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");

    assert_eq!(config.default_agent.as_str(), "alice");
    let alice = config.agents.get("alice").expect("alice agent");
    assert_eq!(alice.label, "Alice");
}

#[test]
#[serial]
fn default_agent_falls_back_to_default_when_missing() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");

    assert_eq!(config.default_agent.as_str(), "default");
    assert!(config.agents.contains_key("default"));
}

#[test]
#[serial]
fn rejects_default_agent_not_in_agents() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
default_agent: missing
agents:
  alice:
    label: Alice"#,
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");
    assert!(matches!(
        error,
        ConfigError::DefaultAgentNotFound { agent_id } if agent_id == "missing"
    ));
}

#[test]
#[serial]
fn rejects_agent_id_path_traversal() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());

    for bad_id in ["../etc", "/etc", "", "foo:bar"] {
        let yaml = format!(
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
default_agent: alice
agents:
  "{bad_id}":
    label: Bad
  alice:
    label: Alice"#
        );
        let file_path = write_config(&temp_dir, &yaml);
        let error = Config::load(Some(&file_path)).expect_err("should reject bad agent id");
        assert!(
            matches!(error, ConfigError::InvalidAgentId { .. }),
            "expected InvalidAgentId for '{bad_id}', got {error:?}"
        );
    }
}

#[test]
#[serial]
fn persists_agents_without_discord_config_surface() {
    use crate::config::persist::save_config_with_secrets;
    use crate::config::secret_ref::env_resolved_value;

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let path = temp_dir.path().join("egopulse.config.yaml");

    let mut agents = std::collections::HashMap::new();
    agents.insert(
        super::AgentId::new("alice"),
        super::AgentConfig {
            label: "Alice".to_string(),
            provider: Some("openai".to_string()),
            model: Some("gpt-5".to_string()),
            ..Default::default()
        },
    );
    agents.insert(
        super::AgentId::new("default"),
        super::AgentConfig {
            label: "Default Agent".to_string(),
            ..Default::default()
        },
    );

    let config = Config {
        default_provider: super::ProviderId::new("openai"),
        default_model: None,
        providers: std::collections::HashMap::from([(
            super::ProviderId::new("openai"),
            super::ProviderConfig {
                label: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: Some(env_resolved_value("OPENAI_API_KEY", "sk-test")),
                default_model: "gpt-5".to_string(),
                models: HashMap::from([("gpt-5".to_string(), super::ModelConfig::default())]),
            },
        )]),
        state_root: temp_dir.path().to_str().expect("path").to_string(),
        log_level: "info".to_string(),
        compaction_timeout_secs: 180,
        max_history_messages: 50,
        compact_keep_recent: 20,
        default_context_window_tokens: 32768,
        compaction_threshold_ratio: 0.80,
        compaction_target_ratio: 0.40,
        channels: std::collections::HashMap::new(),
        default_agent: super::AgentId::new("alice"),
        agents,
        sleep_batch: super::SleepBatchConfig::default(),
        pulse: super::PulseConfig::default(),
        web_fetch: super::web_fetch::WebFetchConfig::default(),
    };

    save_config_with_secrets(&config, &path).expect("save config");

    let yaml = std::fs::read_to_string(&path).expect("yaml");
    assert!(yaml.contains("default_agent: alice"));
    assert!(yaml.contains("label: Alice"));
    assert!(yaml.contains("provider: openai"));
    assert!(yaml.contains("model: gpt-5"));
    assert!(!yaml.contains("discord:"));
}

// --- Step 2: Agent LLM Resolution tests ---

fn agent_config() -> &'static str {
    r#"default_provider: openai
default_model: gpt-5
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
  local:
    label: Local
    base_url: http://127.0.0.1:1234/v1
    default_model: qwen2.5
channels:
  discord:
    enabled: true
default_agent: alice
agents:
  alice:
    label: Alice
  bob:
    label: Bob
    provider: openai
    model: gpt-5-mini
  carol:
    label: Carol
    model: custom-model"#
}

#[test]
#[serial]
fn resolve_llm_for_agent_channel_falls_back_to_default_provider() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, agent_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    let resolved = config
        .resolve_llm_for_agent_channel(&super::AgentId::new("alice"), "discord")
        .expect("resolve");

    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model, "gpt-5");
}

#[test]
#[serial]
fn resolve_llm_for_agent_channel_agent_provider_takes_priority() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, agent_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    let resolved = config
        .resolve_llm_for_agent_channel(&super::AgentId::new("bob"), "discord")
        .expect("resolve");

    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model, "gpt-5-mini");
    assert_eq!(resolved.base_url, "https://api.openai.com/v1");
}

#[test]
#[serial]
fn resolve_llm_for_agent_channel_agent_model_with_default_provider() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, agent_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    let resolved = config
        .resolve_llm_for_agent_channel(&super::AgentId::new("carol"), "discord")
        .expect("resolve");

    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model, "custom-model");
}

#[test]
#[serial]
fn resolve_llm_for_agent_channel_falls_back_to_defaults() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, agent_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    let resolved = config
        .resolve_llm_for_agent_channel(&super::AgentId::new("alice"), "web")
        .expect("resolve");

    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model, "gpt-5");
}

#[test]
#[serial]
fn resolve_llm_for_agent_channel_rejects_unknown_agent() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, agent_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    let error = config
        .resolve_llm_for_agent_channel(&super::AgentId::new("unknown"), "discord")
        .expect_err("should fail");

    assert!(
        matches!(error, ConfigError::AgentNotFound { ref agent_id } if agent_id == "unknown"),
        "expected AgentNotFound, got {error:?}"
    );
}

#[test]
#[serial]
fn resolve_llm_for_agent_channel_rejects_unknown_provider() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
default_agent: alice
agents:
  alice:
    label: Alice
    provider: nonexistent"#,
    );
    let error = Config::load(Some(&file_path)).expect_err("should fail");

    assert!(matches!(
        error,
        ConfigError::InvalidProviderReference { provider } if provider == "nonexistent"
    ));
}

#[test]
#[serial]
fn resolve_llm_for_default_agent_matches_resolve_llm() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, agent_config());
    let config = Config::load(Some(&file_path)).expect("load config");

    let via_agent = config
        .resolve_llm_for_agent_channel(&config.default_agent, "web")
        .expect("via agent");
    let via_resolve = config.resolve_global_llm();

    // default agent (alice) uses default_provider + default_model
    assert_eq!(via_agent.provider, via_resolve.provider);
    assert_eq!(via_agent.model, via_resolve.model);
}

// --- Step 1: Discord Agent Bot Config Helper tests ---

use crate::config::secret_ref::{env_resolved_value as lit_val, env_yaml_value as lit_yaml};

// --- Discord Bot Config tests ---

fn write_env(temp_dir: &tempfile::TempDir, contents: &str) {
    use std::io::Write as IoWrite;
    let env_path = temp_dir.path().join(".env");
    let mut f = std::fs::File::create(&env_path).expect("create .env");
    write!(f, "{contents}").expect("write .env");
}

fn bot_config_yml(bot_section: &str, discord_channels: Option<&str>) -> String {
    let channels_section = discord_channels
        .map(|s| format!("    channels:\n{s}\n"))
        .unwrap_or_default();
    format!(
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
default_agent: assistant
agents:
  assistant:
    label: Assistant
  reviewer:
    label: Reviewer
channels:
  discord:
    enabled: true
{bot_section}
{channels_section}"#
    )
}

#[test]
#[serial]
fn loads_discord_bots_with_default_agent() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=discord-bot-token-123\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant"#,
            Some(
                r#"      "111222333": {}
      "444555666":
        agents: [reviewer]"#,
            ),
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");

    let discord = config.channels.get("discord").expect("discord channel");
    let bots = discord.discord_bots.as_ref().expect("bots");
    assert_eq!(bots.len(), 1);

    let main_bot = bots.get("main").expect("main bot");
    assert_eq!(
        main_bot.token.as_ref().expect("token").value(),
        "discord-bot-token-123"
    );
    let channels = discord.discord_channels.as_ref().expect("channels");
    assert_eq!(channels.len(), 2);
    assert!(channels.contains_key(&111222333u64));
    assert!(channels.contains_key(&444555666u64));
    assert_eq!(
        channels.get(&444555666u64).map(|c| &c.agents),
        Some(&vec![super::AgentId::new("reviewer")])
    );
}

#[test]
#[serial]
fn discord_bots_validate_channel_agents_exist() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant"#,
            Some(
                r#"      "999":
            agents: [ghost_agent]"#,
            ),
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");

    assert!(
        matches!(
            error,
            ConfigError::DiscordBotChannelAgentNotFound { ref bot_id, channel_id: 999, ref agent_id }
                if bot_id == "discord" && agent_id == "ghost_agent"
        ),
        "expected DiscordBotChannelAgentNotFound, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Step 5: Multi-agent channel config validation
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn validation_rejects_multi_agent_with_single_agent() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant"#,
            Some(
                r#"      "100":
            agents: [assistant]
            multi_agent: true"#,
            ),
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");

    assert!(
        matches!(
            error,
            ConfigError::DiscordBotChannelMultiAgentMismatch {
                ref bot_id,
                channel_id: 100,
                ..
            } if bot_id == "discord"
        ),
        "expected DiscordBotChannelMultiAgentMismatch, got {error:?}"
    );
}

#[test]
#[serial]
fn validation_rejects_single_mode_with_multiple_agents() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant"#,
            Some(
                r#"      "200":
            agents: [assistant, reviewer]
            multi_agent: false"#,
            ),
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");

    assert!(
        matches!(
            error,
            ConfigError::DiscordBotChannelMultiAgentMismatch {
                ref bot_id,
                channel_id: 200,
                ..
            } if bot_id == "discord"
        ),
        "expected DiscordBotChannelMultiAgentMismatch, got {error:?}"
    );
}

#[test]
#[serial]
fn validation_accepts_single_agent() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant"#,
            Some(
                r#"      "300":
            agents: [assistant]"#,
            ),
        ),
    );

    let config = Config::load(Some(&file_path)).expect("should succeed");

    let discord = config.channels.get("discord").expect("discord channel");
    let bots = discord.discord_bots.as_ref().expect("bots");
    let _bot = bots.get(&super::BotId::new("main")).expect("main bot");
    let ch = discord
        .discord_channels
        .as_ref()
        .expect("channels")
        .get(&300)
        .expect("ch 300");
    assert_eq!(ch.agents, vec![super::AgentId::new("assistant")]);
    assert!(!ch.multi_agent);
}

#[test]
#[serial]
fn validation_accepts_multi_agent() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant"#,
            Some(
                r#"      "400":
            agents: [assistant, reviewer]
            multi_agent: true"#,
            ),
        ),
    );

    let config = Config::load(Some(&file_path)).expect("should succeed");

    let discord = config.channels.get("discord").expect("discord channel");
    let bots = discord.discord_bots.as_ref().expect("bots");
    let _bot = bots.get(&super::BotId::new("main")).expect("main bot");
    let ch = discord
        .discord_channels
        .as_ref()
        .expect("channels")
        .get(&400)
        .expect("ch 400");
    assert_eq!(
        ch.agents,
        vec![
            super::AgentId::new("assistant"),
            super::AgentId::new("reviewer")
        ]
    );
    assert!(ch.multi_agent);
}

#[test]
#[serial]
fn validation_agents_reference_must_exist() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant"#,
            Some(
                r#"      "500":
            agents: [unknown_agent]"#,
            ),
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");

    assert!(
        matches!(
            error,
            ConfigError::DiscordBotChannelAgentNotFound {
                ref bot_id,
                channel_id: 500,
                ref agent_id,
            } if bot_id == "discord" && agent_id == "unknown_agent"
        ),
        "expected DiscordBotChannelAgentNotFound, got {error:?}"
    );
}

#[test]
#[serial]
fn validation_empty_agents_after_normalization() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant"#,
            Some(
                r#"      "600":
            agents: []
            multi_agent: false"#,
            ),
        ),
    );

    let config = Config::load(Some(&file_path)).expect("should succeed after normalization");

    let discord = config.channels.get("discord").expect("discord channel");
    let bots = discord.discord_bots.as_ref().expect("bots");
    let _bot = bots.get(&super::BotId::new("main")).expect("main bot");
    let ch = discord
        .discord_channels
        .as_ref()
        .expect("channels")
        .get(&600)
        .expect("ch 600");
    assert_eq!(ch.agents, vec![super::AgentId::new("assistant")]);
}

#[test]
#[serial]
fn validation_rejects_multi_agent_with_empty_agents_after_defaulting() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant"#,
            Some(
                r#"      "700":
            agents: []
            multi_agent: true"#,
            ),
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");

    assert!(
        matches!(
            error,
            ConfigError::DiscordBotChannelMultiAgentMismatch {
                ref bot_id,
                channel_id: 700,
                ..
            } if bot_id == "discord"
        ),
        "expected DiscordBotChannelMultiAgentMismatch, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Step 6: AgentConfig.discord_bot
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn parse_agent_config_with_discord_bot() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_DISCORD_TOKEN
                default_agent: assistant"#,
            None,
        )
        .replace(
            "agents:\n  assistant:",
            "agents:\n  assistant:\n    discord_bot: main\n  reviewer:",
        ),
    );

    let config = Config::load(Some(&file_path)).expect("should succeed");
    let agent = config
        .agents
        .get(&super::AgentId::new("assistant"))
        .expect("assistant agent");
    assert_eq!(agent.discord_bot.as_ref(), Some(&super::BotId::new("main")));
}

#[test]
#[serial]
fn parse_agent_config_without_discord_bot() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_DISCORD_TOKEN
                default_agent: assistant"#,
            None,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("should succeed");
    let agent = config
        .agents
        .get(&super::AgentId::new("assistant"))
        .expect("assistant agent");
    assert!(agent.discord_bot.is_none());
}

#[test]
#[serial]
fn validation_discord_bot_must_exist() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_DISCORD_TOKEN
                default_agent: assistant"#,
            None,
        )
        .replace(
            "agents:\n  assistant:",
            "agents:\n  assistant:\n    discord_bot: nonexistent_bot\n  reviewer:",
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");

    assert!(
        matches!(
            error,
            ConfigError::AgentDiscordBotNotFound { ref agent_id, ref bot_id }
                if agent_id == "assistant" && bot_id == "nonexistent_bot"
        ),
        "expected AgentDiscordBotNotFound, got {error:?}"
    );
}

#[test]
#[serial]
fn validation_discord_bot_null_is_ok() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_DISCORD_TOKEN
                default_agent: assistant"#,
            None,
        )
        .replace(
            "agents:\n  assistant:",
            "agents:\n  assistant:\n    discord_bot: null\n  reviewer:",
        ),
    );

    let config = Config::load(Some(&file_path)).expect("should succeed");
    let agent = config
        .agents
        .get(&super::AgentId::new("assistant"))
        .expect("assistant agent");
    assert!(agent.discord_bot.is_none());
}

#[test]
#[serial]
fn validation_agent_discord_bot_checked_even_without_discord_channel() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());

    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
default_agent: assistant
agents:
  assistant:
    label: Assistant
    discord_bot: nonexistent_bot"#,
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");

    assert!(
        matches!(
            error,
            ConfigError::AgentDiscordBotNotFound { ref agent_id, ref bot_id }
                if agent_id == "assistant" && bot_id == "nonexistent_bot"
        ),
        "expected AgentDiscordBotNotFound, got {error:?}"
    );
}

#[test]
#[serial]
fn discord_bots_preserve_secret_refs_on_save() {
    use crate::config::persist::save_config_with_secrets;

    // Arrange
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let path = temp_dir.path().join("egopulse.config.yaml");

    let mut agents = HashMap::new();
    agents.insert(
        super::AgentId::new("assistant"),
        super::AgentConfig {
            label: "Assistant".to_string(),
            ..Default::default()
        },
    );

    let mut discord_bots = HashMap::new();
    discord_bots.insert(
        super::BotId::new("main"),
        super::DiscordBotConfig {
            token: Some(lit_val("DISCORD_BOT_TOKEN", "secret-bot-token")),
            file_token: Some(lit_yaml("DISCORD_BOT_TOKEN")),
        },
    );

    let mut channels = HashMap::new();
    channels.insert(
        super::ChannelName::new("discord"),
        super::ChannelConfig {
            enabled: Some(true),
            discord_bots: Some(discord_bots),
            discord_channels: Some(
                [(123456u64, super::DiscordChannelConfig::default())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        },
    );

    let config = Config {
        default_provider: super::ProviderId::new("openai"),
        default_model: None,
        providers: HashMap::from([(
            super::ProviderId::new("openai"),
            super::ProviderConfig {
                label: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: Some(lit_val("OPENAI_API_KEY", "sk-test")),
                default_model: "gpt-5".to_string(),
                models: HashMap::from([("gpt-5".to_string(), super::ModelConfig::default())]),
            },
        )]),
        state_root: temp_dir.path().to_str().expect("path").to_string(),
        log_level: "info".to_string(),
        compaction_timeout_secs: 180,
        max_history_messages: 50,
        compact_keep_recent: 20,
        default_context_window_tokens: 32768,
        compaction_threshold_ratio: 0.80,
        compaction_target_ratio: 0.40,
        channels,
        default_agent: super::AgentId::new("assistant"),
        agents,
        sleep_batch: super::SleepBatchConfig::default(),
        pulse: super::PulseConfig::default(),
        web_fetch: super::web_fetch::WebFetchConfig::default(),
    };

    // Act
    save_config_with_secrets(&config, &path).expect("save config");

    // Assert - YAML has SecretRef, not plain token
    let yaml = std::fs::read_to_string(&path).expect("yaml");
    assert!(yaml.contains("source: env"));
    assert!(yaml.contains("id: DISCORD_BOT_TOKEN"));
    assert!(!yaml.contains("secret-bot-token"));

    // Assert - .env has the actual token
    let dotenv = std::fs::read_to_string(temp_dir.path().join(".env")).expect(".env");
    assert!(dotenv.contains("DISCORD_BOT_TOKEN=secret-bot-token"));
}

// --- Step 2: Discord Bot Resolver tests ---

#[test]
#[serial]
fn discord_bots_returns_only_channel_bots_with_token() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_TOKEN=bot-token\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_TOKEN
                default_agent: assistant
              no_token_bot:
                default_agent: reviewer"#,
            None,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let bots = config.discord_bots();

    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0].bot_id.as_str(), "main");
    assert_eq!(bots[0].token, "bot-token");
}

#[test]
#[serial]
fn discord_bots_sort_by_bot_id() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "T1=t1\nT2=t2\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              zeta:
                token:
                  source: env
                  id: T1
                default_agent: assistant
              alpha:
                token:
                  source: env
                  id: T2
                default_agent: assistant"#,
            None,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let bots = config.discord_bots();

    assert_eq!(bots.len(), 2);
    assert_eq!(bots[0].bot_id.as_str(), "alpha");
    assert_eq!(bots[1].bot_id.as_str(), "zeta");
}

#[test]
#[serial]
fn discord_bots_disabled_channel_returns_empty() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
default_agent: assistant
agents:
  assistant:
    label: Assistant
channels:
  discord:
    enabled: false
    bots:
      main:
        token:
          source: env
          id: MY_TOKEN
        default_agent: assistant"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let bots = config.discord_bots();

    assert!(bots.is_empty());
}

#[test]
#[serial]
fn discord_bot_channels_defaults_to_none() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_TOKEN=token\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_TOKEN
                default_agent: assistant"#,
            None,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let bots = config.discord_bots();

    assert_eq!(bots.len(), 1);
    assert!(config.discord_channels().is_empty());
}

#[test]
#[serial]
fn discord_bot_channel_agents_are_preserved() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_TOKEN=token\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_TOKEN
                default_agent: assistant"#,
            Some(
                r#"      "42":
          agents: [reviewer]"#,
            ),
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let bots = config.discord_bots();

    assert_eq!(bots.len(), 1);
    let channels = config.discord_channels();
    assert_eq!(
        channels.get(&42).map(|c| &c.agents),
        Some(&vec![super::AgentId::new("reviewer")])
    );
}

#[test]
#[serial]
fn loads_openai_codex_without_api_key() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai-codex
providers:
  openai-codex:
    label: OpenAI Codex
    default_model: gpt-5.3-codex
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );
    let config = Config::load(Some(&file_path)).expect("should load openai-codex without api_key");
    let resolved = config
        .resolve_llm_for_agent_channel(&config.default_agent, "web")
        .expect("web llm");
    assert_eq!(resolved.provider, "openai-codex");
    assert!(resolved.api_key.is_none());
}

#[test]
#[serial]
fn openai_codex_gets_default_base_url() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai-codex
providers:
  openai-codex:
    label: OpenAI Codex
    default_model: gpt-5.3-codex
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );
    let config = Config::load(Some(&file_path)).expect("load");
    let provider = config
        .providers
        .get(&super::ProviderId::new("openai-codex"))
        .expect("provider");
    assert_eq!(provider.base_url, "https://chatgpt.com/backend-api/codex");
}

#[test]
#[serial]
fn openai_codex_custom_base_url_preserved() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai-codex
providers:
  openai-codex:
    label: OpenAI Codex
    base_url: https://custom.proxy.example.com/codex
    default_model: gpt-5.3-codex
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );
    let config = Config::load(Some(&file_path)).expect("load");
    let provider = config
        .providers
        .get(&super::ProviderId::new("openai-codex"))
        .expect("provider");
    assert_eq!(provider.base_url, "https://custom.proxy.example.com/codex");
}

// --- Structured channel/chat config tests ---

#[test]
#[serial]
fn discord_channels_parses_null_value() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_TOKEN
                default_agent: assistant"#,
            Some(
                r#"      "123":
"#,
            ),
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let discord = config.channels.get("discord").expect("discord");
    let channels = discord.discord_channels.as_ref().expect("channels");
    let ch = channels.get(&123u64).expect("channel 123");
    assert!(!ch.require_mention);
    assert_eq!(ch.agents, vec![super::AgentId::new("assistant")]);
    assert!(!ch.multi_agent);
}

#[test]
#[serial]
fn discord_channels_parses_require_mention() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_TOKEN
                default_agent: assistant"#,
            Some(
                r#"      "123":
          require_mention: true"#,
            ),
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let discord = config.channels.get("discord").expect("discord");
    let ch = discord
        .discord_channels
        .as_ref()
        .expect("channels")
        .get(&123u64)
        .expect("channel");
    assert!(ch.require_mention);
}

#[test]
#[serial]
fn discord_channels_parses_agent_override() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_TOKEN
                default_agent: assistant"#,
            Some(
                r#"      "123":
          agents: [reviewer]"#,
            ),
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let discord = config.channels.get("discord").expect("discord");
    let ch = discord
        .discord_channels
        .as_ref()
        .expect("channels")
        .get(&123u64)
        .expect("channel");
    assert_eq!(ch.agents, vec![super::AgentId::new("reviewer")]);
}

#[test]
#[serial]
fn discord_channels_empty_means_no_guild_allowed() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_TOKEN
                default_agent: assistant"#,
            None,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");

    let discord = config.channels.get("discord").expect("discord");
    assert!(discord.discord_channels.is_none());

    let bots = config.discord_bots();
    assert_eq!(bots.len(), 1);
    assert!(config.discord_channels().is_empty());
}

#[test]
#[serial]
fn telegram_chats_parses_null_value() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
default_agent: default
agents:
  default:
    label: Default
channels:
  telegram:
    enabled: true
    bot_token: test-token
    chats:
      "123":"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let telegram = config.channels.get("telegram").expect("telegram");
    let chats = telegram.chats.as_ref().expect("chats");
    let chat = chats.get(&123i64).expect("chat 123");
    assert!(!chat.require_mention);
}

#[test]
#[serial]
fn telegram_chats_parses_require_mention() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
default_agent: default
agents:
  default:
    label: Default
channels:
  telegram:
    enabled: true
    bot_token: test-token
    chats:
      "456":
        require_mention: true"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let telegram = config.channels.get("telegram").expect("telegram");
    let chat = telegram
        .chats
        .as_ref()
        .expect("chats")
        .get(&456i64)
        .expect("chat");
    assert!(chat.require_mention);
}

#[test]
#[serial]
fn discord_channels_invalid_key_not_u64() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    write_env(&temp_dir, "MY_TOKEN=tok\n");
    let file_path = write_config(
        &temp_dir,
        &bot_config_yml(
            r#"    bots:
              main:
                token:
                  source: env
                  id: MY_TOKEN
                default_agent: assistant"#,
            Some(r#"      "not_a_number": {}"#),
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");
    assert!(
        matches!(
            error,
            ConfigError::InvalidChannelsKey { ref key }
                if key == "not_a_number"
        ),
        "expected InvalidChannelsKey, got {error:?}"
    );
}

#[test]
#[serial]
fn telegram_chats_invalid_key_not_i64() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
default_agent: default
agents:
  default:
    label: Default
channels:
  telegram:
    enabled: true
    bot_token: test-token
    chats:
      "not_a_number": {}"#,
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");
    assert!(
        matches!(
            error,
            ConfigError::InvalidChatsKey { ref key } if key == "not_a_number"
        ),
        "expected InvalidChatsKey, got {error:?}"
    );
}

// --- Safety Compaction config tests ---

#[test]
#[serial]
fn loads_provider_models_with_context_windows() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_context_window_tokens: 32768
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-test
    default_model: gpt-5
    models:
      gpt-5:
        context_window_tokens: 200000
      gpt-4o-mini:
        context_window_tokens: 128000
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let provider = config.providers.get("openai").expect("openai provider");

    let gpt5 = provider.models.get("gpt-5").expect("gpt-5 model");
    assert_eq!(gpt5.context_window_tokens, Some(200000));

    let mini = provider
        .models
        .get("gpt-4o-mini")
        .expect("gpt-4o-mini model");
    assert_eq!(mini.context_window_tokens, Some(128000));
}

#[test]
#[serial]
fn uses_default_context_window_when_model_context_missing() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_context_window_tokens: 32768
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-test
    default_model: gpt-4o-mini
    models:
      gpt-4o-mini: {}
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");

    // Model has no explicit context_window_tokens → falls back to default
    assert_eq!(
        config.resolve_context_window_tokens(&super::ProviderId::new("openai"), "gpt-4o-mini"),
        32768
    );
}

#[test]
#[serial]
fn loads_compaction_ratios_from_top_level() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_context_window_tokens: 65536
compaction_threshold_ratio: 0.90
compaction_target_ratio: 0.30
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-test
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    assert!((config.compaction_threshold_ratio - 0.90).abs() < f64::EPSILON);
    assert!((config.compaction_target_ratio - 0.30).abs() < f64::EPSILON);
}

#[test]
#[serial]
fn defaults_compaction_ratios_to_issue_values() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_context_window_tokens: 32768
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-test
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    assert!((config.compaction_threshold_ratio - 0.80).abs() < f64::EPSILON);
    assert!((config.compaction_target_ratio - 0.40).abs() < f64::EPSILON);
}

#[test]
#[serial]
fn rejects_invalid_compaction_ratios() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());

    let cases = [
        // (threshold, target, description)
        (0.0, 0.40, "zero threshold"),
        (1.01, 0.40, "threshold over 1.0"),
        (0.80, 0.0, "zero target"),
        (0.80, 1.01, "target over 1.0"),
        (0.50, 0.50, "target equals threshold"),
        (0.40, 0.60, "target greater than threshold"),
    ];

    for (threshold, target, desc) in cases {
        let yaml = format!(
            r#"default_provider: openai
default_context_window_tokens: 32768
compaction_threshold_ratio: {threshold}
compaction_target_ratio: {target}
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-test
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#
        );
        let file_path = write_config(&temp_dir, &yaml);
        let error = Config::load(Some(&file_path)).expect_err(desc);
        assert!(
            matches!(error, ConfigError::InvalidCompactionConfig(_)),
            "{desc}: expected InvalidCompactionConfig, got {error:?}"
        );
    }
}

#[test]
#[serial]
fn rejects_zero_context_window_tokens() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());

    // default_context_window_tokens = 0
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_context_window_tokens: 0
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-test
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );
    let error = Config::load(Some(&file_path)).expect_err("zero default");
    assert!(
        matches!(error, ConfigError::InvalidCompactionConfig(_)),
        "expected InvalidCompactionConfig for zero default, got {error:?}"
    );

    // model context_window_tokens = 0
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_context_window_tokens: 32768
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-test
    default_model: gpt-4o-mini
    models:
      gpt-4o-mini:
        context_window_tokens: 0
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );
    let error = Config::load(Some(&file_path)).expect_err("zero model context");
    assert!(
        matches!(error, ConfigError::InvalidCompactionConfig(_)),
        "expected InvalidCompactionConfig for zero model context, got {error:?}"
    );
}

#[test]
#[serial]
fn rejects_unsafe_default_context_window_tokens() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_context_window_tokens: 2000000
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-test
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
    );

    let error = Config::load(Some(&file_path)).expect_err("unsafe default");
    assert!(
        matches!(error, ConfigError::InvalidCompactionConfig(_)),
        "expected InvalidCompactionConfig for unsafe default, got {error:?}"
    );
}

#[test]
#[serial]
fn persists_provider_model_contexts_without_secret_leak() {
    use crate::config::ModelConfig;
    use crate::config::persist::save_config_with_secrets;
    use crate::config::secret_ref::env_resolved_value;

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let path = temp_dir.path().join("egopulse.config.yaml");

    let config = Config {
        default_provider: super::ProviderId::new("openai"),
        default_model: None,
        providers: HashMap::from([(
            super::ProviderId::new("openai"),
            super::ProviderConfig {
                label: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: Some(env_resolved_value("OPENAI_API_KEY", "sk-secret-key-12345")),
                default_model: "gpt-5".to_string(),
                models: HashMap::from([
                    (
                        "gpt-5".to_string(),
                        ModelConfig {
                            context_window_tokens: Some(200000),
                        },
                    ),
                    ("gpt-4o-mini".to_string(), ModelConfig::default()),
                ]),
            },
        )]),
        state_root: temp_dir.path().to_str().expect("path").to_string(),
        log_level: "info".to_string(),
        compaction_timeout_secs: 180,
        max_history_messages: 50,
        compact_keep_recent: 20,
        default_context_window_tokens: 32768,
        compaction_threshold_ratio: 0.80,
        compaction_target_ratio: 0.40,
        channels: HashMap::new(),
        default_agent: super::AgentId::new("default"),
        agents: HashMap::from([(
            super::AgentId::new("default"),
            super::AgentConfig {
                label: "Default Agent".to_string(),
                ..Default::default()
            },
        )]),
        sleep_batch: super::SleepBatchConfig::default(),
        pulse: super::PulseConfig::default(),
        web_fetch: super::web_fetch::WebFetchConfig::default(),
    };

    save_config_with_secrets(&config, &path).expect("save config");

    let yaml = std::fs::read_to_string(&path).expect("yaml");
    // Model context_window_tokens should be present
    assert!(yaml.contains("context_window_tokens: 200000"));
    // Secret must NOT appear in YAML
    assert!(!yaml.contains("sk-secret-key-12345"));
    // SecretRef should be used instead
    assert!(yaml.contains("source: env"));
    assert!(yaml.contains("OPENAI_API_KEY"));
}

// --- Sleep Batch config tests ---

#[test]
#[serial]
fn loads_sleep_batch_model() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
sleep_batch:
  model: deepseek-chat-v3"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    assert_eq!(
        config.sleep_batch.model.as_deref(),
        Some("deepseek-chat-v3")
    );
}

#[test]
#[serial]
fn loads_sleep_batch_provider() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
  deepseek:
    label: DeepSeek
    base_url: https://api.deepseek.com/v1
    api_key: sk-deepseek
    default_model: deepseek-chat
channels:
  web:
    enabled: true
    auth_token: web-secret
sleep_batch:
  provider: deepseek"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    assert_eq!(
        config.sleep_batch.provider.as_ref().map(|p| p.as_str()),
        Some("deepseek")
    );
}

#[test]
#[serial]
fn sleep_batch_model_defaults_to_none() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());
    let config = Config::load(Some(&file_path)).expect("load config");
    assert!(config.sleep_batch.model.is_none());
}

#[test]
#[serial]
fn sleep_batch_provider_defaults_to_none() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());
    let config = Config::load(Some(&file_path)).expect("load config");
    assert!(config.sleep_batch.provider.is_none());
}

#[test]
#[serial]
fn resolve_sleep_batch_llm_uses_provider_when_set() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
  deepseek:
    label: DeepSeek
    base_url: https://api.deepseek.com/v1
    api_key: sk-deepseek
    default_model: deepseek-chat
channels:
  web:
    enabled: true
    auth_token: web-secret
sleep_batch:
  provider: deepseek"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let resolved = config.resolve_sleep_batch_llm().expect("resolve");
    assert_eq!(resolved.provider, "deepseek");
    assert_eq!(resolved.model, "deepseek-chat");
}

#[test]
#[serial]
fn resolve_sleep_batch_llm_uses_model_when_set() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
default_model: gpt-5
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
sleep_batch:
  model: gpt-4o"#,
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let resolved = config.resolve_sleep_batch_llm().expect("resolve");
    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.model, "gpt-4o");
}

#[test]
#[serial]
fn resolve_sleep_batch_llm_falls_back_to_global_default_model() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());
    let config = Config::load(Some(&file_path)).expect("load config");
    let resolved = config.resolve_sleep_batch_llm().expect("resolve");
    assert_eq!(resolved.provider, "openai");
    // sample_config has no default_model, so falls back to provider.default_model
    assert_eq!(resolved.model, "gpt-4o-mini");
}

#[test]
#[serial]
fn rejects_unknown_sleep_batch_provider() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
sleep_batch:
  provider: nonexistent"#,
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");
    assert!(
        matches!(error, ConfigError::InvalidProviderReference { ref provider } if provider == "nonexistent"),
        "expected InvalidProviderReference, got {error:?}"
    );
}

#[test]
#[serial]
fn persist_preserves_sleep_batch_config() {
    use crate::config::persist::save_config_with_secrets;

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let path = temp_dir.path().join("egopulse.config.yaml");

    let mut providers = HashMap::new();
    providers.insert(
        super::ProviderId::new("openai"),
        super::ProviderConfig {
            label: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: Some(super::secret_ref::ResolvedValue::Literal(
                "sk-test".to_string(),
            )),
            default_model: "gpt-4o-mini".to_string(),
            models: HashMap::new(),
        },
    );
    providers.insert(
        super::ProviderId::new("deepseek"),
        super::ProviderConfig {
            label: "DeepSeek".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key: Some(super::secret_ref::ResolvedValue::Literal(
                "sk-ds".to_string(),
            )),
            default_model: "deepseek-chat".to_string(),
            models: HashMap::new(),
        },
    );

    let config = Config {
        default_provider: super::ProviderId::new("openai"),
        default_model: None,
        providers,
        state_root: temp_dir.path().to_str().expect("path").to_string(),
        log_level: "info".to_string(),
        compaction_timeout_secs: 180,
        max_history_messages: 50,
        compact_keep_recent: 20,
        default_context_window_tokens: 32768,
        compaction_threshold_ratio: 0.80,
        compaction_target_ratio: 0.40,
        channels: HashMap::new(),
        default_agent: super::AgentId::new("default"),
        agents: HashMap::from([(
            super::AgentId::new("default"),
            super::AgentConfig {
                label: "Default Agent".to_string(),
                ..Default::default()
            },
        )]),
        sleep_batch: super::SleepBatchConfig {
            provider: Some(super::ProviderId::new("deepseek")),
            model: Some("deepseek-chat-v3".to_string()),
            ..Default::default()
        },
        pulse: super::PulseConfig::default(),
        web_fetch: super::web_fetch::WebFetchConfig::default(),
    };

    save_config_with_secrets(&config, &path).expect("save config");

    let yaml = std::fs::read_to_string(&path).expect("yaml");
    assert!(yaml.contains("sleep_batch:"));
    assert!(yaml.contains("provider: deepseek"));
    assert!(yaml.contains("model: deepseek-chat-v3"));
}

// --- Step 2: Sleep Batch Scheduler Config tests ---

fn sleep_batch_scheduler_yml(sleep_batch_section: &str) -> String {
    format!(
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
default_agent: alice
agents:
  alice:
    label: Alice
  bob:
    label: Bob
  carol:
    label: Carol
sleep_batch:
{sleep_batch_section}"#
    )
}

#[test]
#[serial]
fn loads_sleep_batch_enabled() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo""#,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    assert!(config.sleep_batch.enabled);
}

#[test]
#[serial]
fn sleep_batch_enabled_defaults_to_false() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());

    let config = Config::load(Some(&file_path)).expect("load config");
    assert!(!config.sleep_batch.enabled);
}

#[test]
#[serial]
fn loads_sleep_batch_schedule() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo""#,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    assert_eq!(config.sleep_batch.schedule.as_deref(), Some("04:00"));
}

#[test]
#[serial]
fn loads_sleep_batch_timezone() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo""#,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    assert_eq!(config.sleep_batch.timezone.as_deref(), Some("Asia/Tokyo"));
}

#[test]
#[serial]
fn sleep_batch_enabled_requires_schedule() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  timezone: "Asia/Tokyo""#,
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");
    assert!(
        matches!(error, ConfigError::SleepBatchEnabledRequiresSchedule),
        "expected SleepBatchEnabledRequiresSchedule, got {error:?}"
    );
}

#[test]
#[serial]
fn sleep_batch_enabled_requires_timezone() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00""#,
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");
    assert!(
        matches!(error, ConfigError::SleepBatchEnabledRequiresTimezone),
        "expected SleepBatchEnabledRequiresTimezone, got {error:?}"
    );
}

#[test]
#[serial]
fn sleep_batch_disabled_allows_missing_schedule_timezone() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, &sleep_batch_scheduler_yml(r#"  enabled: false"#));

    let config = Config::load(Some(&file_path)).expect("load config");
    assert!(!config.sleep_batch.enabled);
    assert!(config.sleep_batch.schedule.is_none());
    assert!(config.sleep_batch.timezone.is_none());
}

#[test]
#[serial]
fn loads_sleep_batch_agents() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo"
  agents:
    - alice
    - bob"#,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let agents = config.sleep_batch.agents.expect("agents");
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].as_str(), "alice");
    assert_eq!(agents[1].as_str(), "bob");
}

#[test]
#[serial]
fn sleep_batch_agents_defaults_to_none_when_unset() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo""#,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    assert!(config.sleep_batch.agents.is_none());
}

#[test]
#[serial]
fn sleep_batch_agents_empty_means_no_agents() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo"
  agents: []"#,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let agents = config.sleep_batch.agents.expect("agents");
    assert!(agents.is_empty());
}

#[test]
#[serial]
fn sleep_batch_agent_order_puts_default_first() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo"
  agents:
    - carol
    - alice
    - bob"#,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let agents = config.sleep_batch.agents.expect("agents");
    assert_eq!(agents[0].as_str(), "alice");
    assert_eq!(agents[1].as_str(), "bob");
    assert_eq!(agents[2].as_str(), "carol");
}

#[test]
#[serial]
fn sleep_batch_agents_deduplicates_duplicates() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo"
  agents:
    - alice
    - bob
    - alice"#,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    let agents = config.sleep_batch.agents.expect("agents");
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].as_str(), "alice");
    assert_eq!(agents[1].as_str(), "bob");
}

#[test]
#[serial]
fn loads_sleep_batch_retry_config() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo"
  retry:
    max_attempts: 5
    interval_minutes: 10"#,
        ),
    );

    let config = Config::load(Some(&file_path)).expect("load config");
    assert_eq!(config.sleep_batch.retry_max_attempts, 5);
    assert_eq!(config.sleep_batch.retry_interval_minutes, 10);
}

#[test]
#[serial]
fn rejects_unknown_sleep_batch_agent() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Asia/Tokyo"
  agents:
    - nonexistent"#,
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");
    assert!(
        matches!(error, ConfigError::SleepBatchUnknownAgent { ref agent_id } if agent_id == "nonexistent"),
        "expected SleepBatchUnknownAgent, got {error:?}"
    );
}

#[test]
#[serial]
fn rejects_invalid_sleep_batch_schedule() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "25:00"
  timezone: "Asia/Tokyo""#,
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");
    assert!(
        matches!(error, ConfigError::SleepBatchInvalidSchedule { ref schedule } if schedule == "25:00"),
        "expected SleepBatchInvalidSchedule, got {error:?}"
    );
}

#[test]
#[serial]
fn rejects_invalid_sleep_batch_timezone() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &sleep_batch_scheduler_yml(
            r#"  enabled: true
  schedule: "04:00"
  timezone: "Invalid/Zone""#,
        ),
    );

    let error = Config::load(Some(&file_path)).expect_err("should fail");
    assert!(
        matches!(error, ConfigError::SleepBatchInvalidTimezone { ref timezone } if timezone == "Invalid/Zone"),
        "expected SleepBatchInvalidTimezone, got {error:?}"
    );
}

#[test]
#[serial]
fn persist_preserves_sleep_batch_scheduler_config() {
    use crate::config::persist::save_config_with_secrets;

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let path = temp_dir.path().join("egopulse.config.yaml");

    let mut providers = HashMap::new();
    providers.insert(
        super::ProviderId::new("openai"),
        super::ProviderConfig {
            label: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: Some(super::secret_ref::ResolvedValue::Literal(
                "sk-test".to_string(),
            )),
            default_model: "gpt-4o-mini".to_string(),
            models: HashMap::new(),
        },
    );

    let mut agents = HashMap::new();
    agents.insert(
        super::AgentId::new("alice"),
        super::AgentConfig {
            label: "Alice".to_string(),
            ..Default::default()
        },
    );

    let config = Config {
        default_provider: super::ProviderId::new("openai"),
        default_model: None,
        providers,
        state_root: temp_dir.path().to_str().expect("path").to_string(),
        log_level: "info".to_string(),
        compaction_timeout_secs: 180,
        max_history_messages: 50,
        compact_keep_recent: 20,
        default_context_window_tokens: 32768,
        compaction_threshold_ratio: 0.80,
        compaction_target_ratio: 0.40,
        channels: HashMap::new(),
        default_agent: super::AgentId::new("alice"),
        agents,
        sleep_batch: super::SleepBatchConfig {
            enabled: true,
            schedule: Some("04:00".to_string()),
            timezone: Some("Asia/Tokyo".to_string()),
            agents: Some(vec![super::AgentId::new("alice")]),
            retry_max_attempts: 5,
            retry_interval_minutes: 10,
            ..Default::default()
        },
        pulse: super::PulseConfig::default(),
        web_fetch: super::web_fetch::WebFetchConfig::default(),
    };

    save_config_with_secrets(&config, &path).expect("save config");

    let yaml = std::fs::read_to_string(&path).expect("yaml");
    assert!(yaml.contains("enabled: true"));
    assert!(yaml.contains("schedule:"));
    assert!(yaml.contains("04:00"));
    assert!(yaml.contains("timezone:"));
    assert!(yaml.contains("Asia/Tokyo"));
    assert!(yaml.contains("- alice"));
    assert!(yaml.contains("max_attempts: 5"));
    assert!(yaml.contains("interval_minutes: 10"));
}

// --- Pulse config tests ---

fn pulse_config_yml(pulse_section: &str) -> String {
    format!(
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
pulse:
{pulse_section}"#
    )
}

#[test]
#[serial]
fn pulse_config_defaults_disabled() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(&temp_dir, sample_config());

    // Act
    let config = Config::load(Some(&file_path)).expect("load config");

    // Assert
    assert!(!config.pulse().enabled);
}

#[test]
#[serial]
fn pulse_config_loads_runtime_fields() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &pulse_config_yml(
            r#"  enabled: true
  tick_interval: "2m"
  timezone: "Asia/Tokyo""#,
        ),
    );

    // Act
    let config = Config::load(Some(&file_path)).expect("load config");

    // Assert
    assert!(config.pulse().enabled);
    assert_eq!(config.pulse().tick_interval_secs, 120);
    assert_eq!(config.pulse().timezone.as_deref(), Some("Asia/Tokyo"));
}

#[test]
#[serial]
fn pulse_config_rejects_invalid_timezone() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &pulse_config_yml(
            r#"  enabled: true
  timezone: "Invalid/Zone""#,
        ),
    );

    // Act
    let error = Config::load(Some(&file_path)).expect_err("should fail");

    // Assert
    assert!(
        matches!(error, ConfigError::PulseInvalidTimezone { ref timezone } if timezone == "Invalid/Zone"),
        "expected PulseInvalidTimezone, got {error:?}"
    );
}

#[test]
#[serial]
fn pulse_config_rejects_invalid_tick_interval() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("HOME", temp_dir.path());
    let file_path = write_config(
        &temp_dir,
        &pulse_config_yml(
            r#"  enabled: true
  tick_interval: "0s""#,
        ),
    );

    // Act
    let error = Config::load(Some(&file_path)).expect_err("should fail");

    // Assert
    assert!(
        matches!(error, ConfigError::PulseInvalidTickInterval { .. }),
        "expected PulseInvalidTickInterval, got {error:?}"
    );
}

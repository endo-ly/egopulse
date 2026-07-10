//! 出力サニタイズユーティリティ。
//!
//! Config 由来のシークレット値と well-known パターンの二層リダクションにより、
//! ツール出力に秘密情報が漏洩しないようマスクする。

use crate::config::Config;
use crate::llm::codex_auth::{is_codex_provider, resolve_codex_auth};
use crate::tools::ToolResult;

/// Well-known secret パターン。出力に含まれる場合 [REDACTED] に置換する。
pub(crate) const SECRET_PATTERNS: &[&str] = &[
    // OpenAI
    "sk-",
    // OpenRouter
    "sk-or-",
    // Anthropic
    "sk-ant-",
    // Slack
    "xoxb-",
    "xapp-",
    // GitHub
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    // GitLab
    "glpat-",
    // AWS Access Key ID
    "AKIA",
    "ASIA",
    // Google API Key / OAuth
    "AIza",
    // Stripe
    "sk_live_",
    "sk_test_",
    "rk_live_",
];

/// Config から収集したシークレット値で出力をリダクションする。
pub(crate) fn redact_secrets(output: &str, secrets: &[(String, String)]) -> String {
    if secrets.is_empty() {
        return output.to_string();
    }
    let mut sorted: Vec<_> = secrets
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.1.len()));
    let mut redacted = output.to_string();
    for (key, value) in &sorted {
        redacted = redacted.replace(value, &format!("[REDACTED:{key}]"));
    }
    redacted
}

/// Well-known secret プレフィックスに基づくパターンリダクション。
pub(crate) fn redact_known_secret_patterns(output: &str) -> String {
    let mut result = output.to_string();
    for prefix in SECRET_PATTERNS {
        let mut start = 0usize;
        while let Some(offset) = result[start..].find(prefix) {
            let abs_offset = start + offset;
            let preceded_by_boundary = abs_offset == 0
                || result[..abs_offset]
                    .chars()
                    .last()
                    .is_some_and(|c| !c.is_alphanumeric() && c != '_');
            if !preceded_by_boundary {
                start = abs_offset + 1;
                continue;
            }
            let prefix_end = abs_offset + prefix.len();
            let secret_end = result[prefix_end..]
                .find(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == '\n' || c == ';')
                .map(|i| prefix_end + i)
                .unwrap_or(result.len());
            if secret_end > prefix_end {
                result = format!(
                    "{}[REDACTED:secret]{}",
                    &result[..abs_offset],
                    &result[secret_end..]
                );
                start = abs_offset + "[REDACTED:secret]".len();
            } else {
                start = prefix_end;
            }
            if start >= result.len() {
                break;
            }
        }
    }
    result
}

pub(crate) fn sanitize_output_string(output: &str, secrets: &[(String, String)]) -> String {
    let redacted = redact_secrets(output, secrets);
    redact_known_secret_patterns(&redacted)
}

pub(crate) fn sanitize_message_content(
    content: crate::llm::MessageContent,
    secrets: &[(String, String)],
) -> crate::llm::MessageContent {
    use crate::llm::{MessageContent, MessageContentPart};

    match content {
        MessageContent::Text(text) => MessageContent::Text(sanitize_output_string(&text, secrets)),
        MessageContent::Parts(parts) => MessageContent::Parts(
            parts
                .into_iter()
                .map(|part| match part {
                    MessageContentPart::InputText { text } => MessageContentPart::InputText {
                        text: sanitize_output_string(&text, secrets),
                    },
                    MessageContentPart::InputImage { image_url, detail } => {
                        MessageContentPart::InputImage {
                            image_url: sanitize_output_string(&image_url, secrets),
                            detail: detail.map(|value| sanitize_output_string(&value, secrets)),
                        }
                    }
                })
                .collect(),
        ),
    }
}

pub(crate) fn sanitize_json_value(
    value: serde_json::Value,
    secrets: &[(String, String)],
) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            serde_json::Value::String(sanitize_output_string(&text, secrets))
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|item| sanitize_json_value(item, secrets))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, sanitize_json_value(value, secrets)))
                .collect(),
        ),
        other => other,
    }
}

pub(crate) fn sanitize_tool_result(
    mut result: ToolResult,
    secrets: &[(String, String)],
) -> ToolResult {
    result.content = sanitize_output_string(&result.content, secrets);
    result.llm_content = sanitize_message_content(result.llm_content, secrets);
    result.details = result
        .details
        .take()
        .map(|details| sanitize_json_value(details, secrets));
    result
}

/// Config から抽出したシークレット値のリストを構築する。
pub(crate) fn collect_config_secrets(config: &Config) -> Vec<(String, String)> {
    let mut secrets = Vec::new();
    for (name, provider) in &config.providers {
        if let Some(rv) = &provider.api_key {
            secrets.push((format!("provider.{name}.api_key"), rv.value().to_string()));
        }
    }
    for (name, channel) in &config.channels {
        if let Some(rv) = &channel.auth_token {
            secrets.push((format!("channel.{name}.auth_token"), rv.value().to_string()));
        }
        if let Some(bots) = &channel.discord_bots {
            for (bot_id, bot) in bots {
                if let Some(rv) = &bot.token {
                    secrets.push((
                        format!("channels.{name}.bots.{bot_id}.token"),
                        rv.value().to_string(),
                    ));
                }
            }
        }
        if let Some(bots) = &channel.telegram_bots {
            for (bot_id, bot) in bots {
                if let Some(rv) = &bot.token {
                    secrets.push((
                        format!("channels.{name}.telegram_bots.{bot_id}.token"),
                        rv.value().to_string(),
                    ));
                }
            }
        }
    }
    for (receiver_id, receiver) in &config.webhooks.receivers {
        if let Some(token) = &receiver.token {
            secrets.push((
                format!("webhooks.receivers.{receiver_id}.token"),
                token.value().to_string(),
            ));
        }
    }
    let has_codex = config
        .providers
        .keys()
        .any(|name| is_codex_provider(name.as_str()));
    if has_codex {
        if let Ok(auth) = resolve_codex_auth() {
            secrets.push(("codex.bearer_token".to_string(), auth.bearer_token));
        }
    }
    secrets.retain(|(_, value)| !value.is_empty());
    secrets.sort_by(|left, right| left.0.cmp(&right.0));
    secrets.dedup_by(|left, right| left.1 == right.1);
    secrets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::secret_ref::ResolvedValue;
    use crate::config::{ChannelConfig, ChannelName, Config, ProviderConfig, ProviderId};
    use crate::llm::{MessageContent, MessageContentPart};
    use crate::test_env::EnvVarGuard;
    use serde_json::json;

    /// Build a Config with no providers/channels/webhooks secrets, for tests that
    /// customize a subset of secret sources.
    fn base_config(state_root: &str) -> Config {
        Config {
            default_provider: ProviderId::new("local"),
            default_model: None,
            providers: std::collections::HashMap::new(),
            state_root: state_root.to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            compact_keep_recent: 20,
            default_context_window_tokens: 32768,
            compaction_threshold_ratio: 0.80,
            compaction_target_ratio: 0.40,
            channels: std::collections::HashMap::new(),
            default_agent: crate::config::AgentId::new("default"),
            agents: std::collections::HashMap::new(),
            timezone: "UTC".to_string(),
            sleep_batch: crate::config::SleepBatchConfig::default(),
            pulse: crate::config::PulseConfig::default(),
            db: crate::config::DatabaseConfig::default(),
            web_fetch: crate::config::web_fetch::WebFetchConfig::default(),
            webhooks: crate::config::WebhooksConfig::default(),
        }
    }

    /// redact_secrets: Config 由来のシークレット値を [REDACTED:key] に置換する。
    #[test]
    fn test_redact_secrets_replaces_config_values() {
        // Arrange
        let secrets = vec![(
            "provider.openai.api_key".to_string(),
            "sk-abc123".to_string(),
        )];
        let input = "The key is sk-abc123 and it should be hidden";

        // Act
        let result = redact_secrets(input, &secrets);

        // Assert
        assert!(result.contains("[REDACTED:provider.openai.api_key]"));
        assert!(!result.contains("sk-abc123"));
    }

    /// redact_secrets: 空のシークレットリストでは入力が変更されない。
    #[test]
    fn test_redact_secrets_empty_list_noop() {
        // Arrange
        let secrets: Vec<(String, String)> = vec![];
        let input = "no secrets here";

        // Act
        let result = redact_secrets(input, &secrets);

        // Assert
        assert_eq!(result, input);
    }

    /// redact_secrets: 長いシークレットから先に置換し、部分一致による漏洩を防ぐ。
    #[test]
    fn test_redact_secrets_longer_first() {
        // Arrange
        // "sk-long-secret-key" と "sk-long" が重なる場合、長い方が先に置換される
        let secrets = vec![
            ("short".to_string(), "sk-long".to_string()),
            ("long".to_string(), "sk-long-secret-key".to_string()),
        ];
        let input = "found sk-long-secret-key and also sk-long";

        // Act
        let result = redact_secrets(input, &secrets);

        // Assert
        assert!(result.contains("[REDACTED:long]"));
        assert!(result.contains("[REDACTED:short]"));
        assert!(!result.contains("sk-long"));
    }

    /// redact_known_secret_patterns: OpenAI sk- プレフィックスがマスクされる。
    #[test]
    fn test_redact_known_patterns_openai() {
        // Arrange
        let input = "key=sk-proj-abc123def456 end";

        // Act
        let result = redact_known_secret_patterns(input);

        // Assert
        assert!(result.contains("[REDACTED:secret]"));
        assert!(!result.contains("sk-proj-abc123def456"));
    }

    /// redact_known_secret_patterns: 1行に複数シークレットがあっても全てマスクされる。
    #[test]
    fn test_redact_known_patterns_multiple() {
        // Arrange
        let input = "key1=sk-aaa111 key2=ghp_bbb222";

        // Act
        let result = redact_known_secret_patterns(input);

        // Assert
        // sk- と ghp_ の両方がマスクされる
        let redacted_count = result.matches("[REDACTED:secret]").count();
        assert_eq!(redacted_count, 2);
    }

    /// redact_known_secret_patterns: 単語途中の sk- はマスクされない。
    #[test]
    fn test_redact_known_patterns_no_false_positive() {
        // Arrange
        // "task-name" の "sk-" は単語途中なのでマスク対象外
        let input = "task-name is valid";

        // Act
        let result = redact_known_secret_patterns(input);

        // Assert
        assert_eq!(result, input);
    }

    /// sanitize_output_string: Config シークレットと known パターンの二層が両方適用される。
    #[test]
    fn test_sanitize_output_string_both_layers() {
        // Arrange
        let secrets = vec![("my.key".to_string(), "my-secret-value".to_string())];
        let input = "config=my-secret-value and known=sk-abc123";

        // Act
        let result = sanitize_output_string(input, &secrets);

        // Assert
        assert!(result.contains("[REDACTED:my.key]"));
        assert!(result.contains("[REDACTED:secret]"));
        assert!(!result.contains("my-secret-value"));
        assert!(!result.contains("sk-abc123"));
    }

    /// sanitize_json_value: ネストされた JSON 文字列値もマスクされる。
    #[test]
    fn test_sanitize_json_value_nested() {
        // Arrange
        let secrets = vec![("token".to_string(), "sk-hidden-token".to_string())];
        let value = json!({
            "level1": {
                "level2": "sk-hidden-token is here",
                "number": 42,
                "list": ["sk-hidden-token in array"]
            }
        });

        // Act
        let result = sanitize_json_value(value, &secrets);

        // Assert
        let level2 = result.get("level1").unwrap().get("level2").unwrap();
        assert!(level2.as_str().unwrap().contains("[REDACTED:token]"));
        assert!(
            result
                .get("level1")
                .unwrap()
                .get("list")
                .unwrap()
                .get(0)
                .unwrap()
                .as_str()
                .unwrap()
                .contains("[REDACTED:token]")
        );
        // 数値はそのまま
        assert_eq!(result.get("level1").unwrap().get("number").unwrap(), 42);
    }

    /// sanitize_tool_result: content / llm_content / details の全フィールドがサニタイズされる。
    #[test]
    fn test_sanitize_tool_result_applies_to_all_fields() {
        // Arrange
        let secrets = vec![("key".to_string(), "leaked-key".to_string())];
        let result = ToolResult {
            content: "contains leaked-key here".to_string(),
            is_error: false,
            details: Some(json!({"trace": "leaked-key in trace"})),
            llm_content: MessageContent::text("leaked-key in llm".to_string()),
        };

        // Act
        let sanitized = sanitize_tool_result(result, &secrets);

        // Assert
        assert!(sanitized.content.contains("[REDACTED:key]"));
        assert!(!sanitized.content.contains("leaked-key"));
        match &sanitized.llm_content {
            MessageContent::Text(text) => {
                assert!(text.contains("[REDACTED:key]"));
                assert!(!text.contains("leaked-key"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
        let trace = sanitized
            .details
            .as_ref()
            .and_then(|d| d.get("trace"))
            .and_then(|v| v.as_str())
            .unwrap();
        assert!(trace.contains("[REDACTED:key]"));
    }

    /// sanitize_message_content: MessageContent::Parts 内の InputText/InputImage もサニタイズされる。
    #[test]
    fn test_sanitize_message_content_parts() {
        // Arrange
        let secrets = vec![("secret".to_string(), "SECRET123".to_string())];
        let content = MessageContent::parts(vec![
            MessageContentPart::InputText {
                text: "payload SECRET123".to_string(),
            },
            MessageContentPart::InputImage {
                image_url: "https://example.com/img?token=SECRET123".to_string(),
                detail: Some("detail SECRET123".to_string()),
            },
        ]);

        // Act
        let sanitized = sanitize_message_content(content, &secrets);

        // Assert
        match sanitized {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    MessageContentPart::InputText { text } => {
                        assert!(!text.contains("SECRET123"));
                        assert!(text.contains("[REDACTED:secret]"));
                    }
                    other => panic!("expected InputText, got {other:?}"),
                }
                match &parts[1] {
                    MessageContentPart::InputImage { image_url, detail } => {
                        assert!(!image_url.contains("SECRET123"));
                        assert!(detail.as_deref().is_some_and(|d| !d.contains("SECRET123")));
                    }
                    other => panic!("expected InputImage, got {other:?}"),
                }
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    /// collect_config_secrets: Provider API キーが抽出される。
    #[test]
    fn test_collect_config_secrets_extracts_api_keys() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = Config {
            default_provider: ProviderId::new("openai"),
            default_model: None,
            providers: std::collections::HashMap::from([(
                ProviderId::new("openai"),
                ProviderConfig {
                    label: "OpenAI".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key: Some(ResolvedValue::Literal("sk-test-key-123".to_string())),
                    default_model: "gpt-4o".to_string(),
                    models: std::collections::HashMap::from([(
                        "gpt-4o".to_string(),
                        crate::config::ModelConfig::default(),
                    )]),
                },
            )]),
            state_root: dir.path().to_str().expect("path").to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            compact_keep_recent: 20,
            default_context_window_tokens: 32768,
            compaction_threshold_ratio: 0.80,
            compaction_target_ratio: 0.40,
            channels: std::collections::HashMap::new(),
            default_agent: crate::config::AgentId::new("default"),
            agents: std::collections::HashMap::new(),
            timezone: "UTC".to_string(),
            sleep_batch: crate::config::SleepBatchConfig::default(),
            pulse: crate::config::PulseConfig::default(),
            db: crate::config::DatabaseConfig::default(),
            web_fetch: crate::config::web_fetch::WebFetchConfig::default(),
            webhooks: crate::config::WebhooksConfig::default(),
        };

        // Act
        let secrets = collect_config_secrets(&config);

        // Assert
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].0, "provider.openai.api_key");
        assert_eq!(secrets[0].1, "sk-test-key-123");
    }

    /// collect_config_secrets: Channel の auth_token / Discord bot token が抽出される。
    #[test]
    fn test_collect_config_secrets_extracts_auth_tokens() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());

        let mut bots = std::collections::HashMap::new();
        bots.insert(
            crate::config::BotId::new("main"),
            crate::config::DiscordBotConfig {
                token: Some(ResolvedValue::Literal("bot-token-value".to_string())),
                file_token: None,
            },
        );

        let config = Config {
            default_provider: ProviderId::new("local"),
            default_model: None,
            providers: std::collections::HashMap::new(),
            state_root: dir.path().to_str().expect("path").to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            compact_keep_recent: 20,
            default_context_window_tokens: 32768,
            compaction_threshold_ratio: 0.80,
            compaction_target_ratio: 0.40,
            channels: std::collections::HashMap::from([(
                ChannelName::new("discord"),
                ChannelConfig {
                    enabled: Some(true),
                    auth_token: Some(ResolvedValue::Literal("auth-token-value".to_string())),
                    file_auth_token: None,
                    discord_bots: Some(bots),
                    ..Default::default()
                },
            )]),
            default_agent: crate::config::AgentId::new("default"),
            agents: std::collections::HashMap::new(),
            timezone: "UTC".to_string(),
            sleep_batch: crate::config::SleepBatchConfig::default(),
            pulse: crate::config::PulseConfig::default(),
            db: crate::config::DatabaseConfig::default(),
            web_fetch: crate::config::web_fetch::WebFetchConfig::default(),
            webhooks: crate::config::WebhooksConfig::default(),
        };

        // Act
        let secrets = collect_config_secrets(&config);

        // Assert
        let keys: Vec<&str> = secrets.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"channel.discord.auth_token"));
        assert!(keys.contains(&"channels.discord.bots.main.token"));
        assert_eq!(secrets.len(), 2);
    }

    /// collect_config_secrets: channels.discord.bots.<bot_id>.token が抽出される。
    #[test]
    fn test_collect_config_secrets_extracts_discord_bot_tokens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());

        let mut bots = std::collections::HashMap::new();
        bots.insert(
            crate::config::BotId::new("bot123"),
            crate::config::DiscordBotConfig {
                token: Some(ResolvedValue::Literal("bot-token-value".to_string())),
                file_token: None,
            },
        );

        let config = Config {
            default_provider: ProviderId::new("local"),
            default_model: None,
            providers: std::collections::HashMap::new(),
            state_root: dir.path().to_str().expect("path").to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            compact_keep_recent: 20,
            default_context_window_tokens: 32768,
            compaction_threshold_ratio: 0.80,
            compaction_target_ratio: 0.40,
            channels: std::collections::HashMap::from([(
                ChannelName::new("discord"),
                ChannelConfig {
                    enabled: Some(true),
                    discord_bots: Some(bots),
                    ..Default::default()
                },
            )]),
            default_agent: crate::config::AgentId::new("default"),
            agents: std::collections::HashMap::new(),
            timezone: "UTC".to_string(),
            sleep_batch: crate::config::SleepBatchConfig::default(),
            pulse: crate::config::PulseConfig::default(),
            db: crate::config::DatabaseConfig::default(),
            web_fetch: crate::config::web_fetch::WebFetchConfig::default(),
            webhooks: crate::config::WebhooksConfig::default(),
        };

        let secrets = collect_config_secrets(&config);

        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].0, "channels.discord.bots.bot123.token");
        assert_eq!(secrets[0].1, "bot-token-value");
    }

    /// collect_config_secrets: openai-codex プロバイダー存在時に bearer_token が抽出される。
    #[test]
    fn test_collect_config_secrets_extracts_codex_bearer_token() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvVarGuard::set("HOME", dir.path())
            .also_set("OPENAI_CODEX_ACCESS_TOKEN", "")
            .also_set("CODEX_HOME", "");
        crate::llm::codex_auth::clear_auth_cache();

        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).expect("create .codex dir");
        std::fs::write(
            codex_dir.join("auth.json"),
            r#"{"tokens":{"access_token":"test-codex-bearer-abc123","refresh_token":"test-refresh"}}"#,
        )
        .expect("write auth.json");

        let config = Config {
            default_provider: ProviderId::new("openai-codex"),
            default_model: None,
            providers: std::collections::HashMap::from([(
                ProviderId::new("openai-codex"),
                ProviderConfig {
                    label: "Codex".to_string(),
                    base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                    api_key: None,
                    default_model: "codex-mini".to_string(),
                    models: std::collections::HashMap::from([(
                        "codex-mini".to_string(),
                        crate::config::ModelConfig::default(),
                    )]),
                },
            )]),
            state_root: dir.path().to_str().expect("path").to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            compact_keep_recent: 20,
            default_context_window_tokens: 32768,
            compaction_threshold_ratio: 0.80,
            compaction_target_ratio: 0.40,
            channels: std::collections::HashMap::new(),
            default_agent: crate::config::AgentId::new("default"),
            agents: std::collections::HashMap::new(),
            timezone: "UTC".to_string(),
            sleep_batch: crate::config::SleepBatchConfig::default(),
            pulse: crate::config::PulseConfig::default(),
            db: crate::config::DatabaseConfig::default(),
            web_fetch: crate::config::web_fetch::WebFetchConfig::default(),
            webhooks: crate::config::WebhooksConfig::default(),
        };

        // Act
        let secrets = collect_config_secrets(&config);

        // Assert
        let keys: Vec<&str> = secrets.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            keys.contains(&"codex.bearer_token"),
            "expected codex.bearer_token in {keys:?}"
        );
        let bearer = secrets
            .iter()
            .find(|(k, _)| k == "codex.bearer_token")
            .expect("bearer_token entry");
        assert_eq!(bearer.1, "test-codex-bearer-abc123");
    }

    /// collect_config_secrets: openai-codex なしの場合は codex 系エントリが含まれない。
    #[test]
    fn test_collect_config_secrets_no_codex_without_provider() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());

        let config = Config {
            default_provider: ProviderId::new("openai"),
            default_model: None,
            providers: std::collections::HashMap::from([(
                ProviderId::new("openai"),
                ProviderConfig {
                    label: "OpenAI".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key: Some(ResolvedValue::Literal("sk-test".to_string())),
                    default_model: "gpt-4o".to_string(),
                    models: std::collections::HashMap::from([(
                        "gpt-4o".to_string(),
                        crate::config::ModelConfig::default(),
                    )]),
                },
            )]),
            state_root: dir.path().to_str().expect("path").to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            compact_keep_recent: 20,
            default_context_window_tokens: 32768,
            compaction_threshold_ratio: 0.80,
            compaction_target_ratio: 0.40,
            channels: std::collections::HashMap::new(),
            default_agent: crate::config::AgentId::new("default"),
            agents: std::collections::HashMap::new(),
            timezone: "UTC".to_string(),
            sleep_batch: crate::config::SleepBatchConfig::default(),
            pulse: crate::config::PulseConfig::default(),
            db: crate::config::DatabaseConfig::default(),
            web_fetch: crate::config::web_fetch::WebFetchConfig::default(),
            webhooks: crate::config::WebhooksConfig::default(),
        };

        // Act
        let secrets = collect_config_secrets(&config);

        // Assert
        let keys: Vec<&str> = secrets.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            !keys.iter().any(|k| k.starts_with("codex.")),
            "codex entries should not exist: {keys:?}"
        );
    }

    /// collect_config_secrets: channels.telegram.telegram_bots.<bot_id>.token が抽出される。
    #[test]
    fn test_collect_config_secrets_extracts_telegram_bot_tokens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());

        let mut bots = std::collections::HashMap::new();
        bots.insert(
            crate::config::BotId::new("main"),
            crate::config::TelegramBotConfig {
                token: Some(ResolvedValue::Literal("tg-bot-token-value".to_string())),
                file_token: None,
            },
        );

        let mut config = base_config(dir.path().to_str().expect("path"));
        config.channels.insert(
            ChannelName::new("telegram"),
            ChannelConfig {
                telegram_bots: Some(bots),
                ..Default::default()
            },
        );

        let secrets = collect_config_secrets(&config);

        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].0, "channels.telegram.telegram_bots.main.token");
        assert_eq!(secrets[0].1, "tg-bot-token-value");
    }

    /// collect_config_secrets: webhooks.receivers.<receiver_id>.token が抽出される。
    #[test]
    fn test_collect_config_secrets_extracts_webhook_receiver_tokens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());

        let mut config = base_config(dir.path().to_str().expect("path"));
        config.webhooks.receivers.insert(
            crate::config::WebhookReceiverId::new("egograph"),
            crate::config::WebhookReceiverConfig {
                token: Some(ResolvedValue::Literal("wh-receiver-token".to_string())),
                file_token: None,
                target: crate::config::WebhookTargetConfig {
                    channel: ChannelName::new("web"),
                    thread: "main".to_string(),
                    agent: None,
                },
            },
        );

        let secrets = collect_config_secrets(&config);

        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].0, "webhooks.receivers.egograph.token");
        assert_eq!(secrets[0].1, "wh-receiver-token");
    }

    /// collect_config_secrets: 空値は除外し、同じ値は 1 件に deduplicate される。
    #[test]
    fn test_collect_config_secrets_skips_empty_and_deduplicates_by_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());

        let mut discord_bots = std::collections::HashMap::new();
        discord_bots.insert(
            crate::config::BotId::new("a"),
            crate::config::DiscordBotConfig {
                token: Some(ResolvedValue::Literal("dup-tok".to_string())),
                file_token: None,
            },
        );
        let mut telegram_bots = std::collections::HashMap::new();
        telegram_bots.insert(
            crate::config::BotId::new("b"),
            crate::config::TelegramBotConfig {
                token: Some(ResolvedValue::Literal("dup-tok".to_string())),
                file_token: None,
            },
        );

        let mut config = base_config(dir.path().to_str().expect("path"));
        // Empty API key must be skipped.
        config.providers.insert(
            ProviderId::new("empty"),
            ProviderConfig {
                label: "Empty".to_string(),
                base_url: "https://example.com".to_string(),
                api_key: Some(ResolvedValue::Literal(String::new())),
                default_model: "m".to_string(),
                models: std::collections::HashMap::new(),
            },
        );
        config.channels.insert(
            ChannelName::new("discord"),
            ChannelConfig {
                discord_bots: Some(discord_bots),
                ..Default::default()
            },
        );
        config.channels.insert(
            ChannelName::new("telegram"),
            ChannelConfig {
                telegram_bots: Some(telegram_bots),
                ..Default::default()
            },
        );
        config.webhooks.receivers.insert(
            crate::config::WebhookReceiverId::new("r"),
            crate::config::WebhookReceiverConfig {
                token: Some(ResolvedValue::Literal("unique-wh".to_string())),
                file_token: None,
                target: crate::config::WebhookTargetConfig {
                    channel: ChannelName::new("web"),
                    thread: "main".to_string(),
                    agent: None,
                },
            },
        );

        let secrets = collect_config_secrets(&config);

        // "dup-tok" appears once (deduplicated), "unique-wh" once, empty excluded.
        assert_eq!(secrets.len(), 2, "secrets = {secrets:?}");
        assert!(
            secrets.iter().all(|(_, v)| !v.is_empty()),
            "empty values must be skipped: {secrets:?}"
        );
        let dup_count = secrets.iter().filter(|(_, v)| v == "dup-tok").count();
        assert_eq!(
            dup_count, 1,
            "duplicate value must be deduplicated: {secrets:?}"
        );
        // Sorted by key; the discord bot key sorts before the webhook receiver key.
        assert_eq!(secrets[0].0, "channels.discord.bots.a.token");
        assert_eq!(secrets[0].1, "dup-tok");
        assert_eq!(secrets[1].0, "webhooks.receivers.r.token");
        assert_eq!(secrets[1].1, "unique-wh");
    }

    /// collect_config_secrets + sanitize: 複数種の秘密値が string / JSON details / LLM content の全経路でマスクされる。
    #[test]
    fn test_sanitize_redacts_multiple_secret_kinds_across_outputs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());

        let mut discord_bots = std::collections::HashMap::new();
        discord_bots.insert(
            crate::config::BotId::new("main"),
            crate::config::DiscordBotConfig {
                token: Some(ResolvedValue::Literal("discord-bot-tok".to_string())),
                file_token: None,
            },
        );
        let mut telegram_bots = std::collections::HashMap::new();
        telegram_bots.insert(
            crate::config::BotId::new("main"),
            crate::config::TelegramBotConfig {
                token: Some(ResolvedValue::Literal("tg-bot-tok".to_string())),
                file_token: None,
            },
        );

        let mut config = base_config(dir.path().to_str().expect("path"));
        config.providers.insert(
            ProviderId::new("openai"),
            ProviderConfig {
                label: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: Some(ResolvedValue::Literal("sk-prod-123".to_string())),
                default_model: "gpt-4o".to_string(),
                models: std::collections::HashMap::new(),
            },
        );
        config.channels.insert(
            ChannelName::new("discord"),
            ChannelConfig {
                discord_bots: Some(discord_bots),
                ..Default::default()
            },
        );
        config.channels.insert(
            ChannelName::new("telegram"),
            ChannelConfig {
                telegram_bots: Some(telegram_bots),
                ..Default::default()
            },
        );
        config.webhooks.receivers.insert(
            crate::config::WebhookReceiverId::new("egograph"),
            crate::config::WebhookReceiverConfig {
                token: Some(ResolvedValue::Literal("wh-receiver-tok".to_string())),
                file_token: None,
                target: crate::config::WebhookTargetConfig {
                    channel: ChannelName::new("web"),
                    thread: "main".to_string(),
                    agent: None,
                },
            },
        );

        let secrets = collect_config_secrets(&config);
        let values: Vec<&str> = secrets.iter().map(|(_, v)| v.as_str()).collect();
        assert!(values.contains(&"sk-prod-123"), "secrets = {secrets:?}");
        assert!(values.contains(&"discord-bot-tok"), "secrets = {secrets:?}");
        assert!(values.contains(&"tg-bot-tok"), "secrets = {secrets:?}");
        assert!(values.contains(&"wh-receiver-tok"), "secrets = {secrets:?}");

        // (1) Plain string output containing every secret value.
        let text = "keys: sk-prod-123 discord-bot-tok tg-bot-tok wh-receiver-tok";
        let redacted_text = sanitize_output_string(text, &secrets);
        assert!(!redacted_text.contains("sk-prod-123"));
        assert!(!redacted_text.contains("discord-bot-tok"));
        assert!(!redacted_text.contains("tg-bot-tok"));
        assert!(!redacted_text.contains("wh-receiver-tok"));
        assert!(redacted_text.contains("[REDACTED:"));

        // (2) Nested JSON details.
        let details = json!({
            "trace": "discord-bot-tok leaked",
            "nested": { "items": ["tg-bot-tok", { "deep": "wh-receiver-tok" }] }
        });
        let redacted_json = sanitize_json_value(details, &secrets);
        let rendered = redacted_json.to_string();
        assert!(!rendered.contains("discord-bot-tok"));
        assert!(!rendered.contains("tg-bot-tok"));
        assert!(!rendered.contains("wh-receiver-tok"));

        // (3) LLM message content.
        let content = MessageContent::parts(vec![MessageContentPart::InputText {
            text: "sk-prod-123 and wh-receiver-tok in content".to_string(),
        }]);
        let redacted_content = sanitize_message_content(content, &secrets);
        match redacted_content {
            MessageContent::Parts(parts) => match &parts[0] {
                MessageContentPart::InputText { text } => {
                    assert!(!text.contains("sk-prod-123"));
                    assert!(!text.contains("wh-receiver-tok"));
                    assert!(text.contains("[REDACTED:"));
                }
                other => panic!("expected InputText, got {other:?}"),
            },
            other => panic!("expected Parts, got {other:?}"),
        }
    }
}

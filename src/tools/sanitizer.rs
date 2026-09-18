//! 出力サニタイズユーティリティ。
//!
//! Config 由来のシークレット値と well-known パターンの二層リダクションにより、
//! ツール出力に秘密情報が漏洩しないようマスクする。

use std::collections::HashSet;

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
    let mut seen_values = HashSet::new();
    secrets.retain(|(_, value)| seen_values.insert(value.clone()));
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

    /// collect_config_secrets: 全ての設定由来の秘密値を抽出し、空値と重複値を除外する。
    #[test]
    fn test_collect_config_secrets_covers_all_config_sources() {
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

        let mut config = base_config(dir.path().to_str().expect("path"));
        config.providers.insert(
            ProviderId::new("openai"),
            ProviderConfig {
                label: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: Some(ResolvedValue::Literal("openai-api-key".to_string())),
                default_model: "gpt-4o".to_string(),
                models: std::collections::HashMap::new(),
            },
        );
        config.providers.insert(
            ProviderId::new("openai-codex"),
            ProviderConfig {
                label: "Codex".to_string(),
                base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                api_key: None,
                default_model: "codex-mini".to_string(),
                models: std::collections::HashMap::new(),
            },
        );
        config.providers.insert(
            ProviderId::new("empty"),
            ProviderConfig {
                label: "Empty".to_string(),
                base_url: "https://example.com".to_string(),
                api_key: Some(ResolvedValue::Literal(String::new())),
                default_model: "model".to_string(),
                models: std::collections::HashMap::new(),
            },
        );

        let mut discord_bots = std::collections::HashMap::new();
        discord_bots.insert(
            crate::config::BotId::new("alias"),
            crate::config::DiscordBotConfig {
                token: Some(ResolvedValue::Literal("discord-bot-token".to_string())),
                file_token: None,
            },
        );
        discord_bots.insert(
            crate::config::BotId::new("main"),
            crate::config::DiscordBotConfig {
                token: Some(ResolvedValue::Literal("discord-bot-token".to_string())),
                file_token: None,
            },
        );
        config.channels.insert(
            ChannelName::new("discord"),
            ChannelConfig {
                auth_token: Some(ResolvedValue::Literal("channel-auth".to_string())),
                discord_bots: Some(discord_bots),
                ..Default::default()
            },
        );

        let mut telegram_bots = std::collections::HashMap::new();
        telegram_bots.insert(
            crate::config::BotId::new("main"),
            crate::config::TelegramBotConfig {
                token: Some(ResolvedValue::Literal("telegram-bot-token".to_string())),
                file_token: None,
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
                token: Some(ResolvedValue::Literal("webhook-token".to_string())),
                file_token: None,
                target: crate::config::WebhookTargetConfig {
                    channel: ChannelName::new("web"),
                    thread: "main".to_string(),
                    agent: None,
                },
            },
        );

        let secrets = collect_config_secrets(&config);
        assert_eq!(
            secrets,
            vec![
                (
                    "channel.discord.auth_token".to_string(),
                    "channel-auth".to_string(),
                ),
                (
                    "channels.discord.bots.alias.token".to_string(),
                    "discord-bot-token".to_string(),
                ),
                (
                    "channels.telegram.telegram_bots.main.token".to_string(),
                    "telegram-bot-token".to_string(),
                ),
                (
                    "codex.bearer_token".to_string(),
                    "test-codex-bearer-abc123".to_string(),
                ),
                (
                    "provider.openai.api_key".to_string(),
                    "openai-api-key".to_string(),
                ),
                (
                    "webhooks.receivers.egograph.token".to_string(),
                    "webhook-token".to_string(),
                ),
            ]
        );

        let mut without_codex = config.clone();
        without_codex
            .providers
            .remove(&ProviderId::new("openai-codex"));
        assert!(
            collect_config_secrets(&without_codex)
                .iter()
                .all(|(key, _)| !key.starts_with("codex."))
        );
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

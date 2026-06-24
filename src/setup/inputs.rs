//! チャットライクウィザード向けの入力データ型と検証ロジック。

use crate::config::is_valid_base_url;
use crate::llm::codex_auth;
use crate::setup::provider::{provider_default_base_url, provider_default_model};

/// チャットライクセットアップウィザードが収集する全入力フィールド。
///
/// `Field` 構造体廃止後の後続 Step で `save_config` の入力データ型として使用される。
/// 各フィールドはプロンプト (Q1〜Q7) に 1:1 対応する。
#[derive(Clone)]
pub(crate) struct SetupInputs {
    pub agent_label: String,
    pub provider_id: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub web_enabled: bool,
    pub discord_enabled: bool,
    pub discord_bot_token: String,
    pub telegram_enabled: bool,
    pub telegram_bot_token: String,
}

/// `SetupInputs` の内容を検証する。
///
/// 検証項目 (いずれかでも違反なら `Err`):
/// - `provider_id` が空でない
/// - `base_url` が空でない (空なら provider preset のデフォルトで補完)
/// - `base_url` が有効な URL である
/// - `model` が空でない (空なら provider preset のデフォルトで補完)
/// - 非 localhost 系プロバイダーで `api_key` が空でない
/// - Discord 有効時は `discord_bot_token` が必須
/// - Telegram 有効時は `telegram_bot_token` が必須
///
/// # Errors
///
/// 上記いずれかの検証に失敗した場合、人間が読めるエラーメッセージを返す。
pub(crate) fn validate_inputs(inputs: &SetupInputs) -> Result<(), String> {
    let provider = inputs.provider_id.trim();
    if provider.is_empty() {
        return Err("Provider profile ID is required".into());
    }

    let effective_base_url =
        effective_value(&inputs.base_url, || provider_default_base_url(provider));
    if effective_base_url.is_empty() {
        return Err(format!(
            "API base URL is required for provider '{provider}'"
        ));
    }
    if !is_valid_base_url(effective_base_url) {
        return Err(format!("Invalid API base URL: {effective_base_url}"));
    }

    let effective_model = effective_value(&inputs.model, || provider_default_model(provider));
    if effective_model.is_empty() {
        return Err(format!("LLM model is required for provider '{provider}'"));
    }

    if !codex_auth::provider_allows_empty_api_key(provider, effective_base_url)
        && inputs.api_key.trim().is_empty()
    {
        return Err(
            "API key is required for non-local endpoints. Use a local URL (localhost/127.0.0.1) to skip.".into(),
        );
    }

    if inputs.discord_enabled && inputs.discord_bot_token.trim().is_empty() {
        return Err("Discord bot token is required when Discord is enabled".into());
    }

    if inputs.telegram_enabled && inputs.telegram_bot_token.trim().is_empty() {
        return Err("Telegram bot token is required when Telegram is enabled".into());
    }

    Ok(())
}

fn effective_value(value: &str, fallback: impl FnOnce() -> Option<&'static str>) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback().unwrap_or("")
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::SetupInputs;
    use super::validate_inputs;

    fn valid_inputs() -> SetupInputs {
        SetupInputs {
            agent_label: "Partner".into(),
            provider_id: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o".into(),
            api_key: "sk-test-key".into(),
            web_enabled: true,
            discord_enabled: false,
            discord_bot_token: String::new(),
            telegram_enabled: false,
            telegram_bot_token: String::new(),
        }
    }

    #[test]
    fn validate_inputs_rejects_empty_provider() {
        let mut inputs = valid_inputs();
        inputs.provider_id = String::new();
        let err = validate_inputs(&inputs).unwrap_err();
        assert!(err.to_lowercase().contains("provider"));
    }

    #[test]
    fn validate_inputs_rejects_invalid_base_url() {
        let mut inputs = valid_inputs();
        inputs.base_url = "not a valid url".into();
        let err = validate_inputs(&inputs).unwrap_err();
        assert!(err.to_lowercase().contains("base url") || err.to_lowercase().contains("url"));
    }

    #[test]
    fn validate_inputs_rejects_discord_enabled_without_token() {
        let mut inputs = valid_inputs();
        inputs.discord_enabled = true;
        inputs.discord_bot_token = String::new();
        let err = validate_inputs(&inputs).unwrap_err();
        assert!(err.to_lowercase().contains("discord"));
    }

    #[test]
    fn validate_inputs_accepts_minimum_set() {
        let inputs = valid_inputs();
        validate_inputs(&inputs).expect("valid inputs should pass");
    }

    #[test]
    fn validate_inputs_allows_empty_api_key_for_localhost() {
        let mut inputs = valid_inputs();
        inputs.provider_id = "ollama".into();
        inputs.base_url = "http://127.0.0.1:11434/v1".into();
        inputs.model = "llama3.2".into();
        inputs.api_key = String::new();
        validate_inputs(&inputs).expect("localhost with empty api key should pass");
    }
}

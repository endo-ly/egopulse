//! Setup wizard のフロー制御、メッセージビルダー、分岐判断純粋関数。
//!
//! Step 7 の純粋関数群と Step 8 の trait 抽象化されたフロー統合を統合する。
//! wizard 本体 ([`run_with_source_and_sink`]) は [`PromptSource`] / [`OutputSink`]
//! trait を介して入出力を行い、本番では dialoguer、テストではモックを使用する。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::default_config_path;
use crate::llm::codex_auth::provider_allows_empty_api_key;
use crate::setup::error::SetupWizardError;
use crate::setup::inputs::{SetupInputs, validate_inputs};
use crate::setup::prompts::format_api_key_for_review;
use crate::setup::prompts::{DialoguerOutputSink, DialoguerPromptSource, OutputSink, PromptSource};
use crate::setup::provider::{PROVIDER_PRESETS, find_provider_preset, provider_label_for};
use crate::setup::slugify::slugify_agent_id;
use crate::setup::summary::{ExistingConfig, parse_existing_config, save_config};

/// Web UI のデフォルトアクセス URL。
const WEB_UI_URL: &str = "http://127.0.0.1:10961";
const CUSTOM_MODEL_LABEL: &str = "Custom model...";

/// Review 画面でユーザーが "no" を選択した際の次アクション。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewDecision {
    /// Q1 に戻り、入力をやり直す。
    StartOver,
    /// 保存せずに終了する。
    Abort,
    /// 警告を了承の上、保存へ進む。
    SaveAnyway,
}

/// Select インデックスを `ReviewDecision` へ変換する。
///
/// `dialoguer::Select` が返すインデックス (0=Start over, 1=Abort, 2=Save anyway)
/// を対応する列挙子へマッピングする。範囲外のインデックスは `SaveAnyway` になる。
pub(crate) fn review_decision_from_index(index: usize) -> ReviewDecision {
    match index {
        0 => ReviewDecision::StartOver,
        1 => ReviewDecision::Abort,
        _ => ReviewDecision::SaveAnyway,
    }
}

/// Review 画面に表示する設定内容サマリーを構築する (`docs/setup-redesign.md §4.2 Review`)。
///
/// API Key 行は `format_api_key_for_review` でマスクした値を表示する。
pub(crate) fn build_review_summary(inputs: &SetupInputs) -> String {
    let agent_id = slugify_agent_id(&inputs.agent_label);
    let masked_key = format_api_key_for_review(&inputs.api_key);
    let web_line = if inputs.web_enabled {
        "enabled (auth_token: auto-generated, saved to .env)"
    } else {
        "disabled"
    };

    [
        String::new(),
        "== Review ==".to_string(),
        "About to save the configuration file with the following values:".to_string(),
        String::new(),
        format!("  Agent:    {} (id: {})", inputs.agent_label, agent_id),
        format!("  Provider: {} ({})", inputs.provider_id, inputs.base_url),
        format!("  Model:    {}", inputs.model),
        format!("  API Key:  {}", masked_key),
        format!("  Web:      {}", web_line),
        format!("  Discord:  {}", enabled_label(inputs.discord_enabled)),
        format!("  Telegram: {}", enabled_label(inputs.telegram_enabled)),
    ]
    .join("\n")
}

/// Additional Options ステップの固定案内テキストを返す (`docs/setup-redesign.md §4.2`)。
pub(crate) fn build_additional_options_text() -> String {
    [
        "",
        "== Additional options ==",
        "The configuration has been saved. The following options were not configured in",
        "this setup, but can be set by editing ~/.egopulse/egopulse.config.yaml:",
        "",
        "System:",
        "  - timezone (default: UTC)",
        "  - log_level (default: info)",
        "  - default_context_window_tokens (default: 32768)",
        "  - compaction_threshold_ratio / compaction_target_ratio / compact_keep_recent",
        "  - max_history_messages",
        "",
        "Web UI:",
        "  - channels.web.host (default: 127.0.0.1)",
        "  - channels.web.port (default: 10961)",
        "  - channels.web.allowed_origins (default: [])",
        "",
        "Channels:",
        "  - Additional providers and agents (add entries under \"providers\" / \"agents\")",
        "  - Discord/Telegram channel access control (see docs/channels.md)",
        "  - Voice channel (channels.voice.*)",
        "  - Per-agent persona (SOUL.md)",
        "",
        "Subsystems:",
        "  - sleep_batch (long-term memory processing)",
        "  - pulse (attention activation)",
        "  - db.backup (SQLite backup settings)",
        "  - web_fetch (built-in tool settings)",
        "",
        "See docs/config.md for the full reference.",
        "",
        "Press Enter to continue.",
    ]
    .join("\n")
}

/// Done メッセージを構築する (`docs/setup-redesign.md §4.2 Done`)。
///
/// 保存先パス、バックアップパス (存在する場合のみ)、次ステップの案内、
/// および有効化されたチャネル (Web / Discord / Telegram) の案内を含む。
pub(crate) fn build_done_message(
    inputs: &SetupInputs,
    config_path: &str,
    backup_path: Option<&str>,
) -> String {
    let mut lines = vec![
        String::new(),
        "== Done ==".to_string(),
        format!("Configuration saved: {config_path}"),
    ];

    if let Some(backup) = backup_path {
        lines.push(format!("Backup: {backup}"));
    }

    lines.push(String::new());
    lines.push("Next steps:".into());
    lines.push("  - Start chatting now:          egopulse chat".into());
    lines.push("  - Install as a systemd service: egopulse gateway install".into());
    lines.push(format!("  - Edit configuration:          {config_path}"));
    lines.push("  - Add more agents:             edit the \"agents\" section in the YAML".into());

    if inputs.web_enabled {
        lines.push(String::new());
        lines.push("If Web UI is enabled:".into());
        lines.push(format!("  - URL:    {WEB_UI_URL}"));
        lines.push("  - Token:  see WEB_AUTH_TOKEN in ~/.egopulse/.env".into());
    }

    if inputs.discord_enabled || inputs.telegram_enabled {
        lines.push(String::new());
        lines.push("If Discord or Telegram is enabled:".into());
        lines.push("  - The bot responds to DMs out of the box.".into());
        lines
            .push("  - To enable server/group responses, add channel/chat IDs to the YAML.".into());
        lines.push("    See docs/channels.md for details.".into());
    }

    lines.join("\n")
}

fn build_welcome_message(config_path: &Path) -> String {
    [
        "== EgoPulse Setup ==".to_string(),
        "This wizard will create the minimum configuration needed to run an AI agent.".to_string(),
        String::new(),
        format!("Config file: {}", config_path.to_string_lossy()),
        String::new(),
        "Non-secret answers remain visible. API keys and bot tokens stay hidden.".to_string(),
    ]
    .join("\n")
}

/// API key 空欄時に確認ダイアログを表示すべきか判定する。
///
/// localhost 系 URL (Ollama / LMStudio 等) および `openai-codex` は
/// API key 不要のため `false` を返す。それ以外のリモートエンドポイントは `true`。
pub(crate) fn should_confirm_empty_api_key(provider_id: &str, base_url: &str) -> bool {
    !provider_allows_empty_api_key(provider_id, base_url)
}

/// 指定したプロバイダー ID が preset に存在しない (Custom 扱い) か判定する。
pub(crate) fn is_custom_provider(provider_id: &str) -> bool {
    find_provider_preset(provider_id).is_none()
}

/// モデルをフリーテキスト入力すべきか判定する。
///
/// Custom プロバイダーには preset models リストがないため、`is_custom_provider` と同等。
pub(crate) fn should_ask_model_as_free_text(provider_id: &str) -> bool {
    is_custom_provider(provider_id)
}

fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

struct PrefillValues {
    agent_label: String,
    provider_id: String,
    base_url: String,
    model: String,
    web_enabled: bool,
    discord_enabled: bool,
    telegram_enabled: bool,
}

fn extract_prefill(existing: &ExistingConfig) -> PrefillValues {
    PrefillValues {
        agent_label: existing
            .root
            .as_ref()
            .and_then(root_agent_label)
            .unwrap_or_default(),
        provider_id: existing.fields.get("PROVIDER").cloned().unwrap_or_default(),
        base_url: existing.fields.get("BASE_URL").cloned().unwrap_or_default(),
        model: existing.fields.get("MODEL").cloned().unwrap_or_default(),
        web_enabled: existing
            .root
            .as_ref()
            .and_then(root_web_enabled)
            .unwrap_or(true),
        discord_enabled: existing
            .fields
            .get("DISCORD_ENABLED")
            .map(|v| is_truthy(v.as_str()))
            .unwrap_or(false),
        telegram_enabled: existing
            .fields
            .get("TELEGRAM_ENABLED")
            .map(|v| is_truthy(v.as_str()))
            .unwrap_or(false),
    }
}

fn root_agent_label(root: &yaml_serde::Value) -> Option<String> {
    let map = root.as_mapping()?;
    let default_agent = map.get(yaml_str("default_agent"))?.as_str()?;
    map.get(yaml_str("agents"))?
        .as_mapping()?
        .get(yaml_str(default_agent))?
        .as_mapping()?
        .get(yaml_str("label"))?
        .as_str()
        .map(str::to_string)
}

fn root_web_enabled(root: &yaml_serde::Value) -> Option<bool> {
    root.as_mapping()?
        .get(yaml_str("channels"))?
        .as_mapping()?
        .get(yaml_str("web"))?
        .as_mapping()?
        .get(yaml_str("enabled"))?
        .as_bool()
}

fn yaml_str(key: &str) -> yaml_serde::Value {
    yaml_serde::Value::String(key.to_string())
}

fn is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

fn provider_default_index(provider_id: &str) -> usize {
    if provider_id.is_empty() {
        return 0;
    }
    PROVIDER_PRESETS
        .iter()
        .position(|p| p.id == provider_id)
        .unwrap_or(PROVIDER_PRESETS.len())
}

fn provider_select_items() -> Vec<String> {
    PROVIDER_PRESETS
        .iter()
        .map(|p| p.label.to_string())
        .chain(std::iter::once("Custom".to_string()))
        .collect()
}

fn model_select_items(provider_id: &str) -> Vec<String> {
    let mut items: Vec<String> = find_provider_preset(provider_id)
        .map(|p| p.models.iter().map(|m| m.to_string()).collect())
        .unwrap_or_default();
    if !items.is_empty() {
        items.push(CUSTOM_MODEL_LABEL.to_string());
    }
    items
}

fn load_existing(
    source: &dyn PromptSource,
    sink: &dyn OutputSink,
    config_path: &Path,
) -> Result<ExistingConfig, SetupWizardError> {
    let yaml_text = match fs::read_to_string(config_path) {
        Ok(text) => text,
        Err(_) => {
            return Ok(ExistingConfig {
                fields: HashMap::new(),
                root: None,
            });
        }
    };

    match parse_existing_config(&yaml_text) {
        Ok(config) => Ok(config),
        Err(e) => {
            sink.println(&format!("WARNING: {e}"));
            if source
                .confirm("Continue with empty defaults?", false)
                .map_err(SetupWizardError::Prompt)?
            {
                Ok(ExistingConfig {
                    fields: HashMap::new(),
                    root: None,
                })
            } else {
                Err(SetupWizardError::Aborted)
            }
        }
    }
}

fn collect_inputs(
    source: &dyn PromptSource,
    prefill: &PrefillValues,
) -> Result<SetupInputs, SetupWizardError> {
    let agent_label = prompt_agent_label(source, &prefill.agent_label)?;
    let (provider_id, base_url) = prompt_provider(source, &agent_label, prefill)?;
    let model = prompt_model(source, &provider_id, &prefill.model)?;
    let api_key = prompt_api_key(source, &provider_id, &base_url)?;
    let web_enabled = prompt_web(source, prefill.web_enabled)?;
    let (discord_enabled, discord_bot_token) = prompt_discord(source, prefill.discord_enabled)?;
    let (telegram_enabled, telegram_bot_token) = prompt_telegram(source, prefill.telegram_enabled)?;

    Ok(SetupInputs {
        agent_label,
        provider_id,
        base_url,
        model,
        api_key,
        web_enabled,
        discord_enabled,
        discord_bot_token,
        telegram_enabled,
        telegram_bot_token,
    })
}

fn prompt_agent_label(
    source: &dyn PromptSource,
    default: &str,
) -> Result<String, SetupWizardError> {
    let prompt = "Name your agent (e.g. Partner, Companion, Assistant):";
    let input = source
        .text(prompt, default)
        .map_err(SetupWizardError::Prompt)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        if default.is_empty() {
            Ok("Default".to_string())
        } else {
            Ok(default.to_string())
        }
    } else {
        Ok(trimmed.to_string())
    }
}

fn prompt_provider(
    source: &dyn PromptSource,
    agent_label: &str,
    prefill: &PrefillValues,
) -> Result<(String, String), SetupWizardError> {
    let items = provider_select_items();
    let label = format!("Choose the LLM provider for {agent_label}:");
    let default_idx = provider_default_index(&prefill.provider_id);
    let idx = source
        .select(&label, &items, default_idx)
        .map_err(SetupWizardError::Prompt)?;

    if idx >= PROVIDER_PRESETS.len() {
        let url = source
            .text(
                "Enter the base_url (e.g. https://api.example.com/v1):",
                &prefill.base_url,
            )
            .map_err(SetupWizardError::Prompt)?;
        Ok(("custom".to_string(), url.trim().to_string()))
    } else {
        let preset = &PROVIDER_PRESETS[idx];
        Ok((preset.id.to_string(), preset.default_base_url.to_string()))
    }
}

fn prompt_model(
    source: &dyn PromptSource,
    provider_id: &str,
    default: &str,
) -> Result<String, SetupWizardError> {
    if should_ask_model_as_free_text(provider_id) {
        let input = source
            .text(
                "Enter the model name (e.g. gpt-4o, claude-3-opus):",
                default,
            )
            .map_err(SetupWizardError::Prompt)?;
        let trimmed = input.trim();
        if trimmed.is_empty() && !default.is_empty() {
            Ok(default.to_string())
        } else {
            Ok(trimmed.to_string())
        }
    } else {
        let items = model_select_items(provider_id);
        if items.is_empty() {
            let input = source
                .text("Enter the model name (e.g. gpt-4o):", default)
                .map_err(SetupWizardError::Prompt)?;
            return Ok(input.trim().to_string());
        }
        let default_idx = items
            .iter()
            .position(|m| m == default)
            .unwrap_or_else(|| items.len().saturating_sub(1));
        let idx = source
            .select("Choose the model to use:", &items, default_idx)
            .map_err(SetupWizardError::Prompt)?;
        if items[idx] == CUSTOM_MODEL_LABEL {
            let input = source
                .text("Enter the model name:", default)
                .map_err(SetupWizardError::Prompt)?;
            return Ok(input.trim().to_string());
        }
        Ok(items[idx].clone())
    }
}

fn prompt_api_key(
    source: &dyn PromptSource,
    provider_id: &str,
    base_url: &str,
) -> Result<String, SetupWizardError> {
    let provider_label = provider_label_for(provider_id);
    let prompt = format!(
        "Enter the API key for {provider_label} (input is hidden).\n\
         For local endpoints (Ollama/LMStudio), leave it empty and press Enter:"
    );
    loop {
        let key = source.password(&prompt).map_err(SetupWizardError::Prompt)?;
        if key.trim().is_empty() && should_confirm_empty_api_key(provider_id, base_url) {
            let confirm = format!(
                "WARNING: {provider_label} usually requires an API key. \
                 Proceed with an empty key?"
            );
            if !source
                .confirm(&confirm, false)
                .map_err(SetupWizardError::Prompt)?
            {
                continue;
            }
        }
        return Ok(key);
    }
}

fn prompt_web(source: &dyn PromptSource, default: bool) -> Result<bool, SetupWizardError> {
    source
        .confirm(
            &format!(
                "Enable the Web UI?\n\
                 You can access it at {WEB_UI_URL} from your browser."
            ),
            default,
        )
        .map_err(SetupWizardError::Prompt)
}

fn prompt_discord(
    source: &dyn PromptSource,
    default: bool,
) -> Result<(bool, String), SetupWizardError> {
    let enabled = source
        .confirm("Configure a Discord bot?", default)
        .map_err(SetupWizardError::Prompt)?;
    if enabled {
        let token = source
            .password("Enter the Discord bot token (input is hidden):")
            .map_err(SetupWizardError::Prompt)?;
        Ok((true, token))
    } else {
        Ok((false, String::new()))
    }
}

fn prompt_telegram(
    source: &dyn PromptSource,
    default: bool,
) -> Result<(bool, String), SetupWizardError> {
    let enabled = source
        .confirm("Configure a Telegram bot?", default)
        .map_err(SetupWizardError::Prompt)?;
    if enabled {
        let token = source
            .password("Enter the Telegram bot token (input is hidden):")
            .map_err(SetupWizardError::Prompt)?;
        Ok((true, token))
    } else {
        Ok((false, String::new()))
    }
}

fn save_and_finish(
    sink: &dyn OutputSink,
    inputs: &SetupInputs,
    existing: &ExistingConfig,
    config_path: &Path,
) -> Result<(), SetupWizardError> {
    let (backup_path, _summary) =
        save_config(inputs, existing.root.as_ref(), config_path).map_err(SetupWizardError::Save)?;

    sink.println(&build_additional_options_text());
    let path_str = config_path.to_string_lossy();
    sink.println(&build_done_message(
        inputs,
        &path_str,
        backup_path.as_deref(),
    ));

    Ok(())
}

/// trait 抽象化されたプロンプト/シンクを用いて wizard フロー全体を実行する。
///
/// Welcome → Q1〜Q7 → Review → Save → Additional Options → Done の順次制御を行う。
/// Review で no 選択時は StartOver / Abort / SaveAnyway の3択へ分岐する。
/// 既存 YAML のパースエラー時は warn 表示 + Y/N 確認を行う。
///
/// # Errors
///
/// - ユーザーが Abort を選択した場合
/// - 既存 YAML パースエラー時にユーザーが N を選択した場合
/// - プロンプト入力エラー
/// - 設定保存エラー
pub(crate) fn run_with_source_and_sink(
    source: &dyn PromptSource,
    sink: &dyn OutputSink,
    config_path: Option<PathBuf>,
) -> Result<(), SetupWizardError> {
    let resolved_path = match config_path {
        Some(path) => path,
        None => {
            default_config_path().map_err(|e| SetupWizardError::ConfigResolve(e.to_string()))?
        }
    };

    sink.println(&build_welcome_message(&resolved_path));

    let existing = load_existing(source, sink, &resolved_path)?;
    let prefill = extract_prefill(&existing);

    loop {
        let inputs = collect_inputs(source, &prefill)?;

        if let Err(message) = validate_inputs(&inputs) {
            sink.println(&format!("Invalid input: {message}"));
            sink.println("Please answer the questions again.");
            continue;
        }

        sink.println(&build_review_summary(&inputs));

        if source
            .confirm("Save configuration?", true)
            .map_err(SetupWizardError::Prompt)?
        {
            return save_and_finish(sink, &inputs, &existing, &resolved_path);
        }

        let choices = vec![
            "Start over (back to Agent Label)".to_string(),
            "Abort (exit without saving)".to_string(),
            "Save anyway".to_string(),
        ];
        let idx = source
            .select("What would you like to do?", &choices, 0)
            .map_err(SetupWizardError::Prompt)?;
        match review_decision_from_index(idx) {
            ReviewDecision::StartOver => continue,
            ReviewDecision::Abort => {
                sink.println("Setup aborted. No configuration was saved.");
                return Err(SetupWizardError::Aborted);
            }
            ReviewDecision::SaveAnyway => {
                return save_and_finish(sink, &inputs, &existing, &resolved_path);
            }
        }
    }
}

/// dialoguer ベースのプロンプト/シンクを使用して wizard を実行する thin wrapper。
///
/// # Errors
///
/// [`run_with_source_and_sink`] に準ずる。
pub(crate) fn run(config_path: Option<PathBuf>) -> Result<(), SetupWizardError> {
    run_with_source_and_sink(
        &DialoguerPromptSource::new(),
        &DialoguerOutputSink::new(),
        config_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_inputs() -> SetupInputs {
        SetupInputs {
            agent_label: "Partner".into(),
            provider_id: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-5.2".into(),
            api_key: "sk-test-key-1234".into(),
            web_enabled: true,
            discord_enabled: false,
            discord_bot_token: String::new(),
            telegram_enabled: false,
            telegram_bot_token: String::new(),
        }
    }

    #[test]
    fn review_decision_from_index_maps_correctly() {
        assert_eq!(review_decision_from_index(0), ReviewDecision::StartOver);
        assert_eq!(review_decision_from_index(1), ReviewDecision::Abort);
        assert_eq!(review_decision_from_index(2), ReviewDecision::SaveAnyway);
    }

    #[test]
    fn build_review_summary_renders_all_fields() {
        let inputs = sample_inputs();
        let summary = build_review_summary(&inputs);
        let agent_id = slugify_agent_id(&inputs.agent_label);
        let masked_key = format_api_key_for_review(&inputs.api_key);

        assert!(summary.contains(&inputs.agent_label));
        assert!(summary.contains(&agent_id));
        assert!(summary.contains(&inputs.provider_id));
        assert!(summary.contains(&inputs.base_url));
        assert!(summary.contains(&inputs.model));
        assert!(summary.contains(&masked_key));
        assert!(summary.contains("Web:"));
        assert!(summary.contains("Discord:"));
        assert!(summary.contains("Telegram:"));
    }

    #[test]
    fn build_additional_options_text_includes_all_categories() {
        let text = build_additional_options_text();

        assert!(text.contains("System:"));
        assert!(text.contains("Web UI:"));
        assert!(text.contains("Channels:"));
        assert!(text.contains("Subsystems:"));
    }

    #[test]
    fn build_done_message_includes_next_steps_and_channel_hints() {
        let inputs = sample_inputs();
        let config_path = "~/.egopulse/egopulse.config.yaml";
        let message = build_done_message(&inputs, config_path, None);

        assert!(message.contains(config_path));
        assert!(message.contains("egopulse chat"));
        assert!(message.contains("egopulse gateway install"));
        assert!(message.contains("agents"));
        assert!(message.contains(WEB_UI_URL));
        assert!(!message.contains("Backup:"));

        let backup_message = build_done_message(&inputs, config_path, Some("~/backup.yaml"));
        assert!(backup_message.contains("Backup:"));
        assert!(backup_message.contains("~/backup.yaml"));

        let mut discord_inputs = sample_inputs();
        discord_inputs.discord_enabled = true;
        let discord_message = build_done_message(&discord_inputs, config_path, None);
        assert!(discord_message.contains("docs/channels.md"));
    }

    #[test]
    fn should_confirm_empty_api_key_returns_false_for_localhost() {
        assert!(!should_confirm_empty_api_key(
            "ollama",
            "http://127.0.0.1:11434/v1"
        ));
        assert!(!should_confirm_empty_api_key(
            "lmstudio",
            "http://localhost:1234/v1"
        ));
    }

    #[test]
    fn should_confirm_empty_api_key_returns_true_for_remote() {
        assert!(should_confirm_empty_api_key(
            "openai",
            "https://api.openai.com/v1"
        ));
        assert!(should_confirm_empty_api_key(
            "deepseek",
            "https://api.deepseek.com/v1"
        ));
    }

    #[test]
    fn is_custom_provider_returns_true_only_for_custom() {
        assert!(is_custom_provider("custom"));
        assert!(!is_custom_provider("openai"));
        assert!(!is_custom_provider("ollama"));
    }

    #[test]
    fn should_ask_model_as_free_text_returns_true_only_for_custom() {
        assert!(should_ask_model_as_free_text("custom"));
        assert!(!should_ask_model_as_free_text("openai"));
        assert!(!should_ask_model_as_free_text("deepseek"));
    }

    #[test]
    fn model_select_items_appends_custom_model_choice() {
        let items = model_select_items("openai");

        assert_eq!(items.last().map(String::as_str), Some(CUSTOM_MODEL_LABEL));
        assert!(items.iter().any(|item| item == "gpt-5.2"));
    }

    #[test]
    fn model_select_items_returns_empty_for_custom_provider() {
        let items = model_select_items("custom");

        assert!(items.is_empty());
    }

    use crate::setup::prompts::test_mocks::{MockPromptSource, VecOutputSink};
    use serial_test::serial;

    /// Ollama is at index 3 in PROVIDER_PRESETS.
    const OLLAMA_INDEX: usize = 3;

    fn setup_happy_path(source: &MockPromptSource) {
        source
            .expect_text("Name your agent", "Partner")
            .expect_select("Choose the LLM provider", OLLAMA_INDEX)
            .expect_select("Choose the model", 0)
            .expect_password("API key", "")
            .expect_confirm("Web UI", true)
            .expect_confirm("Discord", false)
            .expect_confirm("Telegram", false);
    }

    fn assert_config_saved(config_path: &std::path::Path) {
        assert!(config_path.exists(), "config file must be created");
        let content = std::fs::read_to_string(config_path).expect("read saved config");
        assert!(
            content.contains("default_provider"),
            "saved config must contain default_provider"
        );
    }

    #[test]
    #[serial]
    fn wizard_allows_custom_model_for_preset_provider() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let source = MockPromptSource::new();
        let sink = VecOutputSink::new();

        let openai_custom_model_index = model_select_items("openai").len() - 1;
        source
            .expect_text("Name your agent", "Partner")
            .expect_select("Choose the LLM provider", 0)
            .expect_select("Choose the model", openai_custom_model_index)
            .expect_text("Enter the model name", "gpt-next-preview")
            .expect_password("API key", "sk-test-key")
            .expect_confirm("Web UI", true)
            .expect_confirm("Discord", false)
            .expect_confirm("Telegram", false)
            .expect_confirm("Save configuration", true);

        run_with_source_and_sink(&source, &sink, Some(config_path.clone()))
            .expect("wizard should save custom model for preset provider");

        let content = std::fs::read_to_string(config_path).expect("read saved config");
        assert!(
            content.contains("gpt-next-preview"),
            "custom model must be saved in the generated config"
        );
    }

    #[test]
    #[serial]
    fn prefill_defaults_uses_existing_config_values() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");
        std::fs::write(
            &config_path,
            "default_provider: ollama\n\
             default_agent: partner\n\
             providers:\n\
             \x20 ollama:\n\
             \x20   label: Ollama\n\
             \x20   base_url: http://127.0.0.1:11434/v1\n\
             \x20   default_model: llama3.2\n\
             agents:\n\
             \x20 partner:\n\
             \x20   label: Partner\n\
             channels:\n\
             \x20 web:\n\
             \x20   enabled: true\n\
             \x20 discord:\n\
             \x20   enabled: false\n\
             \x20 telegram:\n\
             \x20   enabled: false\n",
        )
        .expect("write existing config");

        let source = MockPromptSource::new();
        let sink = VecOutputSink::new();

        setup_happy_path(&source);
        source.expect_confirm("Save configuration", true);

        run_with_source_and_sink(&source, &sink, Some(config_path.clone()))
            .expect("wizard should succeed");

        let text_defs = source.text_defaults();
        let label_default = text_defs
            .iter()
            .find(|(l, _)| l.contains("Name your agent"))
            .map(|(_, d)| d.clone())
            .expect("agent label text default must be recorded");
        assert_eq!(label_default, "Partner");

        let confirm_defs = source.confirm_defaults();
        let web_default = confirm_defs
            .iter()
            .find(|(l, _)| l.contains("Web UI"))
            .map(|(_, d)| *d)
            .expect("web confirm default must be recorded");
        assert!(web_default);

        let select_defs = source.select_defaults();
        let provider_default = select_defs
            .iter()
            .find(|(l, _)| l.contains("Choose the LLM provider"))
            .map(|(_, d)| *d)
            .expect("provider select default must be recorded");
        assert_eq!(
            provider_default, OLLAMA_INDEX,
            "provider select default must point at the existing provider preset"
        );

        assert_config_saved(&config_path);
    }

    #[test]
    #[serial]
    fn wizard_review_startover_returns_to_q1() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let source = MockPromptSource::new();
        let sink = VecOutputSink::new();

        // Round 1: review -> no -> StartOver
        setup_happy_path(&source);
        source
            .expect_confirm("Save configuration", false)
            .expect_select("What would you like to do?", 0);

        // Round 2: review -> yes -> save
        setup_happy_path(&source);
        source.expect_confirm("Save configuration", true);

        run_with_source_and_sink(&source, &sink, Some(config_path.clone()))
            .expect("wizard should succeed after StartOver loop");

        assert_config_saved(&config_path);
    }

    #[test]
    #[serial]
    fn wizard_review_abort_exits_without_save() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let source = MockPromptSource::new();
        let sink = VecOutputSink::new();

        setup_happy_path(&source);
        source
            .expect_confirm("Save configuration", false)
            .expect_select("What would you like to do?", 1);

        let result = run_with_source_and_sink(&source, &sink, Some(config_path.clone()));

        assert!(result.is_err(), "Abort must return Err");
        assert!(
            !config_path.exists(),
            "config file must NOT be created on Abort"
        );
    }

    #[test]
    #[serial]
    fn wizard_review_save_anyway_writes_config() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let source = MockPromptSource::new();
        let sink = VecOutputSink::new();

        setup_happy_path(&source);
        source
            .expect_confirm("Save configuration", false)
            .expect_select("What would you like to do?", 2);

        run_with_source_and_sink(&source, &sink, Some(config_path.clone()))
            .expect("wizard should succeed via SaveAnyway");

        assert_config_saved(&config_path);
    }

    #[test]
    #[serial]
    fn wizard_review_yes_saves_directly() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let source = MockPromptSource::new();
        let sink = VecOutputSink::new();

        setup_happy_path(&source);
        source.expect_confirm("Save configuration", true);

        run_with_source_and_sink(&source, &sink, Some(config_path.clone()))
            .expect("wizard should succeed on direct yes");

        assert_config_saved(&config_path);

        let output = sink.joined();
        assert!(
            output.contains("Configuration saved"),
            "Done message must appear in output"
        );
    }

    #[test]
    #[serial]
    fn wizard_parse_error_decline_aborts() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");
        std::fs::write(&config_path, "default_provider: [unclosed").expect("write broken yaml");

        let source = MockPromptSource::new();
        let sink = VecOutputSink::new();

        source.expect_confirm("Continue with empty defaults", false);

        let result = run_with_source_and_sink(&source, &sink, Some(config_path.clone()));

        assert!(result.is_err(), "decline on parse error must return Err");
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            "default_provider: [unclosed",
            "original broken config must remain untouched"
        );

        let output = sink.joined();
        assert!(
            output.contains("WARNING"),
            "warning must be displayed on parse error"
        );
    }

    #[test]
    #[serial]
    fn wizard_parse_error_accept_continues() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");
        std::fs::write(&config_path, "default_provider: [unclosed").expect("write broken yaml");

        let source = MockPromptSource::new();
        let sink = VecOutputSink::new();

        source.expect_confirm("Continue with empty defaults", true);
        setup_happy_path(&source);
        source.expect_confirm("Save configuration", true);

        run_with_source_and_sink(&source, &sink, Some(config_path.clone()))
            .expect("wizard should continue after accepting parse error");

        assert_config_saved(&config_path);
    }
}

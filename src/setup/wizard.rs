//! Setup wizard のメッセージビルダーと分岐判断純粋関数。
//!
//! dialoguer に依存しない純粋関数群。Step 8 の wizard フロー統合時に
//! これらの関数を並べて呼び出すことで、回帰リスクを最小化する。

use crate::llm::codex_auth::provider_allows_empty_api_key;
use crate::setup::inputs::SetupInputs;
use crate::setup::prompts::format_api_key_for_review;
use crate::setup::provider::find_provider_preset;
use crate::setup::slugify::slugify_agent_id;

/// Web UI のデフォルトアクセス URL。
const WEB_UI_URL: &str = "http://127.0.0.1:10961";

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
        "About to save the configuration file with the following values:".to_string(),
        String::new(),
        format!("  Agent:    {} (id: {})", inputs.agent_label, agent_id),
        format!("  Provider: {} ({})", inputs.provider_id, inputs.base_url),
        format!("  Model:    {}", inputs.model),
        format!("  API Key:  {}", masked_key),
        format!("  Web:      {}", web_line),
        format!("  Discord:  {}", enabled_label(inputs.discord_enabled)),
        format!("  Telegram: {}", enabled_label(inputs.telegram_enabled)),
        String::new(),
        "Save? (Y/n)".to_string(),
    ]
    .join("\n")
}

/// Additional Options ステップの固定案内テキストを返す (`docs/setup-redesign.md §4.2`)。
pub(crate) fn build_additional_options_text() -> String {
    [
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
    let mut lines = vec![format!("Configuration saved: {config_path}")];

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
}

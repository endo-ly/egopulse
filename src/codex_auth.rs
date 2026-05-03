//! OpenAI Codex OAuth トークン解決・管理。
//!
//! `codex login` が生成する `~/.codex/auth.json` から access_token を読み取り、
//! JWT の有効期限切れを検知して refresh_token による自動更新を行う。

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::error::ConfigError;

/// OpenAI Codex プロバイダーの識別子。
pub(crate) const CODEX_PROVIDER: &str = "openai-codex";

/// OpenAI トークンリフレッシュエンドポイント。
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";

/// Codex CLI の OAuth client ID。
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// `~/.codex/auth.json` のシリアライズ構造。
#[derive(Debug, Deserialize)]
struct CodexAuthFile {
    tokens: Option<CodexAuthTokens>,
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexAuthTokens {
    access_token: Option<String>,
    #[expect(dead_code)]
    refresh_token: Option<String>,
    account_id: Option<String>,
}

/// リフレッシュレスポンス。
#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
}

/// 解決済みの Codex 認証情報。
#[derive(Debug, Clone)]
pub(crate) struct CodexAuth {
    pub(crate) bearer_token: String,
    pub(crate) account_id: Option<String>,
}

/// `~/.codex/auth.json` のデフォルトパスを返す。
///
/// `CODEX_HOME` 環境変数が設定されていればそれを優先し、未設定なら `~/.codex/auth.json` を返す。
pub(crate) fn default_codex_auth_path() -> PathBuf {
    let base = std::env::var("CODEX_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .as_deref()
        .map(expand_tilde)
        .unwrap_or_else(|| expand_tilde("~/.codex"));
    Path::new(&base).join("auth.json")
}

/// `openai-codex` プロバイダーかどうかを判定する。
pub(crate) fn is_codex_provider(provider: &str) -> bool {
    provider.eq_ignore_ascii_case(CODEX_PROVIDER)
}

/// api_key 未設定を許容するプロバイダーかどうかを判定する。
///
/// ローカルホスト向けプロバイダーに加えて、`openai-codex` もトークンを
/// `~/.codex/auth.json` から読み取るため api_key 不要。
pub(crate) fn provider_allows_empty_api_key(provider: &str, base_url: &str) -> bool {
    if is_codex_provider(provider) {
        return true;
    }
    is_local_url(base_url)
}

/// Codex 認証トークンを解決する。
///
/// 解決優先度:
/// 1. `OPENAI_CODEX_ACCESS_TOKEN` 環境変数
/// 2. `~/.codex/auth.json` の `tokens.access_token`
/// 3. `~/.codex/auth.json` の `OPENAI_API_KEY` フィールド
///
/// # Errors
///
/// いずれのソースからもトークンを取得できない場合、`ConfigError` を返す。
pub(crate) fn resolve_codex_auth() -> Result<CodexAuth, ConfigError> {
    if let Ok(token) = std::env::var("OPENAI_CODEX_ACCESS_TOKEN") {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            return Ok(CodexAuth {
                bearer_token: trimmed.to_string(),
                account_id: None,
            });
        }
    }

    {
        let guard = AUTH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((instant, cached)) = guard.as_ref() {
            if instant.elapsed() < CODEX_AUTH_TTL {
                return Ok(cached.clone());
            }
        }
    }

    let auth = resolve_codex_auth_from_file()?;

    {
        let mut guard = AUTH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some((std::time::Instant::now(), auth.clone()));
    }

    Ok(auth)
}

fn resolve_codex_auth_from_file() -> Result<CodexAuth, ConfigError> {
    let auth_path = default_codex_auth_path();
    if auth_path.exists() {
        let content = std::fs::read_to_string(&auth_path).map_err(|source| {
            ConfigError::SecretRefUnresolved {
                reference: format!("failed to read {}: {source}", auth_path.display()),
            }
        })?;
        let parsed: CodexAuthFile =
            serde_json::from_str(&content).map_err(|error| ConfigError::SecretRefUnresolved {
                reference: format!("failed to parse {}: {error}", auth_path.display()),
            })?;

        if let Some(token) = parsed
            .tokens
            .as_ref()
            .and_then(|t| t.access_token.as_deref())
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            return Ok(CodexAuth {
                bearer_token: token.to_string(),
                account_id: parsed
                    .tokens
                    .as_ref()
                    .and_then(|t| t.account_id.as_deref())
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(String::from),
            });
        }

        if let Some(api_key) = parsed
            .openai_api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            return Ok(CodexAuth {
                bearer_token: api_key.to_string(),
                account_id: None,
            });
        }
    }

    Err(ConfigError::SecretRefUnresolved {
        reference: format!(
            "openai-codex requires ~/.codex/auth.json (access token or OPENAI_API_KEY), \
             or OPENAI_CODEX_ACCESS_TOKEN. Run `codex login` first. (expected: {})",
            auth_path.display(),
        ),
    })
}

/// JWT の `exp` クレームが現在時刻を過ぎているかを判定する。
///
/// 不正なフォーマットの場合は `false` を返す（エラーでブロックしない）。
pub(crate) fn is_jwt_expired(token: &str) -> bool {
    let payload = match extract_jwt_payload(token) {
        Some(p) => p,
        None => return false,
    };
    let exp = payload.get("exp").and_then(|v| v.as_i64());
    match exp {
        Some(ts) => now_timestamp() >= ts,
        None => false,
    }
}

/// プロセス内で refresh を直列化するミューテックス。
static REFRESH_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

static AUTH_CACHE: LazyLock<std::sync::Mutex<Option<(std::time::Instant, CodexAuth)>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

const CODEX_AUTH_TTL: std::time::Duration = std::time::Duration::from_secs(300);

#[cfg(test)]
fn clear_auth_cache() {
    *AUTH_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// access_token の有効期限が切れている場合、refresh_token を使って更新する。
///
/// プロセス内ミューテックスで直列化し、並列リクエストによる二重 refresh や
/// auth.json の競合書き込みを防ぐ。更新に成功すると一時ファイル経由で原子的に
/// auth.json を上書きする。失敗した場合はサイレントに戻る（起動をブロックしない）。
pub(crate) async fn refresh_if_needed(http: &reqwest::Client) {
    let _guard = REFRESH_LOCK.lock().await;

    let auth_path = default_codex_auth_path();
    if !auth_path.exists() {
        return;
    }

    let content = match std::fs::read_to_string(&auth_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return,
    };

    let tokens = match parsed.get("tokens").and_then(|t| t.as_object()) {
        Some(obj) => obj,
        None => return,
    };

    let access = match tokens
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
    {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => return,
    };
    let refresh = match tokens
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
    {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => return,
    };

    if !is_jwt_expired(&access) {
        return;
    }

    tracing::debug!("codex access_token expired, refreshing");

    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh,
        "client_id": CLIENT_ID,
    });

    let result = http
        .post(REFRESH_URL)
        .header("content-type", "application/json")
        .body(body.to_string())
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;

    let resp = match result {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::warn!(status = %r.status(), "codex token refresh failed");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "codex token refresh request failed");
            return;
        }
    };

    let refresh_resp: RefreshResponse = match resp.json().await {
        Ok(r) => r,
        Err(error) => {
            tracing::warn!(%error, "codex token refresh response parse failed");
            return;
        }
    };

    if refresh_resp.access_token.trim().is_empty() {
        return;
    }

    if let Some(tokens_obj) = parsed.get_mut("tokens").and_then(|t| t.as_object_mut()) {
        tokens_obj.insert(
            "access_token".to_string(),
            serde_json::Value::String(refresh_resp.access_token),
        );
        if let Some(new_refresh) = refresh_resp.refresh_token {
            if !new_refresh.trim().is_empty() {
                tokens_obj.insert(
                    "refresh_token".to_string(),
                    serde_json::Value::String(new_refresh),
                );
            }
        }
    }

    let updated = match serde_json::to_string_pretty(&parsed) {
        Ok(s) => s,
        Err(error) => {
            tracing::warn!(%error, "failed to serialize refreshed codex auth");
            return;
        }
    };

    write_atomic(&auth_path, &updated);

    AUTH_CACHE.lock().unwrap_or_else(|e| e.into_inner()).take();
}

/// 一時ファイルに書き込み後にリネームして原子的に更新する。
fn write_atomic(path: &Path, content: &str) {
    let temp_path = path.with_extension("tmp");
    if let Err(error) = std::fs::write(&temp_path, content) {
        tracing::warn!(path = %temp_path.display(), %error, "failed to write temp codex auth");
        return;
    }
    if let Err(error) = std::fs::rename(&temp_path, path) {
        tracing::warn!(
            from = %temp_path.display(),
            to = %path.display(),
            %error,
            "failed to rename temp codex auth"
        );
        let _ = std::fs::remove_file(&temp_path);
    }
}

fn extract_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let mut padded = payload.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(padded.as_bytes())
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn now_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn expand_tilde(input: &str) -> String {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    if input == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
    }
    input.to_string()
}

fn is_local_url(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("0.0.0.0") | Some("::1")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvVarGuard;

    fn make_expired_jwt() -> String {
        // exp = 1 (1970-01-01), definitely expired
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#);
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"exp":1,"sub":"test"}"#);
        format!("{header}.{payload}.signature")
    }

    fn make_valid_jwt() -> String {
        // exp = 4102444800 (year 2100)
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"exp":4102444800,"sub":"test"}"#);
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn resolve_auth_prefers_env_var() {
        clear_auth_cache();
        let _guard = EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "env-token-xyz");
        let auth = resolve_codex_auth().expect("resolve");
        assert_eq!(auth.bearer_token, "env-token-xyz");
        assert!(auth.account_id.is_none());
    }

    #[test]
    fn resolve_auth_reads_access_token() {
        clear_auth_cache();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("auth.json"),
            r#"{"tokens":{"access_token":"file-access-token","refresh_token":"rt","account_id":"u-123"}}"#,
        )
        .expect("write");
        let _guard =
            EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "").also_set("CODEX_HOME", dir.path());

        let auth = resolve_codex_auth().expect("resolve");
        assert_eq!(auth.bearer_token, "file-access-token");
        assert_eq!(auth.account_id.as_deref(), Some("u-123"));
    }

    #[test]
    fn resolve_auth_falls_back_to_openai_api_key() {
        clear_auth_cache();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"sk-fallback-key"}"#,
        )
        .expect("write");
        let _guard =
            EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "").also_set("CODEX_HOME", dir.path());

        let auth = resolve_codex_auth().expect("resolve");
        assert_eq!(auth.bearer_token, "sk-fallback-key");
        assert!(auth.account_id.is_none());
    }

    #[test]
    fn resolve_auth_errors_when_no_source() {
        clear_auth_cache();
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard =
            EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "").also_set("CODEX_HOME", dir.path());

        let result = resolve_codex_auth();
        assert!(result.is_err());
    }

    #[test]
    fn is_jwt_expired_true_for_expired() {
        assert!(is_jwt_expired(&make_expired_jwt()));
    }

    #[test]
    fn is_jwt_expired_false_for_valid() {
        assert!(!is_jwt_expired(&make_valid_jwt()));
    }

    #[test]
    fn is_jwt_expired_false_for_malformed() {
        assert!(!is_jwt_expired("not.a.jwt"));
        assert!(!is_jwt_expired(""));
        assert!(!is_jwt_expired("abc"));
    }

    #[test]
    fn default_auth_path_uses_codex_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvVarGuard::set("CODEX_HOME", dir.path());

        let path = default_codex_auth_path();
        assert_eq!(path, dir.path().join("auth.json"));
    }

    #[test]
    fn default_auth_path_defaults_to_home_codex() {
        let _guard = EnvVarGuard::set("CODEX_HOME", "");
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());

        let path = default_codex_auth_path();
        assert_eq!(path, PathBuf::from(format!("{home}/.codex/auth.json")));
    }

    #[test]
    fn is_codex_provider_detects_codex() {
        assert!(is_codex_provider("openai-codex"));
        assert!(is_codex_provider("OPENAI-CODEX"));
        assert!(is_codex_provider("OpenAI-Codex"));
        assert!(!is_codex_provider("openai"));
        assert!(!is_codex_provider("codex"));
    }

    #[test]
    fn provider_allows_empty_api_key_includes_codex() {
        assert!(provider_allows_empty_api_key(
            "openai-codex",
            "https://chatgpt.com/backend-api/codex"
        ));
        assert!(provider_allows_empty_api_key(
            "openai-codex",
            "https://remote.example.com/v1"
        ));
        // localhost still works
        assert!(provider_allows_empty_api_key(
            "anything",
            "http://127.0.0.1:1234/v1"
        ));
        // remote non-codex requires key
        assert!(!provider_allows_empty_api_key(
            "openai",
            "https://api.openai.com/v1"
        ));
    }

    #[test]
    fn cache_returns_same_value_within_ttl() {
        clear_auth_cache();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("auth.json"),
            r#"{"tokens":{"access_token":"cached-token","refresh_token":"rt"}}"#,
        )
        .expect("write");
        let _guard =
            EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "").also_set("CODEX_HOME", dir.path());

        let first = resolve_codex_auth().expect("first");
        assert_eq!(first.bearer_token, "cached-token");

        std::fs::write(
            dir.path().join("auth.json"),
            r#"{"tokens":{"access_token":"updated-token","refresh_token":"rt"}}"#,
        )
        .expect("overwrite");

        let second = resolve_codex_auth().expect("second");
        assert_eq!(second.bearer_token, "cached-token");
    }

    #[test]
    fn cache_bypassed_for_env_var() {
        clear_auth_cache();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("auth.json"),
            r#"{"tokens":{"access_token":"file-token","refresh_token":"rt"}}"#,
        )
        .expect("write");
        {
            let _guard = EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "")
                .also_set("CODEX_HOME", dir.path());
            let file_auth = resolve_codex_auth().expect("file");
            assert_eq!(file_auth.bearer_token, "file-token");
        }

        let _env_guard = EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "env-override");
        let env_auth = resolve_codex_auth().expect("env");
        assert_eq!(env_auth.bearer_token, "env-override");
    }
}

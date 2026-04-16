//! Web 層の認証・Origin 検証を扱うモジュール。
//!
//! HTTP Bearer 認証と WebSocket 接続時の安全性チェックを提供する。

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use url::Url;

use crate::config::Config;

use super::WebState;

/// Enforces bearer-token authentication for protected HTTP routes.
pub(super) async fn require_http_auth(
    State(state): State<WebState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(expected_token) = state.app_state.config.web_auth_token() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({
                "ok": false,
                "error": "web_auth_not_configured",
                "message": "channels.web.auth_token is required"
            })),
        )
            .into_response();
    };
    if !is_authorized_bearer(&headers, expected_token) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({
                "ok": false,
                "error": "unauthorized",
                "message": "invalid web auth token"
            })),
        )
            .into_response();
    }

    next.run(request).await
}

/// Validates whether the WebSocket origin is allowed by configuration.
pub(super) fn is_ws_origin_allowed(headers: &HeaderMap, config: &Config) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(origin_parts) = OriginParts::parse_origin(origin) else {
        return false;
    };

    let allowed_origins = config.web_allowed_origins();
    if !allowed_origins.is_empty() {
        return allowed_origins
            .iter()
            .filter_map(|allowed| OriginParts::parse_origin(allowed))
            .any(|allowed| allowed == origin_parts);
    }

    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    OriginParts::parse_host(host).is_some_and(|expected| expected == origin_parts)
}

/// Validates a WebSocket auth token with constant-time comparison.
pub(super) fn is_valid_ws_token(config: &Config, token: Option<&str>) -> bool {
    let Some(expected_token) = config.web_auth_token() else {
        return false;
    };
    token.is_some_and(|candidate| constant_time_eq(candidate.trim(), expected_token))
}

fn is_authorized_bearer(headers: &HeaderMap, expected_token: &str) -> bool {
    let Some(raw_auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(token) = raw_auth.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(token.trim(), expected_token)
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let lhs = left.get(index).copied().unwrap_or(0);
        let rhs = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(lhs ^ rhs);
    }
    diff == 0
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OriginParts {
    host: String,
    port: Option<u16>,
}

impl OriginParts {
    fn parse_origin(origin: &str) -> Option<Self> {
        let parsed = Url::parse(origin).ok()?;
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        Some(Self {
            host: parsed.host_str()?.to_ascii_lowercase(),
            port: parsed.port_or_known_default(),
        })
    }

    fn parse_host(host: &str) -> Option<Self> {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(stripped) = trimmed.strip_prefix('[') {
            let (host_part, port_part) = stripped.split_once("]:")?;
            return Some(Self {
                host: host_part.to_ascii_lowercase(),
                port: port_part.parse::<u16>().ok(),
            });
        }

        let mut parts = trimmed.rsplitn(2, ':');
        let maybe_port = parts.next()?;
        let maybe_host = parts.next();
        match maybe_host {
            Some(host_part) if maybe_port.chars().all(|c| c.is_ascii_digit()) => Some(Self {
                host: host_part.to_ascii_lowercase(),
                port: maybe_port.parse::<u16>().ok(),
            }),
            _ => Some(Self {
                host: trimmed.to_ascii_lowercase(),
                port: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use crate::config::{ChannelConfig, Config, ProviderConfig};

    use super::{
        OriginParts, constant_time_eq, is_authorized_bearer, is_valid_ws_token,
        is_ws_origin_allowed,
    };

    fn config_with_web(auth_token: Option<&str>, allowed_origins: Option<Vec<String>>) -> Config {
        Config {
            default_provider: "local".to_string(),
            default_model: Some("gpt-4o-mini".to_string()),
            providers: std::collections::HashMap::from([(
                "local".to_string(),
                ProviderConfig {
                    label: "Local".to_string(),
                    base_url: "http://127.0.0.1:1234/v1".to_string(),
                    api_key: None,
                    default_model: "gpt-4o-mini".to_string(),
                    models: vec!["gpt-4o-mini".to_string()],
                },
            )]),
            state_root: ".egopulse".to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            max_session_messages: 40,
            compact_keep_recent: 20,
            channels: std::collections::HashMap::from([(
                "web".to_string(),
                ChannelConfig {
                    enabled: Some(true),
                    host: Some("127.0.0.1".to_string()),
                    port: Some(10961),
                    auth_token: auth_token.map(str::to_string),
                    allowed_origins,
                    ..Default::default()
                },
            )]),
        }
    }

    #[test]
    fn compares_token_in_constant_time() {
        assert!(constant_time_eq("secret", "secret"));
        assert!(!constant_time_eq("secret", "secrex"));
        assert!(!constant_time_eq("secret", "secret-longer"));
    }

    #[test]
    fn validates_ws_token_and_rejects_missing_server_config() {
        let open_config = config_with_web(None, None);
        assert!(!is_valid_ws_token(&open_config, None));

        let protected_config = config_with_web(Some("web-secret"), None);
        assert!(is_valid_ws_token(&protected_config, Some("web-secret")));
        assert!(is_valid_ws_token(&protected_config, Some(" web-secret ")));
        assert!(!is_valid_ws_token(&protected_config, None));
        assert!(!is_valid_ws_token(&protected_config, Some("wrong")));
    }

    #[test]
    fn validates_bearer_authorization_header() {
        let mut headers = HeaderMap::new();
        assert!(!is_authorized_bearer(&headers, "web-secret"));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer web-secret"),
        );
        assert!(is_authorized_bearer(&headers, "web-secret"));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic web-secret"),
        );
        assert!(!is_authorized_bearer(&headers, "web-secret"));
    }

    #[test]
    fn allows_ws_origin_from_allowlist() {
        let config = config_with_web(
            None,
            Some(vec!["https://egopulse.tailnet.ts.net".to_string()]),
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://egopulse.tailnet.ts.net"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("localhost:10961"));

        assert!(is_ws_origin_allowed(&headers, &config));

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example.com"),
        );
        assert!(!is_ws_origin_allowed(&headers, &config));
    }

    #[test]
    fn falls_back_to_same_host_origin_check() {
        let config = config_with_web(None, None);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:10961"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:10961"));
        assert!(is_ws_origin_allowed(&headers, &config));

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:3000"),
        );
        assert!(!is_ws_origin_allowed(&headers, &config));
    }

    #[test]
    fn rejects_missing_or_malformed_origin() {
        let config = config_with_web(None, None);
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:10961"));
        assert!(!is_ws_origin_allowed(&headers, &config));

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("chrome-extension://abc"),
        );
        assert!(!is_ws_origin_allowed(&headers, &config));
    }

    #[test]
    fn parses_host_header() {
        assert_eq!(
            OriginParts::parse_host("127.0.0.1:10961"),
            Some(OriginParts {
                host: "127.0.0.1".to_string(),
                port: Some(10961),
            })
        );
        assert_eq!(
            OriginParts::parse_host("example.com"),
            Some(OriginParts {
                host: "example.com".to_string(),
                port: None,
            })
        );
        assert_eq!(OriginParts::parse_host(""), None);
    }
}

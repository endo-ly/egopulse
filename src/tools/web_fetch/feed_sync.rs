//! Feed-sync: dynamically refresh denylist/allowlist from remote sources.
//!
//! Provides [`fetch_feed_entries`] for fetching and parsing a single feed,
//! and [`resolve_feed_sync`] for merging all configured sources into a
//! combined [`FeedSyncResult`].

use std::time::Duration;

use crate::config::web_fetch::{WebFetchFeedSource, WebFetchFeedSyncConfig};
use crate::tools::web_fetch::url_validation::normalize_host;

/// How feed entries map to config lists.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FeedMode {
    Allowlist,
    Denylist,
}

/// Feed entry format.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) enum FeedFormat {
    #[default]
    Lines,
    CsvFirstColumn,
}

/// Result of feed sync resolution.
pub(crate) struct FeedSyncResult {
    pub denylist: Vec<String>,
    pub allowlist: Vec<String>,
}

/// Errors produced during feed synchronization.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FeedSyncError {
    #[error("feed fetch failed for '{url}': {reason}")]
    FetchFailed { url: String, reason: String },
}

/// Default per-source entry cap used when `max_entries` is not specified.
const DEFAULT_MAX_ENTRIES: usize = 1000;

/// HTTP client timeout for feed requests.
const FEED_TIMEOUT: Duration = Duration::from_secs(5);

/// Fetches, parses, and normalizes entries from a single feed source.
///
/// Returns an empty vec when the source is disabled or has an empty URL.
///
/// # Errors
///
/// Returns [`FeedSyncError::FetchFailed`] when the HTTP request fails.
pub(crate) async fn fetch_feed_entries(
    source: &WebFetchFeedSource,
    client: &reqwest::Client,
) -> Result<Vec<String>, FeedSyncError> {
    if source.enabled == Some(false) {
        return Ok(Vec::new());
    }
    if source.url.trim().is_empty() {
        return Ok(Vec::new());
    }

    let body = client
        .get(&source.url)
        .send()
        .await
        .map_err(|e| FeedSyncError::FetchFailed {
            url: source.url.clone(),
            reason: e.to_string(),
        })?
        .error_for_status()
        .map_err(|e| FeedSyncError::FetchFailed {
            url: source.url.clone(),
            reason: e.to_string(),
        })?
        .text()
        .await
        .map_err(|e| FeedSyncError::FetchFailed {
            url: source.url.clone(),
            reason: e.to_string(),
        })?;

    let format = match source.format.as_deref() {
        Some("csv_first_column") => FeedFormat::CsvFirstColumn,
        _ => FeedFormat::Lines,
    };

    let max = source.max_entries.unwrap_or(DEFAULT_MAX_ENTRIES);

    let entries = parse_entries(&body, &format, max);
    Ok(entries)
}

/// Parses raw feed text into a deduplicated, normalized host list.
fn parse_entries(body: &str, format: &FeedFormat, max_entries: usize) -> Vec<String> {
    let raw: Vec<String> = match format {
        FeedFormat::Lines => body
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with('#') {
                    String::new()
                } else {
                    trimmed.to_string()
                }
            })
            .filter(|l| !l.is_empty())
            .collect(),
        FeedFormat::CsvFirstColumn => body
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with('#') {
                    return String::new();
                }
                trimmed
                    .split(',')
                    .next()
                    .map(|col| col.trim().to_string())
                    .unwrap_or_default()
            })
            .filter(|l| !l.is_empty())
            .collect(),
    };

    // Normalize, deduplicate, and cap.
    let mut normalized: Vec<String> = raw.iter().map(|h| normalize_host(h)).collect();
    normalized.retain(|h| !h.is_empty());
    normalized.sort_unstable();
    normalized.dedup();
    normalized.truncate(max_entries);
    normalized
}

/// Resolves all configured feed sources into merged denylist/allowlist entries.
///
/// When `config.enabled` is `false`, returns empty lists without error.
///
/// # Errors
///
/// Returns [`FeedSyncError`] when a source fetch fails and `config.fail_open`
/// is `false`.
pub(crate) async fn resolve_feed_sync(
    config: &WebFetchFeedSyncConfig,
) -> Result<FeedSyncResult, FeedSyncError> {
    if !config.enabled {
        return Ok(FeedSyncResult {
            denylist: Vec::new(),
            allowlist: Vec::new(),
        });
    }

    let client = reqwest::Client::builder()
        .timeout(FEED_TIMEOUT)
        .build()
        .unwrap_or_default();

    let mut denylist = Vec::new();
    let mut allowlist = Vec::new();

    for source in &config.sources {
        if source.enabled == Some(false) {
            continue;
        }

        match fetch_feed_entries(source, &client).await {
            Ok(entries) => match source.mode.as_str() {
                "allowlist" => allowlist.extend(entries),
                _ => denylist.extend(entries),
            },
            Err(e) => {
                if !config.fail_open {
                    return Err(e);
                }
                // fail_open: skip this source silently.
            }
        }
    }

    Ok(FeedSyncResult {
        denylist,
        allowlist,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::web_fetch::{WebFetchFeedSource, WebFetchFeedSyncConfig};
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_source(url: &str, mode: &str) -> WebFetchFeedSource {
        WebFetchFeedSource {
            url: url.to_string(),
            mode: mode.to_string(),
            format: None,
            enabled: None,
            max_entries: None,
        }
    }

    fn disabled_source(url: &str, mode: &str) -> WebFetchFeedSource {
        WebFetchFeedSource {
            url: url.to_string(),
            mode: mode.to_string(),
            format: None,
            enabled: Some(false),
            max_entries: None,
        }
    }

    #[tokio::test]
    async fn inline_feed_merges_denylist() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("bad.com\nevil.com"))
            .mount(&server)
            .await;

        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: false,
            sources: vec![test_source(&server.uri(), "denylist")],
        };

        let result = resolve_feed_sync(&config).await.expect("sync");

        assert!(result.denylist.contains(&"bad.com".to_string()));
        assert!(result.denylist.contains(&"evil.com".to_string()));
    }

    #[tokio::test]
    async fn inline_feed_merges_allowlist() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok.com"))
            .mount(&server)
            .await;

        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: false,
            sources: vec![test_source(&server.uri(), "allowlist")],
        };

        let result = resolve_feed_sync(&config).await.expect("sync");

        assert!(result.allowlist.contains(&"ok.com".to_string()));
    }

    #[tokio::test]
    async fn csv_first_column() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("host1.com,reason1\nhost2.com,reason2"),
            )
            .mount(&server)
            .await;

        let mut source = test_source(&server.uri(), "denylist");
        source.format = Some("csv_first_column".to_string());

        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: false,
            sources: vec![source],
        };

        let result = resolve_feed_sync(&config).await.expect("sync");

        assert!(result.denylist.contains(&"host1.com".to_string()));
        assert!(result.denylist.contains(&"host2.com".to_string()));
    }

    #[tokio::test]
    async fn skips_disabled_sources() {
        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: false,
            sources: vec![disabled_source(
                "http://unreachable.invalid/feed",
                "denylist",
            )],
        };

        let result = resolve_feed_sync(&config).await.expect("sync");

        assert!(result.denylist.is_empty());
    }

    #[tokio::test]
    async fn skips_comment_lines() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("# comment\nhost.com\n# another comment"),
            )
            .mount(&server)
            .await;

        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: false,
            sources: vec![test_source(&server.uri(), "denylist")],
        };

        let result = resolve_feed_sync(&config).await.expect("sync");

        assert_eq!(result.denylist, vec!["host.com".to_string()]);
    }

    #[tokio::test]
    async fn deduplicates_hosts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("host.com\nhost.com\nhost.com"),
            )
            .mount(&server)
            .await;

        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: false,
            sources: vec![test_source(&server.uri(), "denylist")],
        };

        let result = resolve_feed_sync(&config).await.expect("sync");

        assert_eq!(result.denylist.len(), 1);
    }

    #[tokio::test]
    async fn fails_closed_on_error() {
        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: false,
            sources: vec![test_source("http://0.0.0.0:1/feed", "denylist")],
        };

        assert!(resolve_feed_sync(&config).await.is_err());
    }

    #[tokio::test]
    async fn fails_open_on_error() {
        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: true,
            sources: vec![test_source("http://0.0.0.0:1/feed", "denylist")],
        };

        let result = resolve_feed_sync(&config).await.expect("should not fail");

        assert!(result.denylist.is_empty());
    }

    #[tokio::test]
    async fn feed_sync_disabled_is_noop() {
        let config = WebFetchFeedSyncConfig {
            enabled: false,
            fail_open: false,
            sources: vec![test_source("http://unreachable/feed", "denylist")],
        };

        let result = resolve_feed_sync(&config).await.expect("sync");

        assert!(result.denylist.is_empty());
        assert!(result.allowlist.is_empty());
    }

    #[tokio::test]
    async fn normalizes_feed_hosts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("EXAMPLE.COM\n  spaced.com  \nWith.Trailing."),
            )
            .mount(&server)
            .await;

        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: false,
            sources: vec![test_source(&server.uri(), "denylist")],
        };

        let result = resolve_feed_sync(&config).await.expect("sync");

        assert!(result.denylist.iter().all(|h| h == &h.to_ascii_lowercase()));
        assert!(result.denylist.iter().all(|h| h == h.trim()));
    }

    #[tokio::test]
    async fn max_entries_per_source() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a.com\nb.com\nc.com\nd.com"))
            .mount(&server)
            .await;

        let mut source = test_source(&server.uri(), "denylist");
        source.max_entries = Some(2);

        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: false,
            sources: vec![source],
        };

        let result = resolve_feed_sync(&config).await.expect("sync");

        assert_eq!(result.denylist.len(), 2);
    }

    #[tokio::test]
    async fn http_error_triggers_fetch_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let config = WebFetchFeedSyncConfig {
            enabled: true,
            fail_open: true,
            sources: vec![test_source(&server.uri(), "denylist")],
        };

        let result = resolve_feed_sync(&config).await;

        assert!(result.is_ok());
        let merged = result.unwrap();
        assert!(merged.denylist.is_empty());
    }
}

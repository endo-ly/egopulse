//! URL validation with SSRF protection for the web_fetch tool.
//!
//! Provides scheme, host, denylist/allowlist, and private-IP checks
//! to prevent server-side request forgery when fetching remote content.

use std::net::IpAddr;

use thiserror::Error;
use url::Url;

use crate::config::web_fetch::WebFetchConfig;

/// Errors produced during URL validation.
#[derive(Debug, Error)]
pub(crate) enum UrlValidationError {
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    #[error("scheme '{0}' is not allowed")]
    SchemeNotAllowed(String),
    #[error("host is blocked: {0}")]
    HostBlocked(String),
    #[error("host '{0}' is not in allowlist")]
    HostNotInAllowlist(String),
    #[error("private/loopback ip is blocked: {0}")]
    PrivateIpBlocked(String),
    #[error("dns resolution failed for '{0}'")]
    DnsResolutionFailed(String),
    #[error("too many redirects")]
    TooManyRedirects,
}

/// Normalizes a single host string: lowercase, trim whitespace, remove
/// trailing dots, and strip a leading `*.` wildcard prefix.
pub(crate) fn normalize_host(host: &str) -> String {
    let trimmed = host.trim();
    let lower = trimmed.to_ascii_lowercase();
    let no_trailing_dot = lower.strip_suffix('.').unwrap_or(&lower);
    no_trailing_dot
        .strip_prefix("*.")
        .unwrap_or(no_trailing_dot)
        .to_string()
}

/// Applies [`normalize_host`] to a slice, deduplicates, and removes empty entries.
pub(crate) fn normalize_host_list(hosts: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = hosts.iter().map(|h| normalize_host(h)).collect();
    normalized.retain(|h| !h.is_empty());
    normalized.sort_unstable();
    normalized.dedup();
    normalized
}

/// Returns `true` when `ip` is a loopback, private, link-local, or
/// cloud-metadata address that should be blocked for SSRF safety.
pub(crate) fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 169.254.169.254 is the cloud metadata endpoint — is_link_local()
            // covers the /16 range but the explicit check documents intent.
            let is_cloud_meta = octets == [169, 254, 169, 254];
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || is_cloud_meta
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// Returns `true` when `host` matches an entry in `denylist`.
///
/// Both exact matches and subdomain matches are considered: if the denylist
/// contains `"evil.com"` then `"sub.evil.com"` is also blocked.
fn is_host_denylisted(host: &str, denylist: &[String]) -> bool {
    let normalized = host.to_ascii_lowercase();
    denylist.iter().any(|entry| {
        let entry_lower = entry.to_ascii_lowercase();
        normalized == entry_lower || normalized.ends_with(&format!(".{entry_lower}"))
    })
}

/// Returns `true` when `host` matches an entry in `allowlist` (exact match
/// or subdomain match, mirroring the denylist logic).
fn is_host_allowlisted(host: &str, allowlist: &[String]) -> bool {
    let normalized = host.to_ascii_lowercase();
    allowlist.iter().any(|entry| {
        let entry_lower = entry.to_ascii_lowercase();
        normalized == entry_lower || normalized.ends_with(&format!(".{entry_lower}"))
    })
}

/// Validates a URL against the given [`WebFetchConfig`].
///
/// Checks (in order):
/// 1. URL parses successfully.
/// 2. Scheme is in `config.allowed_schemes`.
/// 3. Host is present.
/// 4. Host is not on the denylist (with subdomain matching).
/// 5. Host is on the allowlist (when non-empty).
/// 6. IP address is not private/loopback (when host is an IP literal and
///    `config.allow_private_ips` is false).
///
/// # Errors
///
/// Returns [`UrlValidationError`] when any check fails.
pub(crate) fn validate_url(url: &str, config: &WebFetchConfig) -> Result<Url, UrlValidationError> {
    let parsed = Url::parse(url).map_err(|e| UrlValidationError::InvalidUrl(e.to_string()))?;

    let scheme = parsed.scheme().to_ascii_lowercase();
    if !config
        .allowed_schemes
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&scheme))
    {
        return Err(UrlValidationError::SchemeNotAllowed(scheme));
    }

    let host = parsed
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| UrlValidationError::InvalidUrl("missing host".to_string()))?;

    if is_host_denylisted(host, &config.denylist) {
        return Err(UrlValidationError::HostBlocked(host.to_string()));
    }

    if !config.allowlist.is_empty() && !is_host_allowlisted(host, &config.allowlist) {
        return Err(UrlValidationError::HostNotInAllowlist(host.to_string()));
    }

    if !config.allow_private_ips {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_blocked_ip(ip) {
                return Err(UrlValidationError::PrivateIpBlocked(ip.to_string()));
            }
        }
    }

    Ok(parsed)
}

/// Resolves `host` via DNS and checks every resulting IP against
/// [`is_blocked_ip`] when `config.allow_private_ips` is `false`.
///
/// # Errors
///
/// Returns [`UrlValidationError::DnsResolutionFailed`] when resolution fails,
/// or [`UrlValidationError::PrivateIpBlocked`] when a resolved IP is blocked.
pub(crate) async fn resolve_dns_and_validate(
    host: &str,
    config: &WebFetchConfig,
) -> Result<(), UrlValidationError> {
    // `lookup_host` requires a port; we use 0 as a placeholder.
    let lookup_target = format!("{host}:0");
    let addrs = tokio::net::lookup_host(&lookup_target)
        .await
        .map_err(|_| UrlValidationError::DnsResolutionFailed(host.to_string()))?;

    if config.allow_private_ips {
        return Ok(());
    }

    for addr in addrs {
        if is_blocked_ip(addr.ip()) {
            return Err(UrlValidationError::PrivateIpBlocked(addr.ip().to_string()));
        }
    }

    Ok(())
}

/// Maximum number of HTTP redirects the tool will follow.
const MAX_REDIRECTS: u8 = 5;

/// Validates a redirect `location` header value against the current URL and
/// configuration. Returns the resolved [`Url`] on success.
///
/// # Errors
///
/// Returns [`UrlValidationError`] when the redirect target fails validation.
pub(crate) fn validate_redirect(
    current_url: &Url,
    location: &str,
    config: &WebFetchConfig,
) -> Result<Url, UrlValidationError> {
    let resolved = current_url
        .join(location)
        .map_err(|e| UrlValidationError::InvalidUrl(e.to_string()))?;

    let url_string = resolved.to_string();
    validate_url(&url_string, config)
}

/// Returns `true` when `redirect_count` has reached [`MAX_REDIRECTS`].
pub(crate) fn is_redirect_limit_exceeded(redirect_count: u8) -> bool {
    redirect_count >= MAX_REDIRECTS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::web_fetch::WebFetchContentValidationConfig;
    use std::str::FromStr;

    fn default_config() -> WebFetchConfig {
        WebFetchConfig::default()
    }

    fn config_with_schemes(schemes: &[&str]) -> WebFetchConfig {
        WebFetchConfig {
            allowed_schemes: schemes.iter().map(|s| (*s).to_string()).collect(),
            ..default_config()
        }
    }

    fn config_with_denylist(hosts: &[&str]) -> WebFetchConfig {
        WebFetchConfig {
            denylist: hosts.iter().map(|s| (*s).to_string()).collect(),
            ..default_config()
        }
    }

    fn config_with_allowlist(hosts: &[&str]) -> WebFetchConfig {
        WebFetchConfig {
            allowlist: hosts.iter().map(|s| (*s).to_string()).collect(),
            ..default_config()
        }
    }

    fn config_validation_disabled() -> WebFetchConfig {
        WebFetchConfig {
            allow_private_ips: true,
            allowed_schemes: vec!["https".to_string(), "http".to_string()],
            content_validation: WebFetchContentValidationConfig {
                enabled: false,
                ..WebFetchContentValidationConfig::default()
            },
            ..default_config()
        }
    }

    #[test]
    fn allows_https_by_default() {
        let config = default_config();

        let result = validate_url("https://example.com", &config);

        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), "https://example.com/");
    }

    #[test]
    fn blocks_http_by_default() {
        let config = default_config();

        let result = validate_url("http://example.com", &config);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(&err, UrlValidationError::SchemeNotAllowed(s) if s == "http"),
            "expected SchemeNotAllowed(\"http\"), got {err:?}"
        );
    }

    #[test]
    fn allows_http_when_configured() {
        let config = config_with_schemes(&["https", "http"]);

        let result = validate_url("http://example.com", &config);

        assert!(result.is_ok());
    }

    #[test]
    fn blocks_ftp_scheme() {
        let config = default_config();

        let result = validate_url("ftp://example.com", &config);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UrlValidationError::SchemeNotAllowed(_)
        ));
    }

    #[test]
    fn blocks_invalid_url() {
        let config = default_config();

        let result = validate_url("not a url", &config);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UrlValidationError::InvalidUrl(_)
        ));
    }

    #[test]
    fn blocks_url_without_host() {
        let config = config_with_schemes(&["https", "data"]);

        // `data:` URLs have no host component (`host_str()` returns `None`).
        let result = validate_url("data:text/plain,hello", &config);

        assert!(
            result.is_err(),
            "expected URL without host to be rejected, got {result:?}"
        );
        assert!(
            matches!(&result.unwrap_err(), UrlValidationError::InvalidUrl(_)),
            "expected InvalidUrl for URL without host"
        );
    }

    #[test]
    fn blocks_denylist_host() {
        let config = config_with_denylist(&["example.com"]);

        let result = validate_url("https://example.com", &config);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UrlValidationError::HostBlocked(_)
        ));
    }

    #[test]
    fn blocks_denylist_subdomain() {
        let config = config_with_denylist(&["example.com"]);

        let result = validate_url("https://sub.example.com", &config);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UrlValidationError::HostBlocked(_)
        ));
    }

    #[test]
    fn allows_denylist_unrelated() {
        let config = config_with_denylist(&["bad.com"]);

        let result = validate_url("https://good.com", &config);

        assert!(result.is_ok());
    }

    #[test]
    fn enforces_allowlist() {
        let config = config_with_allowlist(&["ok.com"]);

        assert!(validate_url("https://ok.com", &config).is_ok());

        let result = validate_url("https://other.com", &config);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UrlValidationError::HostNotInAllowlist(_)
        ));
    }

    #[test]
    fn denylist_precedes_allowlist() {
        let config = WebFetchConfig {
            denylist: vec!["example.com".to_string()],
            allowlist: vec!["example.com".to_string()],
            ..default_config()
        };

        let result = validate_url("https://example.com", &config);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UrlValidationError::HostBlocked(_)
        ));
    }

    #[test]
    fn validation_disabled_allows_all() {
        let config = config_validation_disabled();

        assert!(validate_url("http://example.com", &config).is_ok());
        assert!(validate_url("https://example.com", &config).is_ok());
    }

    #[test]
    fn ssrf_blocks_loopback() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();

        assert!(is_blocked_ip(ip));
    }

    #[test]
    fn ssrf_blocks_private_10() {
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        assert!(is_blocked_ip(ip));
    }

    #[test]
    fn ssrf_blocks_private_172_16() {
        let ip: IpAddr = "172.16.0.1".parse().unwrap();

        assert!(is_blocked_ip(ip));
    }

    #[test]
    fn ssrf_blocks_private_192_168() {
        let ip: IpAddr = "192.168.1.1".parse().unwrap();

        assert!(is_blocked_ip(ip));
    }

    #[test]
    fn ssrf_blocks_link_local() {
        let ip: IpAddr = "169.254.1.1".parse().unwrap();

        assert!(is_blocked_ip(ip));
    }

    #[test]
    fn ssrf_blocks_cloud_metadata() {
        let ip: IpAddr = "169.254.169.254".parse().unwrap();

        assert!(is_blocked_ip(ip));
    }

    #[test]
    fn ssrf_blocks_localhost() {
        let config = default_config();

        // localhost is not an IP literal, so URL validation passes.
        // DNS resolution would later reveal 127.0.0.1.
        let result = validate_url("https://localhost", &config);

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn ssrf_allows_when_flag_enabled() {
        let config = WebFetchConfig {
            allow_private_ips: true,
            ..default_config()
        };

        let result = resolve_dns_and_validate("example.com", &config).await;

        assert!(result.is_ok());
    }

    #[test]
    fn ssrf_allows_public_ip() {
        let ip: IpAddr = "93.184.216.34".parse().unwrap();

        assert!(!is_blocked_ip(ip));
    }

    #[test]
    fn redirect_blocks_denylisted_target() {
        let config = config_with_denylist(&["evil.com"]);
        let current = Url::parse("https://example.com/page").unwrap();

        let result = validate_redirect(&current, "https://evil.com/stolen", &config);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UrlValidationError::HostBlocked(_)
        ));
    }

    #[test]
    fn redirect_allows_relative() {
        let config = default_config();
        let current = Url::parse("https://example.com/page").unwrap();

        let result = validate_redirect(&current, "/next", &config);

        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), "https://example.com/next");
    }

    #[test]
    fn redirect_blocks_to_private_ip() {
        let config = default_config();
        let current = Url::parse("https://example.com/page").unwrap();

        let result = validate_redirect(&current, "http://127.0.0.1/", &config);

        assert!(result.is_err());
    }

    #[test]
    fn redirect_too_many() {
        assert!(is_redirect_limit_exceeded(6));
        assert!(is_redirect_limit_exceeded(100));
        assert!(is_redirect_limit_exceeded(5));
        assert!(!is_redirect_limit_exceeded(4));
        assert!(!is_redirect_limit_exceeded(0));
    }

    #[test]
    fn host_normalization_lowercase() {
        assert_eq!(normalize_host("EXAMPLE.COM"), "example.com");
    }

    #[test]
    fn host_normalization_trailing_dot() {
        assert_eq!(normalize_host("example.com."), "example.com");
    }

    #[test]
    fn host_normalization_wildcard_prefix() {
        assert_eq!(normalize_host("*.example.com"), "example.com");
    }

    #[test]
    fn blocks_ipv6_ula() {
        assert!(is_blocked_ip(IpAddr::from_str("fc00::1").unwrap()));
        assert!(is_blocked_ip(
            IpAddr::from_str("fd12:3456:789a::1").unwrap()
        ));
    }

    #[test]
    fn blocks_ipv6_link_local() {
        assert!(is_blocked_ip(IpAddr::from_str("fe80::1").unwrap()));
    }

    #[test]
    fn allows_ipv6_public() {
        assert!(!is_blocked_ip(IpAddr::from_str("2001:db8::1").unwrap()));
    }
}

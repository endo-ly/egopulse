//! WebFetch tool configuration types, defaults, normalization, and tests.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_ALLOWED_SCHEMES: &[&str] = &["https"];
const DEFAULT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_MAX_BYTES: usize = 20_000;
const DEFAULT_MAX_SCAN_BYTES: usize = 50_000;

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Top-level web_fetch configuration.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default)]
pub(crate) struct WebFetchConfig {
    /// Allowed URL schemes. Default: `["https"]`
    pub allowed_schemes: Vec<String>,
    /// Request timeout in seconds. Default: 15
    pub timeout_secs: u64,
    /// Maximum response body size in bytes. Default: 20000
    pub max_bytes: usize,
    /// Whether to allow requests to private/loopback IPs. Default: false
    pub allow_private_ips: bool,
    /// Host denylist (exact match + subdomain wildcard). Default: empty
    pub denylist: Vec<String>,
    /// Host allowlist. Default: empty (allow all)
    pub allowlist: Vec<String>,
    /// Content validation settings.
    pub content_validation: WebFetchContentValidationConfig,
    /// Feed sync settings.
    pub feed_sync: WebFetchFeedSyncConfig,
}

/// Content-validation sub-settings for the web_fetch tool.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct WebFetchContentValidationConfig {
    /// Whether content validation is enabled. Default: true
    pub enabled: bool,
    /// Strict mode: blocks on single low-confidence hit. Default: false
    pub strict_mode: bool,
    /// Maximum bytes to scan for injection. Default: 50000
    pub max_scan_bytes: usize,
}

/// Feed-sync sub-settings for the web_fetch tool.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct WebFetchFeedSyncConfig {
    /// Whether feed sync is enabled. Default: false
    pub enabled: bool,
    /// Whether to fail open (allow) when feed fetch fails. Default: false
    pub fail_open: bool,
    /// Feed sources.
    pub sources: Vec<WebFetchFeedSource>,
}

/// A single feed source used by `WebFetchFeedSyncConfig`.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub(crate) struct WebFetchFeedSource {
    pub url: String,
    /// `"allowlist"` or `"denylist"`.
    pub mode: String,
    /// `"lines"` or `"csv_first_column"`. Default: `"lines"`
    pub format: Option<String>,
    /// Whether this source is enabled. Default: true
    pub enabled: Option<bool>,
    /// Maximum entries per source. Default: 1000
    pub max_entries: Option<usize>,
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            allowed_schemes: DEFAULT_ALLOWED_SCHEMES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_bytes: DEFAULT_MAX_BYTES,
            allow_private_ips: false,
            denylist: Vec::new(),
            allowlist: Vec::new(),
            content_validation: WebFetchContentValidationConfig::default(),
            feed_sync: WebFetchFeedSyncConfig::default(),
        }
    }
}

impl Default for WebFetchContentValidationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strict_mode: false,
            max_scan_bytes: DEFAULT_MAX_SCAN_BYTES,
        }
    }
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

impl WebFetchConfig {
    /// Normalizes in place and returns `self`.
    ///
    /// * Fills `allowed_schemes` with `["https"]` when empty.
    /// * Lowercases / trims hosts in denylist/allowlist (handles `*.prefix`).
    /// * Falls back to defaults for zero-valued numeric fields.
    pub(crate) fn normalize(mut self) -> Self {
        if self.allowed_schemes.is_empty() {
            self.allowed_schemes = DEFAULT_ALLOWED_SCHEMES
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        }

        self.denylist = normalize_hosts(self.denylist);
        self.allowlist = normalize_hosts(self.allowlist);

        if self.max_bytes == 0 {
            self.max_bytes = DEFAULT_MAX_BYTES;
        }
        if self.timeout_secs == 0 {
            self.timeout_secs = DEFAULT_TIMEOUT_SECS;
        }

        self.feed_sync.sources.retain(|s| !s.url.trim().is_empty());

        self
    }
}

/// Lowercases, trims, and strips trailing dots from each host entry.
/// Handles `*.prefix` by normalizing the prefix part.
fn normalize_hosts(hosts: Vec<String>) -> Vec<String> {
    hosts
        .into_iter()
        .map(|h| {
            let trimmed = h.trim();
            let lower = trimmed.to_ascii_lowercase();
            let stripped = lower.strip_suffix('.').unwrap_or(&lower);
            stripped.to_string()
        })
        .filter(|h| !h.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_values() {
        let cfg = WebFetchConfig::default();

        assert_eq!(cfg.allowed_schemes, vec!["https"]);
        assert_eq!(cfg.timeout_secs, 15);
        assert_eq!(cfg.max_bytes, 20_000);
        assert!(!cfg.allow_private_ips);
        assert!(cfg.denylist.is_empty());
        assert!(cfg.allowlist.is_empty());
        assert!(cfg.content_validation.enabled);
        assert!(!cfg.content_validation.strict_mode);
        assert_eq!(cfg.content_validation.max_scan_bytes, 50_000);
        assert!(!cfg.feed_sync.enabled);
        assert!(!cfg.feed_sync.fail_open);
        assert!(cfg.feed_sync.sources.is_empty());
    }

    #[test]
    fn config_deserialize_full_yaml() {
        let yaml = r#"
allowed_schemes:
  - https
  - http
timeout_secs: 30
max_bytes: 50000
allow_private_ips: true
denylist:
  - evil.com
allowlist:
  - safe.org
content_validation:
  enabled: false
  strict_mode: true
  max_scan_bytes: 100000
feed_sync:
  enabled: true
  fail_open: true
  sources:
    - url: "https://feeds.example.com/block.txt"
      mode: denylist
      format: lines
      enabled: true
      max_entries: 500
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");

        assert_eq!(cfg.allowed_schemes, vec!["https", "http"]);
        assert_eq!(cfg.timeout_secs, 30);
        assert_eq!(cfg.max_bytes, 50_000);
        assert!(cfg.allow_private_ips);
        assert_eq!(cfg.denylist, vec!["evil.com"]);
        assert_eq!(cfg.allowlist, vec!["safe.org"]);
        assert!(!cfg.content_validation.enabled);
        assert!(cfg.content_validation.strict_mode);
        assert_eq!(cfg.content_validation.max_scan_bytes, 100_000);
        assert!(cfg.feed_sync.enabled);
        assert!(cfg.feed_sync.fail_open);
        assert_eq!(cfg.feed_sync.sources.len(), 1);
        let src = &cfg.feed_sync.sources[0];
        assert_eq!(src.url, "https://feeds.example.com/block.txt");
        assert_eq!(src.mode, "denylist");
        assert_eq!(src.format.as_deref(), Some("lines"));
        assert_eq!(src.enabled, Some(true));
        assert_eq!(src.max_entries, Some(500));
    }

    #[test]
    fn config_deserialize_missing_optional() {
        let yaml = "";
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");

        assert_eq!(cfg.allowed_schemes, vec!["https"]);
        assert_eq!(cfg.timeout_secs, 15);
        assert_eq!(cfg.max_bytes, 20_000);
        assert!(cfg.content_validation.enabled);
        assert!(!cfg.feed_sync.enabled);
    }

    #[test]
    fn config_normalize_empty_schemes() {
        let yaml = r#"
allowed_schemes: []
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");
        assert!(cfg.allowed_schemes.is_empty());

        let normalized = cfg.normalize();
        assert_eq!(normalized.allowed_schemes, vec!["https"]);
    }

    #[test]
    fn config_normalize_hosts() {
        let yaml = r#"
denylist:
  - "  EVIL.COM.  "
  - "*.WildCARD.Net."
  - "  "
allowlist:
  - " SAFE.org "
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");
        let normalized = cfg.normalize();

        assert_eq!(normalized.denylist, vec!["evil.com", "*.wildcard.net"]);
        assert_eq!(normalized.allowlist, vec!["safe.org"]);
    }

    #[test]
    fn config_normalize_zero_max_bytes() {
        let yaml = r#"
max_bytes: 0
timeout_secs: 0
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.max_bytes, 0);
        assert_eq!(cfg.timeout_secs, 0);

        let normalized = cfg.normalize();
        assert_eq!(normalized.max_bytes, 20_000);
        assert_eq!(normalized.timeout_secs, 15);
    }

    #[test]
    fn feed_source_normalize() {
        let yaml = r#"
feed_sync:
  sources:
    - url: ""
      mode: allowlist
    - url: "  "
      mode: denylist
    - url: "https://valid.example.com/list.txt"
      mode: denylist
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.feed_sync.sources.len(), 3);

        let normalized = cfg.normalize();
        assert_eq!(normalized.feed_sync.sources.len(), 1);
        assert_eq!(
            normalized.feed_sync.sources[0].url,
            "https://valid.example.com/list.txt"
        );
    }
}

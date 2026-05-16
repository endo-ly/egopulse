//! WebFetch tool configuration types, defaults, normalization, and tests.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_ALLOWED_SCHEMES: &[&str] = &["https"];
const DEFAULT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_MAX_FETCH_BYTES: usize = 512 * 1024;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_SCAN_BYTES: usize = 64 * 1024;

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
    /// Maximum bytes to fetch from the network. Default: 524288 (512KB)
    pub max_fetch_bytes: usize,
    /// Maximum bytes in the final output after processing. Default: 65536 (64KB)
    pub max_output_bytes: usize,
    /// Whether to allow requests to private/loopback IPs. Default: false
    pub allow_private_ips: bool,
    /// Host denylist (exact match + subdomain wildcard). Default: empty
    pub denylist: Vec<String>,
    /// Host allowlist. Default: empty (allow all)
    pub allowlist: Vec<String>,
    /// Content validation settings.
    pub content_validation: WebFetchContentValidationConfig,
}

/// Content-validation sub-settings for the web_fetch tool.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct WebFetchContentValidationConfig {
    /// Whether content validation is enabled. Default: true
    pub enabled: bool,
    /// Strict mode: blocks on single low-confidence hit. Default: false
    pub strict_mode: bool,
    /// Maximum bytes to scan for injection. Default: 65536
    pub max_scan_bytes: usize,
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
            max_fetch_bytes: DEFAULT_MAX_FETCH_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            allow_private_ips: false,
            denylist: Vec::new(),
            allowlist: Vec::new(),
            content_validation: WebFetchContentValidationConfig::default(),
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
    /// * Ensures `content_validation.max_scan_bytes >= max_output_bytes` so that
    ///   prompt-injection scanning covers the entire output body.
    pub(crate) fn normalize(mut self) -> Self {
        if self.allowed_schemes.is_empty() {
            self.allowed_schemes = DEFAULT_ALLOWED_SCHEMES
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        }

        self.denylist = normalize_hosts(self.denylist);
        self.allowlist = normalize_hosts(self.allowlist);

        if self.max_fetch_bytes == 0 {
            self.max_fetch_bytes = DEFAULT_MAX_FETCH_BYTES;
        }
        if self.max_output_bytes == 0 {
            self.max_output_bytes = DEFAULT_MAX_OUTPUT_BYTES;
        }
        if self.timeout_secs == 0 {
            self.timeout_secs = DEFAULT_TIMEOUT_SECS;
        }
        if self.content_validation.max_scan_bytes < self.max_output_bytes {
            self.content_validation.max_scan_bytes = self.max_output_bytes;
        }

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
        assert_eq!(cfg.max_fetch_bytes, 512 * 1024);
        assert_eq!(cfg.max_output_bytes, 64 * 1024);
        assert!(!cfg.allow_private_ips);
        assert!(cfg.denylist.is_empty());
        assert!(cfg.allowlist.is_empty());
        assert!(cfg.content_validation.enabled);
        assert!(!cfg.content_validation.strict_mode);
        assert_eq!(cfg.content_validation.max_scan_bytes, 64 * 1024);
    }

    #[test]
    fn config_deserialize_full_yaml() {
        let yaml = r#"
allowed_schemes:
  - https
  - http
timeout_secs: 30
max_fetch_bytes: 500000
max_output_bytes: 32000
allow_private_ips: true
denylist:
  - evil.com
allowlist:
  - safe.org
content_validation:
  enabled: false
  strict_mode: true
  max_scan_bytes: 100000
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");

        assert_eq!(cfg.allowed_schemes, vec!["https", "http"]);
        assert_eq!(cfg.timeout_secs, 30);
        assert_eq!(cfg.max_fetch_bytes, 500_000);
        assert_eq!(cfg.max_output_bytes, 32_000);
        assert!(cfg.allow_private_ips);
        assert_eq!(cfg.denylist, vec!["evil.com"]);
        assert_eq!(cfg.allowlist, vec!["safe.org"]);
        assert!(!cfg.content_validation.enabled);
        assert!(cfg.content_validation.strict_mode);
        assert_eq!(cfg.content_validation.max_scan_bytes, 100_000);
    }

    #[test]
    fn config_deserialize_missing_optional() {
        let yaml = "";
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");

        assert_eq!(cfg.allowed_schemes, vec!["https"]);
        assert_eq!(cfg.timeout_secs, 15);
        assert_eq!(cfg.max_fetch_bytes, 512 * 1024);
        assert_eq!(cfg.max_output_bytes, 64 * 1024);
        assert!(cfg.content_validation.enabled);
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
    fn config_normalize_zero_max_fetch_bytes() {
        let yaml = r#"
max_fetch_bytes: 0
max_output_bytes: 0
timeout_secs: 0
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.max_fetch_bytes, 0);
        assert_eq!(cfg.max_output_bytes, 0);
        assert_eq!(cfg.timeout_secs, 0);

        let normalized = cfg.normalize();

        assert_eq!(normalized.max_fetch_bytes, 512 * 1024);
        assert_eq!(normalized.max_output_bytes, 64 * 1024);
        assert_eq!(normalized.timeout_secs, 15);
    }

    #[test]
    fn config_normalize_preserves_nonzero_values() {
        let yaml = r#"
max_fetch_bytes: 100000
max_output_bytes: 50000
timeout_secs: 30
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");
        let normalized = cfg.normalize();

        assert_eq!(normalized.max_fetch_bytes, 100_000);
        assert_eq!(normalized.max_output_bytes, 50_000);
        assert_eq!(normalized.timeout_secs, 30);
    }

    #[test]
    fn config_normalize_raises_max_scan_bytes_to_max_output_bytes() {
        let yaml = r#"
max_output_bytes: 200000
content_validation:
  max_scan_bytes: 10000
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");
        assert_eq!(cfg.max_output_bytes, 200_000);
        assert_eq!(cfg.content_validation.max_scan_bytes, 10_000);

        let normalized = cfg.normalize();

        assert_eq!(normalized.max_output_bytes, 200_000);
        assert_eq!(
            normalized.content_validation.max_scan_bytes, 200_000,
            "max_scan_bytes must be raised to max_output_bytes"
        );
    }

    #[test]
    fn config_normalize_preserves_max_scan_bytes_when_already_sufficient() {
        let yaml = r#"
max_output_bytes: 50000
content_validation:
  max_scan_bytes: 100000
"#;
        let cfg: WebFetchConfig = yaml_serde::from_str(yaml).expect("deserialize");
        let normalized = cfg.normalize();

        assert_eq!(normalized.content_validation.max_scan_bytes, 100_000);
    }
}

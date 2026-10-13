//! 文脈ベースのシークレットリダクション。
//!
//! Config に登録されていない値や prefix を持たない値でも、周囲の文脈
//! （`KEY=VALUE` のキー名、`Authorization` ヘッダー、URL クエリ）が秘密の
//! 保持を示す場合に、値を `[REDACTED:secret]` へ置換する。偽陽性は出力が
//! 多少欠けるだけで済むが、偽陰性は漏洩になるため、検出は広めに取る。

use std::sync::LazyLock;

use regex::{Captures, Regex};

use super::sanitizer::{SECRET_KEY_PATTERN, shannon_entropy};

const REPLACEMENT: &str = "[REDACTED:secret]";

/// 代入形の値として扱う最小長とエントロピー（bit/文字）。
const ASSIGNMENT_MIN_LEN: usize = 20;
const ASSIGNMENT_MIN_ENTROPY: f64 = 3.5;
/// `Bearer <value>` 単体の値の最小長。
const BEARER_MIN_LEN: usize = 16;
/// `Authorization` ヘッダーと URL クエリは、キー名が明示的なので長さだけを見る。
const EXPLICIT_MIN_LEN: usize = 8;

/// `KEY=VALUE` / `key: value` / `"key": "value"`。キー名が秘密を示すもの。
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?P<head>[A-Za-z0-9_.\-]*{SECRET_KEY_PATTERN}[A-Za-z0-9_.\-]*(?:\\?["'])?\s*[:=]\s*(?:\\?["'])?)(?P<value>[^\s"'`,;&)}}\]<>\\]+)"#
    ))
    .expect("assignment pattern")
});

/// `Authorization: Bearer <value>` / `Basic` / `Token`。
static AUTHORIZATION_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?P<head>authorization(?:\\?["'])?\s*[:=]\s*(?:\\?["'])?\s*(?:bearer|basic|token)\s+)(?P<value>[^\s"'`,;\\]+)"#,
    )
    .expect("authorization pattern")
});

/// ヘッダー名を伴わない `Bearer <value>`。
static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?P<head>bearer\s+)(?P<value>[A-Za-z0-9._~+/=\-]+)").expect("bearer pattern")
});

/// URL クエリの認証情報。
static URL_QUERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?P<head>[?&](?:access_token|id_token|refresh_token|token|api_key|apikey|sig|signature|code|x-amz-signature|x-amz-credential|x-amz-security-token)=)(?P<value>[^&\s"'#<>\\]+)"#,
    )
    .expect("url query pattern")
});

/// 文脈から秘密と判断できる値を `[REDACTED:secret]` に置換する。
pub(crate) fn redact_contextual_secrets(output: &str) -> String {
    let redacted = replace_values(&ASSIGNMENT, output, is_assignment_secret);
    let redacted = replace_values(&AUTHORIZATION_HEADER, &redacted, |value| {
        value.chars().count() >= EXPLICIT_MIN_LEN && !is_placeholder(value)
    });
    let redacted = replace_values(&BEARER, &redacted, is_bearer_secret);
    replace_values(&URL_QUERY, &redacted, |value| {
        value.chars().count() >= EXPLICIT_MIN_LEN && !is_placeholder(value)
    })
}

fn replace_values(pattern: &Regex, input: &str, is_secret: impl Fn(&str) -> bool) -> String {
    pattern
        .replace_all(input, |caps: &Captures<'_>| {
            let value = &caps["value"];
            if is_secret(value) {
                format!("{}{REPLACEMENT}", &caps["head"])
            } else {
                caps[0].to_string()
            }
        })
        .into_owned()
}

fn is_assignment_secret(value: &str) -> bool {
    value.chars().count() >= ASSIGNMENT_MIN_LEN
        && shannon_entropy(value) >= ASSIGNMENT_MIN_ENTROPY
        && has_mixed_character_classes(value)
        && !is_placeholder(value)
        && !is_path_or_url(value)
}

fn is_bearer_secret(value: &str) -> bool {
    value.chars().count() >= BEARER_MIN_LEN
        && has_mixed_character_classes(value)
        && !is_placeholder(value)
}

/// 小文字だけの snake_case の単語や数字だけの値を、秘密から除く。
fn has_mixed_character_classes(value: &str) -> bool {
    let classes = [
        value.chars().any(|c| c.is_ascii_lowercase()),
        value.chars().any(|c| c.is_ascii_uppercase()),
        value.chars().any(|c| c.is_ascii_digit()),
    ];
    classes.into_iter().filter(|present| *present).count() >= 2
}

fn is_placeholder(value: &str) -> bool {
    value.contains("REDACTED")
        || value.starts_with('$')
        || value.starts_with('{')
        || value.starts_with('*')
}

fn is_path_or_url(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with('~')
        || value.starts_with("http://")
        || value.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX64: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    const TOKEN: &str = "Zk3pQ9vLm2XrT8wYb4NcHd6Fs1Jg";

    #[test]
    fn redacts_environment_dump_entries_with_secret_like_keys() {
        // Arrange: `ps eww` prints KEY=VALUE pairs separated by spaces.
        let input = format!(
            "PID CMD\n123 node app PATH=/usr/bin GATEWAY_API_KEY={HEX64} GATEWAY_WEBHOOK_SECRET={HEX64} HOME=/root"
        );

        // Act
        let output = redact_contextual_secrets(&input);

        // Assert
        assert_eq!(
            output,
            "PID CMD\n123 node app PATH=/usr/bin GATEWAY_API_KEY=[REDACTED:secret] GATEWAY_WEBHOOK_SECRET=[REDACTED:secret] HOME=/root"
        );
    }

    #[test]
    fn redacts_json_and_yaml_assignments() {
        let input = format!(
            "{{\"api_key\": \"{TOKEN}\", \"name\": \"x\"}}\nclient_secret: {TOKEN}\nescaped: \\\"access_token\\\": \\\"{TOKEN}\\\""
        );

        let output = redact_contextual_secrets(&input);

        assert!(!output.contains(TOKEN), "{output}");
        assert!(output.contains("\"name\": \"x\""));
        assert_eq!(output.matches(REPLACEMENT).count(), 3, "{output}");
    }

    #[test]
    fn redacts_authorization_header_values_regardless_of_entropy() {
        let input =
            "curl -H 'Authorization: Bearer abcd1234' -H \"Authorization: Basic dXNlcjpwYXNz\"";

        let output = redact_contextual_secrets(input);

        assert_eq!(
            output,
            "curl -H 'Authorization: Bearer [REDACTED:secret]' -H \"Authorization: Basic [REDACTED:secret]\""
        );
    }

    #[test]
    fn redacts_standalone_bearer_credentials() {
        let output = redact_contextual_secrets(&format!("sent Bearer {TOKEN} to the server"));

        assert_eq!(output, "sent Bearer [REDACTED:secret] to the server");
    }

    #[test]
    fn redacts_credential_query_parameters_in_urls() {
        let input = "GET https://api.example.com/v1/items?page=2&access_token=abcDEF123456&code=xyz98765 and https://x.test/cb?sig=AbCd1234Ef&other=1";

        let output = redact_contextual_secrets(input);

        assert_eq!(
            output,
            "GET https://api.example.com/v1/items?page=2&access_token=[REDACTED:secret]&code=[REDACTED:secret] and https://x.test/cb?sig=[REDACTED:secret]&other=1"
        );
    }

    #[test]
    fn keeps_non_secret_values_untouched() {
        // Arrange: each line mentions a secret-like key but carries no secret.
        let cases = [
            "max_tokens: 4096",
            "token_type: authorization_code_flow_grant",
            "\"token_path\": \"/home/user/.config/service/token\"",
            "api_key = os.environ[\"SERVICE_API_KEY_VALUE\"]",
            "password: ${DATABASE_PASSWORD_FROM_VAULT_ENTRY}",
            "token_url: https://auth.example.com/oauth2/Token9",
            "API_KEY_ENV = \"EGOPULSE_API_KEY_FOR_SERVICE_NAME\"",
            "secret_count=1234567890123456789012",
            "GET /items?page=2&code=200&limit=10",
            "the bearer of bad news",
            "Authorization: Bearer ",
        ];

        for input in cases {
            // Act
            let output = redact_contextual_secrets(input);

            // Assert
            assert_eq!(output, input);
        }
    }

    #[test]
    fn is_idempotent() {
        let input = format!("API_KEY={HEX64} Authorization: Bearer abcd1234 ?token=abcDEF123456");

        let once = redact_contextual_secrets(&input);
        let twice = redact_contextual_secrets(&once);

        assert_eq!(once, twice);
    }
}

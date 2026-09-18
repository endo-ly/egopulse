//! Agent label から agent id を生成する slugify ユーティリティ。

/// Agent label を agent id へ正規化する。
///
/// ASCII 英小文字化 → 非ASCII英数字をハイフンへ置換 → 連続ハイフンを圧縮 →
/// 前後のハイフンを削除し、結果が空文字列になった場合は `"default"` を返す。
pub(crate) fn slugify_agent_id(label: &str) -> String {
    let slug = compress_and_trim(
        label
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }),
    );
    if slug.is_empty() {
        "default".to_string()
    } else {
        slug
    }
}

fn compress_and_trim<I: Iterator<Item = char>>(chars: I) -> String {
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in chars {
        if c == '-' {
            if !prev_hyphen && !result.is_empty() {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    while result.ends_with('-') {
        result.pop();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::slugify_agent_id;

    #[test]
    fn slugify_covers_normalization_and_fallback_cases() {
        let cases = [
            ("Lyre", "lyre"),
            ("My Agent", "my-agent"),
            ("Vega 2", "vega-2"),
            ("  Multi   Space  ", "multi-space"),
            ("", "default"),
            ("!!!", "default"),
            ("   ", "default"),
            ("日本語Agent", "agent"),
        ];

        for (input, expected) in cases {
            assert_eq!(slugify_agent_id(input), expected, "input: {input:?}");
        }
    }
}

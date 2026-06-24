//! Review 画面等の表示用フォーマット関数。

/// API key を Review 画面向けに部分マスクして返す。
///
/// 空文字列の場合は `"(empty)"` を返す。
/// それ以外は先頭3文字 + `...` + 末尾4文字の形式 (`docs/setup-redesign.md §4.2 Review`)。
/// `sk-` 等のプレフィックスを保持しつつ、実値を秘匿する。
pub(crate) fn format_api_key_for_review(api_key: &str) -> String {
    if api_key.is_empty() {
        return "(empty)".to_string();
    }
    let chars: Vec<char> = api_key.chars().collect();
    let head: String = chars.iter().take(3).collect();
    let tail: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}...{tail}")
}

#[cfg(test)]
mod tests {
    use super::format_api_key_for_review;

    #[test]
    fn format_api_key_for_review_masks_long_values() {
        let result = format_api_key_for_review("sk-abcdef123456");
        assert_eq!(result, "sk-...3456");
    }

    #[test]
    fn format_api_key_for_review_shows_empty_for_blank() {
        assert_eq!(format_api_key_for_review(""), "(empty)");
    }
}

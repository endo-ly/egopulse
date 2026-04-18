//! テキスト分割ユーティリティ。
//!
//! Discord / Telegram のメッセージ長制限に合わせてテキストをチャンクに分割する。
//! UTF-8 文字境界を尊重し、改行位置での優先分割を行う。

/// UTF-8安全なインデックスクランプ。
///
/// 指定インデックスが文字境界でない場合、前の文字境界まで下げる。
pub fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    let len = s.len();
    if index >= len {
        return len;
    }

    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }

    index
}

/// テキストを指定最大長のチャンクに分割。
///
/// 改行境界で優先的に分割し、各チャンクが max_len を超えないようにする。
/// Discord (2000文字) / Telegram (4096文字) のメッセージ長制限対応。
pub fn split_text(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let chunk_len = if remaining.len() <= max_len {
            remaining.len()
        } else {
            let boundary = floor_char_boundary(remaining, max_len.min(remaining.len()));
            remaining[..boundary].rfind('\n').unwrap_or(boundary)
        };
        chunks.push(remaining[..chunk_len].to_string());
        remaining = &remaining[chunk_len..];
        if remaining.starts_with('\n') {
            remaining = &remaining[1..];
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_returns_single_chunk() {
        let result = split_text("hello", 2000);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn empty_text_returns_single_empty_chunk() {
        let result = split_text("", 2000);
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn splits_at_newline_boundary() {
        let text = "line1\nline2\nline3";
        let result = split_text(text, 7);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "line1");
        assert_eq!(result[1], "line2");
        assert_eq!(result[2], "line3");
    }

    #[test]
    fn splits_long_text_respecting_max_len() {
        let text = "a".repeat(5000);
        let result = split_text(&text, 2000);
        assert_eq!(result.len(), 3);
        for chunk in &result {
            assert!(chunk.len() <= 2000);
        }
        let reconstructed: String = result.join("");
        assert_eq!(reconstructed, text);
    }

    #[test]
    fn floor_char_boundary_within_bounds() {
        let s = "hello";
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 10), 5);
        assert_eq!(floor_char_boundary(s, 0), 0);
    }

    #[test]
    fn floor_char_boundary_multibyte() {
        // "こんにちは" — each char is 3 bytes in UTF-8
        let s = "こんにちは";
        assert_eq!(s.len(), 15);
        // Index 7 is mid-character (char boundary at 6, 9)
        assert_eq!(floor_char_boundary(s, 7), 6);
        assert_eq!(floor_char_boundary(s, 6), 6);
    }
}

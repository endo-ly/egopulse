//! Setup wizard のプロンプト入出力抽象化と Review 表示用フォーマット関数。
//!
//! [`PromptSource`] / [`OutputSink`] trait により、wizard 本体 ([`crate::setup::wizard`])
//! への入出力を抽象化する。本番環境では [`DialoguerPromptSource`] /
//! [`DialoguerOutputSink`] を使用し、テストではモック実装に差し替える。

use std::io::Write;

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
    if chars.len() <= 7 {
        return "********".to_string();
    }
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

/// セットアップウィザードのプロンプト入力を抽象化する trait。
///
/// wizard 本体はこの trait を介してユーザー入力を取得する。
/// 本番環境では [`DialoguerPromptSource`] を、テストでは `MockPromptSource` を使用する。
///
/// 全メソッドは入力エラー時に `Err(String)` を返す。
pub(crate) trait PromptSource {
    /// テキスト入力を求める。`default` は事前入力値 (空文字列可)。
    fn text(&self, label: &str, default: &str) -> Result<String, String>;

    /// パスワード入力 (hidden) を求める。
    fn password(&self, label: &str) -> Result<String, String>;

    /// 選択肢からインデックスを選ぶ。`items` は表示文字列のリスト。
    fn select(&self, label: &str, items: &[String]) -> Result<usize, String>;

    /// Yes/No 確認。`default` は Enter 押下時の値。
    fn confirm(&self, label: &str, default: bool) -> Result<bool, String>;
}

/// セットアップウィザードの出力先を抽象化する trait。
pub(crate) trait OutputSink {
    /// 改行付きでテキストを出力する。
    fn println(&self, text: &str);
}

/// [`dialoguer`] を用いた本番用 [`PromptSource`] 実装。
pub(crate) struct DialoguerPromptSource;

impl DialoguerPromptSource {
    /// 新しいインスタンスを生成する。
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Default for DialoguerPromptSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptSource for DialoguerPromptSource {
    fn text(&self, label: &str, default: &str) -> Result<String, String> {
        dialoguer::Input::<String>::new()
            .with_prompt(label)
            .with_initial_text(default)
            .allow_empty(true)
            .interact_text()
            .map_err(|e| format!("Input error: {e}"))
    }

    fn password(&self, label: &str) -> Result<String, String> {
        dialoguer::Password::new()
            .with_prompt(label)
            .allow_empty_password(true)
            .interact()
            .map_err(|e| format!("Password input error: {e}"))
    }

    fn select(&self, label: &str, items: &[String]) -> Result<usize, String> {
        dialoguer::Select::new()
            .with_prompt(label)
            .items(items)
            .default(0)
            .interact()
            .map_err(|e| format!("Select error: {e}"))
    }

    fn confirm(&self, label: &str, default: bool) -> Result<bool, String> {
        dialoguer::Confirm::new()
            .with_prompt(label)
            .default(default)
            .interact()
            .map_err(|e| format!("Confirm error: {e}"))
    }
}

/// 標準出力への [`OutputSink`] 実装。
pub(crate) struct DialoguerOutputSink;

impl DialoguerOutputSink {
    /// 新しいインスタンスを生成する。
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Default for DialoguerOutputSink {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputSink for DialoguerOutputSink {
    fn println(&self, text: &str) {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        let _ = handle.write_all(text.as_bytes());
        let _ = handle.write_all(b"\n");
        let _ = handle.flush();
    }
}

/// テスト用 [`PromptSource`] / [`OutputSink`] モック実装。
///
/// wizard 統合テスト (T35〜T41) が [`crate::setup::test_mocks`] 経由で使用する。
#[cfg(test)]
pub(crate) mod test_mocks {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::OutputSink;
    use super::PromptSource;

    /// テスト用 [`PromptSource`] モック。
    ///
    /// 入力はタイプ別 (text / password / select / confirm) のキューで管理し、
    /// 各呼び出しでラベルが部分一致する先頭エントリを消費する。
    /// `text_defaults` / `confirm_defaults` に渡されたデフォルト値を記録し、
    /// prefill テスト (T35) でアサーションに使用する。
    pub(crate) struct MockPromptSource {
        text: RefCell<VecDeque<(String, String)>>,
        password: RefCell<VecDeque<(String, String)>>,
        select: RefCell<VecDeque<(String, usize)>>,
        confirm: RefCell<VecDeque<(String, bool)>>,
        text_defaults: RefCell<Vec<(String, String)>>,
        confirm_defaults: RefCell<Vec<(String, bool)>>,
    }

    impl MockPromptSource {
        pub(crate) fn new() -> Self {
            Self {
                text: RefCell::new(VecDeque::new()),
                password: RefCell::new(VecDeque::new()),
                select: RefCell::new(VecDeque::new()),
                confirm: RefCell::new(VecDeque::new()),
                text_defaults: RefCell::new(Vec::new()),
                confirm_defaults: RefCell::new(Vec::new()),
            }
        }

        pub(crate) fn expect_text(&self, label_sub: &str, value: &str) -> &Self {
            self.text
                .borrow_mut()
                .push_back((label_sub.to_string(), value.to_string()));
            self
        }

        pub(crate) fn expect_password(&self, label_sub: &str, value: &str) -> &Self {
            self.password
                .borrow_mut()
                .push_back((label_sub.to_string(), value.to_string()));
            self
        }

        pub(crate) fn expect_select(&self, label_sub: &str, index: usize) -> &Self {
            self.select
                .borrow_mut()
                .push_back((label_sub.to_string(), index));
            self
        }

        pub(crate) fn expect_confirm(&self, label_sub: &str, value: bool) -> &Self {
            self.confirm
                .borrow_mut()
                .push_back((label_sub.to_string(), value));
            self
        }

        /// wizard から渡された text デフォルト値の (label, default) ペアを返す。
        pub(crate) fn text_defaults(&self) -> Vec<(String, String)> {
            self.text_defaults.borrow().clone()
        }

        /// wizard から渡された confirm デフォルト値の (label, default) ペアを返す。
        pub(crate) fn confirm_defaults(&self) -> Vec<(String, bool)> {
            self.confirm_defaults.borrow().clone()
        }

        fn consume_text(&self, label: &str, default: &str) -> Result<String, String> {
            self.text_defaults
                .borrow_mut()
                .push((label.to_string(), default.to_string()));
            let mut queue = self.text.borrow_mut();
            consume_entry(&mut queue, label, "text")
        }

        fn consume_password(&self, label: &str) -> Result<String, String> {
            let mut queue = self.password.borrow_mut();
            consume_entry(&mut queue, label, "password")
        }

        fn consume_select(&self, label: &str) -> Result<usize, String> {
            let mut queue = self.select.borrow_mut();
            consume_entry(&mut queue, label, "select")
        }

        fn consume_confirm(&self, label: &str, default: bool) -> Result<bool, String> {
            self.confirm_defaults
                .borrow_mut()
                .push((label.to_string(), default));
            let mut queue = self.confirm.borrow_mut();
            consume_entry(&mut queue, label, "confirm")
        }
    }

    impl Default for MockPromptSource {
        fn default() -> Self {
            Self::new()
        }
    }

    impl PromptSource for MockPromptSource {
        fn text(&self, label: &str, default: &str) -> Result<String, String> {
            self.consume_text(label, default)
        }

        fn password(&self, label: &str) -> Result<String, String> {
            self.consume_password(label)
        }

        fn select(&self, label: &str, _items: &[String]) -> Result<usize, String> {
            self.consume_select(label)
        }

        fn confirm(&self, label: &str, default: bool) -> Result<bool, String> {
            self.consume_confirm(label, default)
        }
    }

    /// テスト用 [`OutputSink`]。出力を `Vec<String>` へ蓄積する。
    pub(crate) struct VecOutputSink {
        lines: RefCell<Vec<String>>,
    }

    impl VecOutputSink {
        pub(crate) fn new() -> Self {
            Self {
                lines: RefCell::new(Vec::new()),
            }
        }

        pub(crate) fn joined(&self) -> String {
            self.lines.borrow().join("\n")
        }
    }

    impl Default for VecOutputSink {
        fn default() -> Self {
            Self::new()
        }
    }

    impl OutputSink for VecOutputSink {
        fn println(&self, text: &str) {
            self.lines.borrow_mut().push(text.to_string());
        }
    }

    fn consume_entry<T>(
        queue: &mut VecDeque<(String, T)>,
        label: &str,
        kind: &str,
    ) -> Result<T, String> {
        let pos = queue
            .iter()
            .position(|(expected, _)| label.contains(expected.as_str()))
            .ok_or_else(|| format!("no mock {kind} input matching '{label}'"))?;
        let (_, value) = queue.remove(pos).expect("position was found");
        Ok(value)
    }
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

    #[test]
    fn format_api_key_for_review_fully_masks_short_values() {
        assert_eq!(format_api_key_for_review("abc"), "********");
        assert_eq!(format_api_key_for_review("sk-1234"), "********");
    }
}

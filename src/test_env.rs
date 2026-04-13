//! テスト用環境変数ガード。
//!
//! `unsafe` をこのモジュールに集約し、テストコード全体で安全に環境変数を操作できるようにする。

/// 環境変数のスコープガード。Drop時に元の値を復元する。
pub(crate) struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    /// 環境変数を設定し、元の値を保持するガードを作成する。
    pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => unsafe {
                std::env::set_var(self.key, value);
            },
            None => unsafe {
                std::env::remove_var(self.key);
            },
        }
    }
}

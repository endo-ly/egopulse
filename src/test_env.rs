//! テスト用環境変数ガード。
//!
//! `unsafe` をこのモジュールに集約し、テストコード全体で安全に環境変数を操作できるようにする。
//! グローバルミューテックスで直列化し、マルチスレッド環境での未定義動作を防ぐ。

use std::sync::{LazyLock, Mutex, MutexGuard};

static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// 環境変数のスコープガード。Drop時に元の値を復元する。
pub(crate) struct EnvVarGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvVarGuard {
    /// 環境変数を設定し、元の値を保持するガードを作成する。
    pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self {
            saved: vec![(key, original)],
            _lock: lock,
        }
    }

    /// 追加の環境変数を同じガード内で設定する。
    ///
    /// 同一ガード内で複数の環境変数を管理できるため、ミューテックスの再取得不要。
    pub(crate) fn also_set(
        mut self,
        key: &'static str,
        value: impl AsRef<std::ffi::OsStr>,
    ) -> Self {
        if self.saved.iter().any(|(k, _)| *k == key) {
            unsafe {
                std::env::set_var(key, value);
            }
            return self;
        }
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        self.saved.push((key, original));
        self
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (key, original) in self.saved.iter().rev() {
            match original {
                Some(value) => unsafe {
                    std::env::set_var(key, value);
                },
                None => unsafe {
                    std::env::remove_var(key);
                },
            }
        }
    }
}

//! `.env` の全値を値ベースのリダクション対象として供給する。
//!
//! Config が参照していない値（他サービスの鍵、skill が使う値など）も
//! ツール出力から伏せるため、ファイルの更新時刻でキャッシュしながら
//! ツール結果ごとに最新の内容を反映する。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use tracing::warn;

use crate::config::secret_ref::read_dotenv;

use super::sanitizer::{is_plausible_secret_value, is_secret_like_key};

pub(crate) struct DotenvSecrets {
    path: PathBuf,
    cache: Mutex<Option<CachedSecrets>>,
}

struct CachedSecrets {
    modified: SystemTime,
    secrets: Vec<(String, String)>,
}

impl DotenvSecrets {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache: Mutex::new(None),
        }
    }

    /// `.env` の値のうちリダクション対象にできるものを `dotenv.<KEY>` 名で返す。
    ///
    /// ファイルが存在しない場合は空を返す。
    pub(crate) fn secrets(&self) -> Vec<(String, String)> {
        let Ok(modified) = std::fs::metadata(&self.path).and_then(|meta| meta.modified()) else {
            return Vec::new();
        };
        let mut cache = self.cache.lock().expect("dotenv secrets lock");
        if let Some(cached) = cache.as_ref()
            && cached.modified == modified
        {
            return cached.secrets.clone();
        }
        let secrets = collect_dotenv_secrets(&read_dotenv(&self.path));
        *cache = Some(CachedSecrets {
            modified,
            secrets: secrets.clone(),
        });
        secrets
    }
}

fn collect_dotenv_secrets(
    dotenv: &std::collections::HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut secrets = Vec::new();
    for (key, raw) in dotenv {
        let value = strip_matching_quotes(raw);
        if is_plausible_secret_value(value) {
            secrets.push((format!("dotenv.{key}"), value.to_string()));
        } else if is_secret_like_key(key) && !value.is_empty() {
            warn!(
                key = %key,
                "value is too short or low-entropy to redact from tool output"
            );
        }
    }
    secrets.sort_by(|left, right| left.0.cmp(&right.0));
    secrets
}

fn strip_matching_quotes(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_plausible_values_and_skips_common_ones() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");
        std::fs::write(
            &path,
            "OTHER_SERVICE_KEY=Zk3pQ9vLm2XrT8wYb4Nc\nPORT=8080\nDEBUG=true\nQUOTED=\"Ab9xQ2mLp7ZkT4vWc8Nd\"\n",
        )
        .expect("write");
        let source = DotenvSecrets::new(path);

        // Act
        let secrets = source.secrets();

        // Assert
        assert_eq!(
            secrets,
            vec![
                (
                    "dotenv.OTHER_SERVICE_KEY".to_string(),
                    "Zk3pQ9vLm2XrT8wYb4Nc".to_string()
                ),
                (
                    "dotenv.QUOTED".to_string(),
                    "Ab9xQ2mLp7ZkT4vWc8Nd".to_string()
                ),
            ]
        );
    }

    #[test]
    fn missing_file_yields_no_secrets() {
        let dir = tempfile::tempdir().expect("tempdir");

        let source = DotenvSecrets::new(dir.path().join(".env"));

        assert!(source.secrets().is_empty());
    }

    #[test]
    fn reloads_when_file_changes() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");
        std::fs::write(&path, "A_KEY=Zk3pQ9vLm2XrT8wYb4Nc\n").expect("write");
        let source = DotenvSecrets::new(path.clone());
        assert_eq!(source.secrets().len(), 1);

        // Act
        std::fs::write(
            &path,
            "A_KEY=Zk3pQ9vLm2XrT8wYb4Nc\nB_KEY=Qw8eRt5yUi2oPa7sDf3g\n",
        )
        .expect("rewrite");
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(later)
            .expect("set mtime");

        // Assert
        assert_eq!(source.secrets().len(), 2);
    }
}

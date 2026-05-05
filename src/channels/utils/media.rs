//! メディアファイルユーティリティ。
//!
//! 受信ファイルの保存および添付テキスト整形を提供する。
//! Discord / Telegram 等のチャネルハンドラから共通して利用される。

use std::path::{Path, PathBuf};

use chrono::Utc;
use thiserror::Error;
use tracing;

/// メディア処理に関するエラー型。
#[derive(Error, Debug)]
pub(crate) enum MediaError {
    /// I/O 操作に失敗した。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// ファイル名が不正（空文字列・パス区切り・パストラバーサル）。
    #[error("invalid filename: {0}")]
    InvalidFilename(String),

    /// パストラバーサル攻撃を検出した。
    #[error("path traversal detected: {0}")]
    PathTraversal(String),
}

/// 受信ファイルを `workspace/media/inbound/` に保存する。
///
/// `workspace_dir` 直下に `media/inbound/` ディレクトリを作成し、
/// タイムスタンプ付きファイル名でバイト列を書き出す。
///
/// # Errors
///
/// - `filename` が空文字列の場合 → [`MediaError::InvalidFilename`]
/// - `filename` に `..` が含まれる場合 → [`MediaError::PathTraversal`]
/// - `filename` に `/` または `\` が含まれる場合 → [`MediaError::InvalidFilename`]
/// - ディレクトリ作成やファイル書き込みに失敗した場合 → [`MediaError::Io`]
pub(crate) fn save_inbound_file(
    workspace_dir: &Path,
    filename: &str,
    data: &[u8],
) -> Result<PathBuf, MediaError> {
    let sanitized = sanitize_filename(filename)?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let generated_name = format!("{timestamp}-{sanitized}");

    let inbound_dir = workspace_dir.join("media").join("inbound");
    std::fs::create_dir_all(&inbound_dir)?;

    let dest = inbound_dir.join(&generated_name);
    std::fs::write(&dest, data)?;

    tracing::debug!(
        path = %dest.display(),
        size = data.len(),
        "saved inbound file"
    );

    Ok(dest)
}

/// 添付ファイルパスとユーザーテキストから通知テキストを組み立てる。
///
/// 各パスについて `[attachment: {full_path}]` 行を先頭に並べ、
/// `user_text` が空でなければ末尾に追加する。
///
/// ```
/// use std::path::PathBuf;
/// use egopulse::channels::utils::media::format_attachment_text;
///
/// let paths = vec![PathBuf::from("/tmp/photo.png")];
/// let text = format_attachment_text(&paths, "see this");
/// assert_eq!(text, "[attachment: /tmp/photo.png]\nsee this");
/// ```
pub(crate) fn format_attachment_text(paths: &[PathBuf], user_text: &str) -> String {
    if paths.is_empty() && user_text.is_empty() {
        return String::new();
    }

    let mut parts: Vec<String> = paths
        .iter()
        .map(|p| format!("[attachment: {}]", p.display()))
        .collect();

    if !user_text.is_empty() {
        parts.push(user_text.to_string());
    }

    parts.join("\n")
}

/// ファイル名を検証・サニタイズする。
///
/// - 空文字列を拒否
/// - `..` を拒否（パストラバーサル）
/// - `/` および `\` を拒否
/// - 先頭のドットを除去（隠しファイル化防止）
fn sanitize_filename(filename: &str) -> Result<String, MediaError> {
    if filename.is_empty() {
        return Err(MediaError::InvalidFilename(
            "filename must not be empty".to_string(),
        ));
    }

    if filename.contains("..") {
        return Err(MediaError::PathTraversal(
            "filename must not contain '..'".to_string(),
        ));
    }

    if filename.contains('/') || filename.contains('\\') {
        return Err(MediaError::InvalidFilename(
            "filename must not contain path separators".to_string(),
        ));
    }

    let stripped = filename.trim_start_matches('.');

    if stripped.is_empty() {
        return Err(MediaError::InvalidFilename(
            "filename must not consist only of dots".to_string(),
        ));
    }

    Ok(stripped.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn save_inbound_file_creates_file_with_timestamp_name() {
        // Arrange
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = dir.path();
        let content = b"hello world";

        // Act
        let result = save_inbound_file(workspace, "photo.png", content);

        // Assert
        let path = result.expect("save should succeed");
        assert!(path.exists());
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with("-photo.png")
        );
        let saved = fs::read(&path).expect("should read saved file");
        assert_eq!(saved, content);
    }

    #[test]
    fn save_inbound_file_creates_directory_if_missing() {
        // Arrange
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = dir.path();
        let inbound_dir = workspace.join("media").join("inbound");
        assert!(!inbound_dir.exists());

        // Act
        let result = save_inbound_file(workspace, "doc.pdf", b"data");

        // Assert
        assert!(result.is_ok());
        assert!(inbound_dir.exists());
        assert!(inbound_dir.is_dir());
    }

    #[test]
    fn save_inbound_file_rejects_path_traversal() {
        // Arrange
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = dir.path();

        // Act
        let result = save_inbound_file(workspace, "../../../etc/passwd", b"data");

        // Assert
        let err = result.expect_err("should reject path traversal");
        assert!(matches!(err, MediaError::PathTraversal(_)));
    }

    #[test]
    fn save_inbound_file_rejects_empty_filename() {
        // Arrange
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = dir.path();

        // Act
        let result = save_inbound_file(workspace, "", b"data");

        // Assert
        let err = result.expect_err("should reject empty filename");
        assert!(matches!(err, MediaError::InvalidFilename(_)));
    }

    #[test]
    fn format_attachment_text_with_user_text() {
        // Arrange
        let paths = vec![PathBuf::from(
            "/workspace/media/inbound/20260428-123456-photo.png",
        )];
        let user_text = "check this out";

        // Act
        let result = format_attachment_text(&paths, user_text);

        // Assert
        assert_eq!(
            result,
            "[attachment: /workspace/media/inbound/20260428-123456-photo.png]\ncheck this out"
        );
    }

    #[test]
    fn format_attachment_text_without_user_text() {
        // Arrange
        let paths = vec![PathBuf::from(
            "/workspace/media/inbound/20260428-123456-photo.png",
        )];
        let user_text = "";

        // Act
        let result = format_attachment_text(&paths, user_text);

        // Assert
        assert_eq!(
            result,
            "[attachment: /workspace/media/inbound/20260428-123456-photo.png]"
        );
    }

    #[test]
    fn format_attachment_text_multiple_files() {
        // Arrange
        let paths = vec![
            PathBuf::from("/workspace/media/inbound/a.png"),
            PathBuf::from("/workspace/media/inbound/b.pdf"),
        ];
        let user_text = "see attached";

        // Act
        let result = format_attachment_text(&paths, user_text);

        // Assert
        assert_eq!(
            result,
            "[attachment: /workspace/media/inbound/a.png]\n\
             [attachment: /workspace/media/inbound/b.pdf]\n\
             see attached"
        );
    }
}

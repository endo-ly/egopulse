//! Setup wizard 固有のエラー型。
//!
//! `String` ベースのアドホックエラーを thiserror で構造化し、
//! 呼び出し元 ([`crate::error::EgoPulseError`]) への `#[from]` 変換を可能にする。

use std::io;

use thiserror::Error;

/// セットアップウィザードの実行中に発生しうる構造化エラー。
#[derive(Debug, Error)]
pub(crate) enum SetupWizardError {
    #[error("setup aborted by user")]
    Aborted,
    #[error("prompt error: {0}")]
    Prompt(String),
    #[error("failed to save config: {0}")]
    Save(String),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("config path resolution failed: {0}")]
    ConfigResolve(String),
}

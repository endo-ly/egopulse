//! 対話型セットアップウィザードのエントリポイント。
//!
//! 実際のフロー実装は `wizard` モジュールを参照。本モジュールは
//! `main.rs` から呼ばれる後方互換エントリポイントのみを公開する。

mod channels;
mod error;
pub(crate) mod inputs;
pub(crate) mod prompts;
mod provider;
pub(crate) mod slugify;
mod summary;
pub(crate) mod wizard;

pub(crate) use error::SetupWizardError;

use std::path::PathBuf;

use crate::error::EgoPulseError;

/// Runs the interactive setup wizard and writes the resulting configuration file.
///
/// Thin wrapper around `wizard::run` for backwards-compatible entrypoint.
pub async fn run_setup_wizard(config_path: Option<PathBuf>) -> Result<(), EgoPulseError> {
    wizard::run(config_path).map_err(EgoPulseError::from)
}

//! 対話型セットアップウィザードのエントリポイント。
//!
//! 実際のフロー実装は [`wizard`] モジュールを参照。本モジュールは
//! `main.rs` から呼ばれる後方互換エントリポイントのみを公開する。

mod channels;
pub(crate) mod inputs;
pub(crate) mod prompts;
mod provider;
pub(crate) mod slugify;
mod summary;
pub(crate) mod wizard;

use std::path::PathBuf;

/// Runs the interactive setup wizard and writes the resulting configuration file.
///
/// Thin wrapper around [`wizard::run`] for backwards-compatible entrypoint.
pub async fn run_setup_wizard(config_path: Option<PathBuf>) -> Result<(), String> {
    wizard::run(config_path)
}

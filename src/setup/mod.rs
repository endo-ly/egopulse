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
/// A user-initiated abort is treated as a successful cancellation and returns
/// `Ok(())`; genuine setup failures are surfaced via [`EgoPulseError`].
///
/// # Errors
///
/// Returns `Err(EgoPulseError::SetupWizard(_))` when the wizard fails due to
/// prompt I/O, validation, config save, or config path resolution errors.
pub async fn run_setup_wizard(config_path: Option<PathBuf>) -> Result<(), EgoPulseError> {
    match wizard::run(config_path) {
        Ok(()) => Ok(()),
        Err(SetupWizardError::Aborted) => Ok(()),
        Err(error) => Err(EgoPulseError::from(error)),
    }
}

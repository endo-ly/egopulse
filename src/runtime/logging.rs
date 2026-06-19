//! ロギング初期化。
//!
//! tracing-subscriber を用いたグローバルロガーのセットアップと、
//! panic 時の二重 abort を防ぐカスタム panic hook の設定を提供する。

use std::io::Write;
use std::sync::OnceLock;

use tracing_subscriber::EnvFilter;

use crate::error::LoggingError;

/// Targets that emit excessive debug output and are clamped to `warn`.
/// Pulled in by readability-js via html5ever — thousands of log lines per
/// large HTML document can fill the stderr pipe buffer.
const NOISY_CRATE_OVERRIDES: &[&str] = &["html5ever=warn", "markup5ever=warn", "selectors=warn"];

/// Ensures `install_eagain_safe_panic_hook` runs exactly once even if
/// `init_logging` is called multiple times.
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

/// Initialize the global tracing subscriber with the given log level.
///
/// Also installs a panic hook that tolerates `EAGAIN` on stderr to prevent
/// the double-panic → `abort()` chain described in `install_eagain_safe_panic_hook`.
pub fn init_logging(level: &str) -> Result<(), LoggingError> {
    PANIC_HOOK_INSTALLED.get_or_init(install_eagain_safe_panic_hook);

    let noisy_directives = NOISY_CRATE_OVERRIDES.join(",");
    let filter_string = format!("{level},{noisy_directives}");
    let filter = EnvFilter::try_new(&filter_string)
        .or_else(|_| EnvFilter::try_new(filter_string.to_ascii_lowercase()))
        .map_err(|error| LoggingError::InitFailed(error.to_string()))?;

    let format = std::env::var("EGOPULSE_LOG_FORMAT").unwrap_or_default();

    let result = if format.eq_ignore_ascii_case("json") {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .json()
            .try_init()
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .try_init()
    };

    match result {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .to_string()
                .contains("global default trace dispatcher")
                || error.to_string().contains("already initialized") =>
        {
            Ok(())
        }
        Err(error) => Err(LoggingError::InitFailed(error.to_string())),
    }
}

/// Installs a panic hook that ignores stderr write failures.
///
/// The default hook writes the panic message to stderr and panics again if
/// that write fails.  When the stderr pipe is full (e.g. html5ever flood),
/// this triggers a double-panic → `abort()`, bypassing `catch_unwind`.
/// This hook discards write failures instead, allowing normal unwinding.
pub(crate) fn install_eagain_safe_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{info}");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            default_hook(info);
        }));
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn init_logging_default_is_text() {
        // SAFETY: test-only env var manipulation, single-threaded via #[serial]
        unsafe { std::env::remove_var("EGOPULSE_LOG_FORMAT") };
        let result = init_logging("info");
        assert!(
            result.is_ok(),
            "default text format should init: {result:?}"
        );
    }

    #[test]
    #[serial]
    fn init_logging_json_format() {
        // SAFETY: test-only env var manipulation, single-threaded via #[serial]
        unsafe { std::env::set_var("EGOPULSE_LOG_FORMAT", "json") };
        let result = init_logging("info");
        assert!(result.is_ok(), "json format should init: {result:?}");
        // SAFETY: test-only env var cleanup
        unsafe { std::env::remove_var("EGOPULSE_LOG_FORMAT") };
    }

    #[test]
    #[serial]
    fn init_logging_invalid_format_falls_back() {
        // SAFETY: test-only env var manipulation, single-threaded via #[serial]
        unsafe { std::env::set_var("EGOPULSE_LOG_FORMAT", "xml") };
        let result = init_logging("info");
        assert!(
            result.is_ok(),
            "invalid format should fall back to text: {result:?}"
        );
        // SAFETY: test-only env var cleanup
        unsafe { std::env::remove_var("EGOPULSE_LOG_FORMAT") };
    }
}

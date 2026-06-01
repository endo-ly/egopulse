//! Public facade for the egopulse binary entrypoint.
//!
//! Re-exports the API surface used by the CLI binary. Internal modules
//! remain `pub(crate)`; this facade is the single point of contact for
//! the binary entrypoint.
//!
//! Each submodule mirrors a domain area and re-exports only the items
//! consumed by `main.rs`. Module-wide re-export is intentionally avoided
//! because Rust disallows re-exporting `pub(crate)` modules publicly
//! (E0365).

/// Agent loop entrypoints used by the CLI binary.
pub mod agent_loop {
    pub use crate::agent_loop::ask_in_session;
}

/// Channel adapters exposed to the CLI binary.
pub mod channels {
    /// Local CLI chat session driver.
    pub mod cli {
        pub use crate::channels::cli::run_chat;
    }
}

/// Configuration types and helpers.
pub mod config {
    pub use crate::config::{default_config_path, Config};
}

/// Top-level error types and config error variants.
pub mod error {
    pub use crate::error::{ConfigError, EgoPulseError};
}

/// Runtime building blocks (AppState assembly, channel startup).
pub mod runtime {
    pub use crate::runtime::{
        ask, build_app_state_with_path, build_sleep_app_state_with_path, run_tui,
        start_channels,
    };

    /// Gateway command actions and CLI config path resolution.
    pub mod gateway {
        pub use crate::runtime::gateway::{
            resolve_cli_config_path, run_gateway, GatewayAction,
        };
    }

    /// Logging initialization.
    pub mod logging {
        pub use crate::runtime::logging::init_logging;
    }
}

/// Interactive setup wizard entrypoint.
pub mod setup {
    pub use crate::setup::run_setup_wizard;
}

/// Sleep batch entrypoints and error type.
pub mod sleep {
    pub use crate::sleep::{run_events_extract, run_sleep_batch, SleepBatchError};
}

/// Storage types consumed directly by the CLI binary.
pub mod storage {
    pub use crate::storage::SleepRunTrigger;
}

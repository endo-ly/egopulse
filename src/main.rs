use std::path::PathBuf;

use clap::{Parser, Subcommand};
use egopulse::channels::cli;
use egopulse::config::Config;
use egopulse::error::EgoPulseError;
use egopulse::logging::init_logging;
use egopulse::runtime;
use egopulse::web;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(name = "egopulse", version = VERSION, about = "EgoPulse persistent agent core")]
struct Cli {
    /// Explicit config file path (absolute or relative)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send one prompt through the persistent agent loop.
    Ask {
        #[arg(long, value_name = "SESSION")]
        session: Option<String>,
        prompt: String,
    },
    /// Start or resume a persistent CLI chat session.
    Chat {
        #[arg(long, value_name = "SESSION")]
        session: Option<String>,
    },
    /// Start the HTTP server with WebUI.
    Web {
        /// Host address to bind to
        #[arg(long)]
        host: Option<String>,
        /// Port to listen on
        #[arg(long)]
        port: Option<u16>,
    },
    /// Start all enabled channel adapters based on config.
    /// Microclaw-compatible: starts web, discord, telegram concurrently.
    Start,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), EgoPulseError> {
    let cli = Cli::parse();
    let is_web = matches!(cli.command, Some(Command::Web { .. }));
    let is_start = matches!(cli.command, Some(Command::Start));
    let resolved_config_path = match cli.config.as_deref() {
        Some(path) => Some(path.to_path_buf()),
        None => Config::resolve_config_path()?,
    };
    let config = if is_web || is_start {
        Config::load_allow_missing_api_key(resolved_config_path.as_deref())?
    } else {
        Config::load(resolved_config_path.as_deref())?
    };
    init_logging(&config.log_level)?;

    match cli.command {
        Some(Command::Ask { session, prompt }) => match if let Some(session) = session.as_deref() {
            runtime::ask_in_session(config, session, &prompt).await
        } else {
            runtime::ask(config, &prompt).await
        } {
            Ok(response) => {
                println!("assistant: {response}");
                Ok(())
            }
            Err(EgoPulseError::ShutdownRequested) => Ok(()),
            Err(error) => Err(error),
        },
        Some(Command::Chat { session }) => {
            let state = runtime::build_app_state_with_path(config, resolved_config_path.clone())?;
            let session = session.unwrap_or_else(|| format!("cli-{}", uuid::Uuid::new_v4()));
            match cli::run_chat(&state, &session).await {
                Ok(()) | Err(EgoPulseError::ShutdownRequested) => Ok(()),
                Err(error) => Err(error),
            }
        }
        Some(Command::Web { host, port }) => {
            if !config.channel_enabled("web") {
                return Err(EgoPulseError::Config(
                    egopulse::error::ConfigError::WebChannelDisabled,
                ));
            }
            let bind_host = host.unwrap_or_else(|| config.web_host());
            let bind_port = port.unwrap_or_else(|| config.web_port());
            let state = runtime::build_app_state_with_path(config, resolved_config_path.clone())?;
            web::run_server(state, &bind_host, bind_port).await
        }
        Some(Command::Start) => {
            let state = runtime::build_app_state_with_path(config, resolved_config_path.clone())?;
            runtime::start_channels(state).await
        }
        None => runtime::run_tui(config).await,
    }
}

//! EgoPulse CLI エントリーポイント。
//!
//! `run` で有効チャネルを一括起動し、`ask` と `chat` でローカル対話を実行する。
//! `setup` は初期設定ウィザード、`gateway` は systemd 管理、`update` は自己更新を担当する。

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use egopulse::agent_loop;
use egopulse::channels::cli;
use egopulse::config::{Config, default_config_path};
use egopulse::error::{ConfigError, EgoPulseError};
use egopulse::gateway::{self, GatewayAction};
use egopulse::logging::init_logging;
use egopulse::runtime;
use egopulse::setup;

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
    /// Run all enabled channel adapters in the foreground.
    Run,
    /// Interactive setup wizard to create egopulse.config.yaml.
    Setup,
    Gateway {
        #[command(subcommand)]
        action: Option<GatewayAction>,
    },
    Update,
}

/// Parses the CLI, runs the requested command, and exits with status 1 on failure.
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), EgoPulseError> {
    let cli = Cli::parse();

    // setup は設定ファイル未作成でも実行できるよう、通常の設定解決フローに入る前に分岐する。
    if matches!(cli.command, Some(Command::Setup)) {
        return setup::run_setup_wizard(cli.config.clone())
            .await
            .map_err(EgoPulseError::Internal);
    }

    match cli.command {
        Some(Command::Run) => run_foreground(cli.config.as_ref()).await,
        Some(Command::Gateway { action }) => {
            gateway::run_gateway(cli.config.as_ref(), action).await
        }
        Some(Command::Update) => gateway::run_update().await,
        _ => run_with_config(&cli).await,
    }
}

async fn run_foreground(cli_config: Option<&PathBuf>) -> Result<(), EgoPulseError> {
    let resolved_config_path = match cli_config {
        Some(path) => Some(gateway::resolve_cli_config_path(path)),
        None => Config::resolve_config_path().map_err(EgoPulseError::Config)?,
    };
    let config = Config::load_allow_missing_api_key(resolved_config_path.as_deref())?;
    init_logging(&config.log_level)?;
    let state = runtime::build_app_state_with_path(config, resolved_config_path).await?;
    runtime::start_channels(state).await
}

async fn run_with_config(cli: &Cli) -> Result<(), EgoPulseError> {
    let resolved_config_path = match cli.config.as_deref() {
        Some(path) => Some(gateway::resolve_cli_config_path(path)),
        None => match Config::resolve_config_path() {
            Ok(path) => path,
            Err(ConfigError::AutoConfigNotFound { .. }) => {
                // 引数なし起動だけは初回体験を優先し、エラーではなく setup への案内を返す。
                if cli.command.is_none() {
                    eprintln!("No configuration found. Run 'egopulse setup' to create one.");
                    return Ok(());
                }
                return Err(EgoPulseError::Config(ConfigError::AutoConfigNotFound {
                    searched_paths: vec![default_config_path()?],
                }));
            }
            Err(e) => return Err(EgoPulseError::Config(e)),
        },
    };
    let config = Config::load(resolved_config_path.as_deref())?;
    init_logging(&config.log_level)?;

    match &cli.command {
        Some(Command::Ask { session, prompt }) => match if let Some(session) = session.as_deref() {
            agent_loop::ask_in_session(config, session, prompt).await
        } else {
            runtime::ask(config, prompt).await
        } {
            Ok(response) => {
                println!("assistant: {response}");
                Ok(())
            }
            Err(EgoPulseError::ShutdownRequested) => Ok(()),
            Err(error) => Err(error),
        },
        Some(Command::Chat { session }) => {
            let state =
                runtime::build_app_state_with_path(config, resolved_config_path.clone()).await?;
            let session = session
                .as_ref()
                .cloned()
                .unwrap_or_else(|| format!("cli-{}", uuid::Uuid::new_v4()));
            match cli::run_chat(&state, &session).await {
                Ok(()) | Err(EgoPulseError::ShutdownRequested) => Ok(()),
                Err(error) => Err(error),
            }
        }
        Some(Command::Run) => unreachable!("handled without standard config flow"),
        Some(Command::Setup) => unreachable!("handled before config loading"),
        Some(Command::Gateway { .. }) | Some(Command::Update) => {
            unreachable!("handled without config")
        }
        None => runtime::run_tui(config, resolved_config_path).await,
    }
}

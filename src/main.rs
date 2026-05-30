//! EgoPulse CLI エントリーポイント。
//!
//! `run` で有効チャネルを一括起動し、`ask` と `chat` でローカル対話を実行する。
//! `setup` は初期設定ウィザード、`gateway` は systemd 管理、`update` は自己更新、
//! `sleep` は手動 sleep batch 実行を担当する。

use std::path::PathBuf;

use chrono::Datelike;
use chrono::TimeZone;
use clap::{Parser, Subcommand};
use egopulse::agent_loop;
use egopulse::channels::cli;
use egopulse::config::{Config, default_config_path};
use egopulse::error::{ConfigError, EgoPulseError};
use egopulse::runtime;
use egopulse::runtime::gateway::{self, GatewayAction};
use egopulse::runtime::logging::init_logging;
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
    /// Run a manual sleep batch for long-term memory processing.
    Sleep {
        /// Agent to run the sleep batch for (defaults to config's default_agent).
        #[arg(long)]
        agent: Option<String>,
    },
    /// Event extraction operations.
    Events {
        #[command(subcommand)]
        action: EventsAction,
    },
}

#[derive(Debug, Subcommand)]
enum EventsAction {
    /// Extract episode events from past sessions.
    Extract {
        /// Agent ID (defaults to config's default_agent).
        #[arg(long)]
        agent: Option<String>,
        /// Start date (RFC3339 or YYYY-MM-DD).
        #[arg(long)]
        from: Option<String>,
        /// End date (RFC3339 or YYYY-MM-DD).
        #[arg(long)]
        to: Option<String>,
    },
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
    init_logging(config.log_level())?;
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
    init_logging(config.log_level())?;

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
        Some(Command::Sleep { agent }) => {
            let state = runtime::build_sleep_app_state_with_path(config, resolved_config_path)?;
            match egopulse::sleep::run_sleep_batch(
                &state,
                agent.as_deref(),
                egopulse::storage::SleepRunTrigger::Manual,
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(egopulse::sleep::SleepBatchError::AlreadyRunning { agent_id }) => {
                    eprintln!("sleep batch already running for agent '{agent_id}'");
                    std::process::exit(1);
                }
                Err(error) => Err(EgoPulseError::Internal(error.to_string())),
            }
        }
        Some(Command::Events { action }) => match action {
            EventsAction::Extract { agent, from, to } => {
                let tz: chrono_tz::Tz = config.timezone.parse().unwrap_or(chrono_tz::Tz::UTC);
                let state = runtime::build_sleep_app_state_with_path(config, resolved_config_path)?;
                let from = from
                    .as_deref()
                    .map(|d| normalize_date_input_from(d, tz))
                    .transpose()?;
                let to = to
                    .as_deref()
                    .map(|d| normalize_date_input_to(d, tz))
                    .transpose()?;
                match egopulse::sleep::run_events_extract(
                    &state,
                    agent.as_deref(),
                    from.as_deref(),
                    to.as_deref(),
                )
                .await
                {
                    Ok(()) => Ok(()),
                    Err(egopulse::sleep::SleepBatchError::AlreadyRunning { agent_id }) => {
                        eprintln!("sleep batch already running for agent '{agent_id}'");
                        std::process::exit(1);
                    }
                    Err(error) => Err(EgoPulseError::Internal(error.to_string())),
                }
            }
        },
        Some(Command::Run) => unreachable!("handled without standard config flow"),
        Some(Command::Setup) => unreachable!("handled before config loading"),
        Some(Command::Gateway { .. }) | Some(Command::Update) => {
            unreachable!("handled without config")
        }
        None => runtime::run_tui(config, resolved_config_path).await,
    }
}

/// Normalizes a `--from` date input to UTC RFC3339.
///
/// Date-only (`YYYY-MM-DD`) is interpreted in the given timezone as 00:00:00
/// local time, then converted to UTC. RFC3339 inputs are also normalized to UTC.
fn normalize_date_input_from(input: &str, tz: chrono_tz::Tz) -> Result<String, EgoPulseError> {
    if is_date_only(input) {
        let date = chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d")
            .map_err(|e| EgoPulseError::Internal(format!("invalid --from date '{input}': {e}")))?;
        let local = tz
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
            .single()
            .ok_or_else(|| {
                EgoPulseError::Internal(format!(
                    "ambiguous or non-existent local time for --from date '{input}' in timezone {tz}"
                ))
            })?;
        Ok(local.naive_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string())
    } else {
        let dt = chrono::DateTime::parse_from_rfc3339(input).map_err(|e| {
            EgoPulseError::Internal(format!("invalid --from datetime '{input}': {e}"))
        })?;
        Ok(dt.naive_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string())
    }
}

fn normalize_date_input_to(input: &str, tz: chrono_tz::Tz) -> Result<String, EgoPulseError> {
    if is_date_only(input) {
        let date = chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d")
            .map_err(|e| EgoPulseError::Internal(format!("invalid --to date '{input}': {e}")))?;
        let next = date + chrono::Duration::days(1);
        let local = tz
            .with_ymd_and_hms(next.year(), next.month(), next.day(), 0, 0, 0)
            .single()
            .ok_or_else(|| {
                EgoPulseError::Internal(format!(
                    "ambiguous or non-existent local time for --to date '{input}' in timezone {tz}"
                ))
            })?;
        Ok(local.naive_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string())
    } else {
        let dt = chrono::DateTime::parse_from_rfc3339(input).map_err(|e| {
            EgoPulseError::Internal(format!("invalid --to datetime '{input}': {e}"))
        })?;
        Ok(dt.naive_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string())
    }
}

/// Returns `true` if the input looks like a date-only string (`YYYY-MM-DD`)
/// without time or timezone components.
fn is_date_only(input: &str) -> bool {
    input.len() == 10
        && input.chars().nth(4) == Some('-')
        && input.chars().nth(7) == Some('-')
        && !input.contains('T')
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn sleep_command_parses_with_agent_flag() {
        let cli: Cli =
            Parser::try_parse_from(["egopulse", "sleep", "--agent", "lyre"]).expect("parse");
        match cli.command {
            Some(Command::Sleep { agent }) => {
                assert_eq!(agent.as_deref(), Some("lyre"));
            }
            other => panic!("expected Sleep, got {other:?}"),
        }
    }

    #[test]
    fn sleep_command_parses_without_agent_flag() {
        let cli: Cli = Parser::try_parse_from(["egopulse", "sleep"]).expect("parse");
        match cli.command {
            Some(Command::Sleep { agent }) => {
                assert!(agent.is_none());
            }
            other => panic!("expected Sleep, got {other:?}"),
        }
    }

    #[test]
    fn sleep_command_rejects_invalid_flags() {
        let result = Cli::try_parse_from(["egopulse", "sleep", "--invalid"]);
        assert!(result.is_err(), "should reject --invalid flag");
    }

    #[test]
    fn status_command_removed_from_clap() {
        let result = Cli::try_parse_from(["egopulse", "status"]);
        assert!(result.is_err(), "`egopulse status` should no longer parse");
    }

    #[test]
    fn normalize_from_date_only() {
        assert_eq!(
            normalize_date_input_from("2025-01-15", chrono_tz::Tz::UTC).unwrap(),
            "2025-01-15T00:00:00Z"
        );
    }

    #[test]
    fn normalize_from_rfc3339_passthrough() {
        assert_eq!(
            normalize_date_input_from("2025-01-15T10:00:00Z", chrono_tz::Tz::UTC).unwrap(),
            "2025-01-15T10:00:00Z"
        );
    }

    #[test]
    fn normalize_to_date_only() {
        assert_eq!(
            normalize_date_input_to("2025-01-15", chrono_tz::Tz::UTC).unwrap(),
            "2025-01-16T00:00:00Z"
        );
    }

    #[test]
    fn normalize_to_month_boundary() {
        assert_eq!(
            normalize_date_input_to("2025-01-31", chrono_tz::Tz::UTC).unwrap(),
            "2025-02-01T00:00:00Z"
        );
    }

    #[test]
    fn normalize_to_year_boundary() {
        assert_eq!(
            normalize_date_input_to("2025-12-31", chrono_tz::Tz::UTC).unwrap(),
            "2026-01-01T00:00:00Z"
        );
    }

    #[test]
    fn normalize_to_rfc3339_passthrough() {
        assert_eq!(
            normalize_date_input_to("2025-06-01T23:59:59Z", chrono_tz::Tz::UTC).unwrap(),
            "2025-06-01T23:59:59Z"
        );
    }
}

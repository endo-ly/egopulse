//! EgoPulse CLI エントリーポイント。
//!
//! 引数なしで TUI を起動し、`-p` で非対話のプロンプトを実行する。
//! `setup` は初期設定ウィザード、`gateway` は systemd 管理、`update` は自己更新、
//! `sleep` は手動 sleep batch 実行を担当する。

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

use chrono::Datelike;
use chrono::TimeZone;
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use egopulse::agent_loop;
use egopulse::config::{Config, default_config_path};
use egopulse::error::{ConfigError, EgoPulseError};
use egopulse::runtime;
use egopulse::runtime::gateway::{self, GatewayAction};
use egopulse::runtime::logging::init_logging;
use egopulse::setup;
use thiserror::Error;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn parse_cli() -> Cli {
    let cli = Cli::parse();
    if let Err(error) = validate_cli(&cli) {
        error.exit();
    }
    cli
}

#[cfg(test)]
fn parse_cli_from(args: &[&str]) -> Result<Cli, clap::Error> {
    let cli = Cli::try_parse_from(args)?;
    validate_cli(&cli)?;
    Ok(cli)
}

fn validate_cli(cli: &Cli) -> Result<(), clap::Error> {
    if cli.print && cli.command.is_some() {
        return Err(Cli::command().error(
            ErrorKind::ArgumentConflict,
            "-p/--print cannot be used with a subcommand",
        ));
    }
    if cli.session.is_some() && cli.command.is_some() {
        return Err(Cli::command().error(
            ErrorKind::ArgumentConflict,
            "--session can only be used with the interactive TUI or --print",
        ));
    }
    Ok(())
}

#[derive(Debug, Parser)]
#[command(
    name = "egopulse",
    version = VERSION,
    about = "EgoPulse persistent agent core",
    subcommand_precedence_over_arg = true
)]
struct Cli {
    /// Explicit config file path (absolute or relative)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Run a non-interactive prompt and print only the response.
    #[arg(short = 'p', long = "print")]
    print: bool,
    /// Prompt text. When omitted, read it from piped stdin.
    #[arg(requires = "print", value_name = "PROMPT")]
    prompt: Option<String>,
    /// Persistent session to continue.
    #[arg(long, value_name = "SESSION", conflicts_with = "continue_")]
    session: Option<String>,
    /// Continue the most recently updated session.
    #[arg(long = "continue", requires = "print", conflicts_with = "session")]
    continue_: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
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

#[derive(Debug, Error, PartialEq, Eq)]
enum HeadlessError {
    #[error("no prompt: pass PROMPT or pipe stdin")]
    NoPrompt,
    #[error("no sessions available to continue")]
    NoSessions,
}

#[derive(Debug)]
enum CliError {
    Headless(HeadlessError),
    Runtime(EgoPulseError),
}

impl From<HeadlessError> for CliError {
    fn from(error: HeadlessError) -> Self {
        Self::Headless(error)
    }
}

impl From<EgoPulseError> for CliError {
    fn from(error: EgoPulseError) -> Self {
        Self::Runtime(error)
    }
}

/// Parses the CLI and runs the requested command.
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        match error {
            CliError::Headless(error) => {
                let exit_code = match &error {
                    HeadlessError::NoPrompt => 2,
                    HeadlessError::NoSessions => 1,
                };
                eprintln!("error: {error}");
                std::process::exit(exit_code);
            }
            CliError::Runtime(error) => {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
    }
}

async fn run() -> Result<(), CliError> {
    let cli = parse_cli();

    // setup は設定ファイル未作成でも実行できるよう、通常の設定解決フローに入る前に分岐する。
    if matches!(cli.command, Some(Command::Setup)) {
        return setup::run_setup_wizard(cli.config.clone())
            .await
            .map_err(Into::into);
    }

    match cli.command {
        Some(Command::Run) => run_foreground(cli.config.as_deref())
            .await
            .map_err(Into::into),
        Some(Command::Gateway { action }) => gateway::run_gateway(cli.config.as_deref(), action)
            .await
            .map_err(Into::into),
        Some(Command::Update) => gateway::run_update().await.map_err(Into::into),
        _ => run_with_config(&cli).await,
    }
}

async fn run_foreground(cli_config: Option<&std::path::Path>) -> Result<(), EgoPulseError> {
    let resolved_config_path = match cli_config {
        Some(path) => Some(gateway::resolve_cli_config_path(path)),
        None => Config::resolve_config_path().map_err(EgoPulseError::Config)?,
    };
    let config = Config::load_allow_missing_api_key(resolved_config_path.as_deref())?;
    init_logging(config.log_level())?;
    let state = runtime::build_app_state_with_path(config, resolved_config_path).await?;
    runtime::start_channels(state).await
}

async fn run_with_config(cli: &Cli) -> Result<(), CliError> {
    let prompt = if cli.print {
        let stdin_text = read_piped_stdin().map_err(CliError::Runtime)?;
        Some(resolve_prompt(cli.prompt.clone(), stdin_text)?)
    } else {
        None
    };

    let resolved_config_path = match cli.config.as_deref() {
        Some(path) => Some(gateway::resolve_cli_config_path(path)),
        None => match Config::resolve_config_path() {
            Ok(path) => path,
            Err(ConfigError::AutoConfigNotFound { .. }) => {
                // 引数なし起動だけは初回体験を優先し、エラーではなく setup への案内を返す。
                if is_tui_launch(cli) {
                    eprintln!("No configuration found. Run 'egopulse setup' to create one.");
                    return Ok(());
                }
                let searched_path = default_config_path()
                    .map_err(|error| CliError::Runtime(EgoPulseError::Config(error)))?;
                return Err(EgoPulseError::Config(ConfigError::AutoConfigNotFound {
                    searched_paths: vec![searched_path],
                })
                .into());
            }
            Err(e) => return Err(EgoPulseError::Config(e).into()),
        },
    };

    if prompt.is_none() && cli.command.is_none() {
        let socket_path = runtime::resolve_runtime_socket_path(resolved_config_path.as_deref())
            .map_err(|error| CliError::Runtime(EgoPulseError::Config(error)))?;
        return egopulse::channels::run_tui(socket_path, cli.session.as_deref())
            .await
            .map_err(Into::into);
    }

    let config = Config::load(resolved_config_path.as_deref())
        .map_err(|error| CliError::Runtime(EgoPulseError::Config(error)))?;
    init_logging(config.log_level())
        .map_err(|error| CliError::Runtime(EgoPulseError::Logging(error)))?;

    if let Some(prompt) = prompt {
        return run_headless(config, cli.session.as_deref(), cli.continue_, &prompt).await;
    }

    match &cli.command {
        Some(Command::Sleep { agent }) => {
            let state = runtime::build_sleep_app_state_with_path(config, resolved_config_path)
                .map_err(CliError::Runtime)?;
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
                Err(error) => Err(EgoPulseError::Internal(error.to_string()).into()),
            }
        }
        Some(Command::Events { action }) => match action {
            EventsAction::Extract { agent, from, to } => {
                let tz: chrono_tz::Tz = config.timezone.parse().unwrap_or(chrono_tz::Tz::UTC);
                let state = runtime::build_sleep_app_state_with_path(config, resolved_config_path)
                    .map_err(CliError::Runtime)?;
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
                    Err(error) => Err(EgoPulseError::Internal(error.to_string()).into()),
                }
            }
        },
        Some(Command::Run) => unreachable!("handled without standard config flow"),
        Some(Command::Setup) => unreachable!("handled before config loading"),
        Some(Command::Gateway { .. }) | Some(Command::Update) => {
            unreachable!("handled without config")
        }
        None => unreachable!("interactive TUI is handled before full config loading"),
    }
}

fn is_tui_launch(cli: &Cli) -> bool {
    !cli.print && cli.command.is_none()
}

fn read_piped_stdin() -> Result<Option<String>, EgoPulseError> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }

    let mut text = String::new();
    stdin
        .lock()
        .read_to_string(&mut text)
        .map_err(|error| EgoPulseError::Internal(format!("failed to read stdin: {error}")))?;
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text))
    }
}

fn resolve_prompt(
    positional: Option<String>,
    stdin_text: Option<String>,
) -> Result<String, HeadlessError> {
    match (positional, stdin_text) {
        (Some(positional), Some(stdin_text)) => Ok(format!("{positional}\n\n{stdin_text}")),
        (Some(positional), None) => Ok(positional),
        (None, Some(stdin_text)) => Ok(stdin_text),
        (None, None) => Err(HeadlessError::NoPrompt),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum AskTarget {
    Session(String),
    OneShot,
}

fn select_ask_target(
    session: Option<&str>,
    continue_: bool,
    session_names: &[String],
) -> Result<AskTarget, HeadlessError> {
    if let Some(session) = session {
        return Ok(AskTarget::Session(session.to_owned()));
    }
    if continue_ {
        return session_names
            .first()
            .cloned()
            .map(AskTarget::Session)
            .ok_or(HeadlessError::NoSessions);
    }
    Ok(AskTarget::OneShot)
}

async fn run_headless(
    config: Config,
    session: Option<&str>,
    continue_: bool,
    prompt: &str,
) -> Result<(), CliError> {
    let (target, existing_state) = if continue_ {
        let state = runtime::build_app_state(config.clone())
            .await
            .map_err(CliError::Runtime)?;
        let session_names = runtime::list_session_names(&state)
            .await
            .map_err(CliError::Runtime)?;
        (
            select_ask_target(session, true, &session_names)?,
            Some(state),
        )
    } else {
        (select_ask_target(session, false, &[])?, None)
    };

    let response = match target {
        AskTarget::Session(session) => {
            let state = match existing_state {
                Some(state) => state,
                None => runtime::build_app_state(config)
                    .await
                    .map_err(CliError::Runtime)?,
            };
            agent_loop::ask_in_session_with_state(&state, &session, prompt)
                .await
                .map_err(CliError::Runtime)
        }
        AskTarget::OneShot => runtime::ask(config, prompt)
            .await
            .map_err(CliError::Runtime),
    };

    match response {
        Ok(response) => write_headless_response(&response).map_err(CliError::Runtime),
        Err(CliError::Runtime(EgoPulseError::ShutdownRequested)) => Ok(()),
        Err(error) => Err(error),
    }
}

fn write_headless_response(response: &str) -> Result<(), EgoPulseError> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{response}")
        .map_err(|error| EgoPulseError::Internal(format!("failed to write stdout: {error}")))
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
    fn parse_print_with_positional_prompt() {
        let cli: Cli = Cli::try_parse_from(["egopulse", "-p", "hello"]).expect("parse");

        assert!(cli.print);
        assert_eq!(cli.prompt.as_deref(), Some("hello"));
    }

    #[test]
    fn parse_print_without_prompt() {
        let cli: Cli = Cli::try_parse_from(["egopulse", "-p"]).expect("parse");

        assert!(cli.print);
        assert!(cli.prompt.is_none());
    }

    #[test]
    fn parse_session_and_continue_conflict() {
        let result = Cli::try_parse_from(["egopulse", "-p", "--session", "a", "--continue"]);

        assert!(result.is_err());
    }

    #[test]
    fn parse_session_for_tui() {
        let cli = Cli::try_parse_from(["egopulse", "--session", "local"]).expect("parse");

        assert!(!cli.print);
        assert_eq!(cli.session.as_deref(), Some("local"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn parse_print_conflicts_with_subcommands() {
        let result = parse_cli_from(&["egopulse", "-p", "run"]);

        assert!(result.is_err());
    }

    #[test]
    fn session_conflicts_with_subcommands() {
        let result = parse_cli_from(&["egopulse", "--session", "local", "run"]);

        assert!(result.is_err());
    }

    #[test]
    fn prompt_without_print_rejected() {
        let result = Cli::try_parse_from(["egopulse", "hello"]);

        assert!(result.is_err());
    }

    #[test]
    fn only_no_argument_launch_is_tui() {
        let tui = Cli::try_parse_from(["egopulse"]).expect("parse");
        let headless = Cli::try_parse_from(["egopulse", "-p", "hello"]).expect("parse");

        assert!(is_tui_launch(&tui));
        assert!(!is_tui_launch(&headless));
    }

    #[test]
    fn resolve_joins_positional_and_stdin() {
        let result = resolve_prompt(Some("a".into()), Some("b".into())).expect("resolve");

        assert_eq!(result, "a\n\nb");
    }

    #[test]
    fn resolve_uses_stdin_only() {
        let result = resolve_prompt(None, Some("b".into())).expect("resolve");

        assert_eq!(result, "b");
    }

    #[test]
    fn resolve_errors_when_no_input() {
        let result = resolve_prompt(None, None);

        assert_eq!(result, Err(HeadlessError::NoPrompt));
    }

    #[test]
    fn route_prefers_explicit_session() {
        let result = select_ask_target(Some("a"), true, &["s1".into()]).expect("route");

        assert_eq!(result, AskTarget::Session("a".into()));
    }

    #[test]
    fn route_continue_resolves_latest() {
        let result = select_ask_target(None, true, &["s1".into(), "s2".into()]).expect("route");

        assert_eq!(result, AskTarget::Session("s1".into()));
    }

    #[test]
    fn route_continue_errors_when_no_sessions() {
        let result = select_ask_target(None, true, &[]);

        assert_eq!(result, Err(HeadlessError::NoSessions));
    }

    #[test]
    fn route_defaults_to_one_shot() {
        let result = select_ask_target(None, false, &[]).expect("route");

        assert_eq!(result, AskTarget::OneShot);
    }

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

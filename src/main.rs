use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use clap::{Parser, Subcommand};
use egopulse::channels::cli;
use egopulse::config::{Config, default_config_path};
use egopulse::error::{ConfigError, EgoPulseError};
use egopulse::logging::init_logging;
use egopulse::runtime;
use egopulse::setup;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const UNIT_PATH: &str = "/etc/systemd/system/egopulse.service";

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

#[derive(Debug, Subcommand)]
enum GatewayAction {
    /// Install and enable the systemd service
    Install,
    /// Start the installed systemd service
    Start,
    /// Stop the installed systemd service
    Stop,
    /// Disable and remove the systemd service
    Uninstall,
    /// Show systemd service status
    Status,
    /// Restart the systemd service
    Restart,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn resolve_config_for_service(cli_config: Option<&PathBuf>) -> Option<PathBuf> {
    if let Some(path) = cli_config {
        return Some(resolve_cli_config_path(path));
    }
    Config::resolve_config_path().ok().flatten()
}

fn resolve_cli_config_path(path: &std::path::Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn runtime_default_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".egopulse").join("egopulse.config.yaml"))
}

fn escape_systemd_exec_arg(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_systemd_path(value: &str) -> String {
    value.replace('\\', "\\\\").replace(' ', "\\s")
}

fn render_systemd_unit(exe_path: &str, config_path: &std::path::Path) -> String {
    let uses_runtime_default_config = runtime_default_config_path().as_deref() == Some(config_path);
    let default_config_arg = "%h/.egopulse/egopulse.config.yaml";
    let config_arg = if uses_runtime_default_config {
        default_config_arg.to_string()
    } else {
        config_path.to_string_lossy().to_string()
    };
    let config_dir = config_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/etc/egopulse".into());
    let state_root = "%h/.egopulse";
    let read_write_paths = if uses_runtime_default_config {
        format!("{state_root} {state_root}/data {state_root}/workspace")
    } else {
        format!(
            "{} {state_root} {state_root}/data {state_root}/workspace",
            escape_systemd_path(&config_dir)
        )
    };
    let config_arg = escape_systemd_exec_arg(&config_arg);

    format!(
        "[Unit]
Description=EgoPulse Agent Runtime
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={exe_path} --config \"{config_arg}\" run
Restart=always
RestartSec=10
Environment=HOME=%h

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths={read_write_paths}
ProtectHome=read-only

[Install]
WantedBy=multi-user.target
"
    )
}

fn systemctl_cmd(args: &[&str]) -> Result<std::process::Output, EgoPulseError> {
    ProcessCommand::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| EgoPulseError::Internal(format!("failed to run systemctl: {e}")))
}

fn ensure_success(output: std::process::Output, action: &str) -> Result<(), EgoPulseError> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Err(EgoPulseError::Internal(format!(
        "{action} failed: {stderr} {stdout}"
    )))
}

fn restart_service() -> Result<(), EgoPulseError> {
    if !std::path::Path::new(UNIT_PATH).exists() {
        println!("Service not installed, skipping restart");
        return Ok(());
    }

    let output = systemctl_cmd(&["restart", "egopulse"])?;
    if output.status.success() {
        println!("egopulse service restarted");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(EgoPulseError::Internal(format!(
            "failed to restart egopulse service: {stderr}"
        )))
    }
}

async fn run_foreground(cli_config: Option<&PathBuf>) -> Result<(), EgoPulseError> {
    let resolved_config_path = match cli_config {
        Some(path) => Some(resolve_cli_config_path(path)),
        None => Config::resolve_config_path().map_err(EgoPulseError::Config)?,
    };
    let config = Config::load_allow_missing_api_key(resolved_config_path.as_deref())?;
    init_logging(&config.log_level)?;
    let state = runtime::build_app_state_with_path(config, resolved_config_path)?;
    runtime::start_channels(state).await
}

async fn run() -> Result<(), EgoPulseError> {
    let cli = Cli::parse();

    if matches!(cli.command, Some(Command::Setup)) {
        return setup::run_setup_wizard(cli.config.clone())
            .await
            .map_err(EgoPulseError::Internal);
    }

    match cli.command {
        Some(Command::Run) => run_foreground(cli.config.as_ref()).await,
        Some(Command::Gateway { action }) => run_gateway(cli.config.as_ref(), action).await,
        Some(Command::Update) => run_update().await,
        _ => run_with_config(&cli).await,
    }
}

async fn run_gateway(
    cli_config: Option<&PathBuf>,
    action: Option<GatewayAction>,
) -> Result<(), EgoPulseError> {
    let Some(action) = action else {
        println!(
            r#"Gateway service management

USAGE:
    egopulse gateway <ACTION>

ACTIONS:
    install      Install and enable the systemd service
    start        Start the installed systemd service
    stop         Stop the installed systemd service
    uninstall    Disable and remove the systemd service
    status       Show systemd service status
    restart      Restart the systemd service
"#
        );
        return Ok(());
    };

    match action {
        GatewayAction::Install => {
            let exe_path = std::env::current_exe().map_err(|e| {
                EgoPulseError::Internal(format!("failed to resolve binary path: {e}"))
            })?;
            let config_path = resolve_config_for_service(cli_config);

            if config_path.is_none() {
                return Err(EgoPulseError::Internal(
                    "No configuration found. Run 'egopulse setup' first, then retry.".into(),
                ));
            }

            let config_path = config_path.unwrap();
            if !config_path.exists() {
                return Err(EgoPulseError::Internal(format!(
                    "Config not found at: {}. Run 'egopulse setup' first, then retry.",
                    config_path.display()
                )));
            }

            let already_installed = std::path::Path::new(UNIT_PATH).exists();
            let unit_content = render_systemd_unit(&exe_path.to_string_lossy(), &config_path);
            std::fs::write(UNIT_PATH, &unit_content).map_err(|e| {
                let msg = if e.kind() == std::io::ErrorKind::PermissionDenied {
                    format!(
                        "failed to write unit file: permission denied. \
                         Run as root or with sudo, or grant write access to {UNIT_PATH}"
                    )
                } else {
                    format!("failed to write unit file: {e}")
                };
                EgoPulseError::Internal(msg)
            })?;

            ensure_success(systemctl_cmd(&["daemon-reload"])?, "daemon-reload")?;

            if already_installed {
                ensure_success(systemctl_cmd(&["restart", "egopulse"])?, "restart service")?;
                println!("Updated and restarted egopulse service: {UNIT_PATH}");
            } else {
                ensure_success(
                    systemctl_cmd(&["enable", "--now", "egopulse"])?,
                    "enable service",
                )?;
                println!("Installed and started egopulse service: {UNIT_PATH}");
            }
            Ok(())
        }
        GatewayAction::Start => {
            ensure_success(systemctl_cmd(&["start", "egopulse"])?, "start service")?;
            println!("egopulse service started");
            Ok(())
        }
        GatewayAction::Stop => {
            ensure_success(systemctl_cmd(&["stop", "egopulse"])?, "stop service")?;
            println!("egopulse service stopped");
            Ok(())
        }
        GatewayAction::Uninstall => {
            let _ = systemctl_cmd(&["disable", "--now", "egopulse"]);
            let _ = systemctl_cmd(&["daemon-reload"]);

            if std::path::Path::new(UNIT_PATH).exists() {
                std::fs::remove_file(UNIT_PATH).map_err(|e| {
                    EgoPulseError::Internal(format!("failed to remove unit file: {e}"))
                })?;
            }
            ensure_success(systemctl_cmd(&["daemon-reload"])?, "daemon-reload")?;

            println!("Uninstalled egopulse service");
            Ok(())
        }
        GatewayAction::Status => {
            let output = systemctl_cmd(&["status", "egopulse", "--no-pager"])?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            print!("{stdout}{stderr}");

            if output.status.success() {
                Ok(())
            } else {
                Err(EgoPulseError::Internal(
                    "egopulse service is not running".into(),
                ))
            }
        }
        GatewayAction::Restart => {
            let output = systemctl_cmd(&["restart", "egopulse"])?;
            if output.status.success() {
                println!("egopulse service restarted");
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                Err(EgoPulseError::Internal(format!(
                    "failed to restart egopulse service: {stderr}"
                )))
            }
        }
    }
}

async fn run_update() -> Result<(), EgoPulseError> {
    println!("Current version: {VERSION}");
    println!("Updating EgoPulse from latest release...");

    let script_url =
        "https://raw.githubusercontent.com/endo-ava/ego-graph/main/scripts/install-egopulse.sh";
    let cmd = format!(
        "(curl -fsSL '{url}' || wget -qO- '{url}') | bash -s -- --skip-run",
        url = script_url
    );
    let status = ProcessCommand::new("sh")
        .args(["-c", &cmd])
        .status()
        .map_err(|e| EgoPulseError::Internal(format!("failed to run install script: {e}")))?;

    if !status.success() {
        return Err(EgoPulseError::Internal(format!(
            "update failed (exit code {status:?}). Run install script manually:\n  curl -fsSL {script_url} | bash"
        )));
    }

    println!("Update completed. Restarting service...");
    restart_service()?;
    Ok(())
}

async fn run_with_config(cli: &Cli) -> Result<(), EgoPulseError> {
    let resolved_config_path = match cli.config.as_deref() {
        Some(path) => Some(resolve_cli_config_path(path)),
        None => match Config::resolve_config_path() {
            Ok(path) => path,
            Err(ConfigError::AutoConfigNotFound { .. }) => {
                if cli.command.is_none() {
                    eprintln!("No configuration found. Run 'egopulse setup' to create one.");
                    return Ok(());
                }
                return Err(EgoPulseError::Config(ConfigError::AutoConfigNotFound {
                    searched_paths: vec![default_config_path()],
                }));
            }
            Err(e) => return Err(EgoPulseError::Config(e)),
        },
    };
    let config = Config::load(resolved_config_path.as_deref())?;
    init_logging(&config.log_level)?;

    match &cli.command {
        Some(Command::Ask { session, prompt }) => match if let Some(session) = session.as_deref() {
            runtime::ask_in_session(config, session, prompt).await
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
            let state = runtime::build_app_state_with_path(config, resolved_config_path.clone())?;
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
        None => runtime::run_tui(config).await,
    }
}

#[cfg(test)]
mod tests {
    use super::{render_systemd_unit, runtime_default_config_path};
    use std::path::PathBuf;

    #[test]
    fn render_systemd_unit_uses_runtime_home_placeholder_for_default_config() {
        let default_config_path =
            runtime_default_config_path().expect("home directory should resolve in tests");

        let unit = render_systemd_unit("/usr/local/bin/egopulse", &default_config_path);

        assert!(unit.contains(
            "ExecStart=/usr/local/bin/egopulse --config \"%h/.egopulse/egopulse.config.yaml\" run"
        ));
        assert!(
            unit.contains("ReadWritePaths=%h/.egopulse %h/.egopulse/data %h/.egopulse/workspace")
        );
    }

    #[test]
    fn render_systemd_unit_quotes_exec_path_and_escapes_read_write_paths() {
        let config_path = PathBuf::from("/tmp/ego pulse/config dir/egopulse.config.yaml");

        let unit = render_systemd_unit("/usr/local/bin/egopulse", &config_path);

        assert!(unit.contains(
            "ExecStart=/usr/local/bin/egopulse --config \"/tmp/ego pulse/config dir/egopulse.config.yaml\" run"
        ));
        assert!(unit.contains(
            "ReadWritePaths=/tmp/ego\\spulse/config\\sdir %h/.egopulse %h/.egopulse/data %h/.egopulse/workspace"
        ));
    }
}

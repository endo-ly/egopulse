use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use clap::{Parser, Subcommand};
use egopulse::channels::cli;
use egopulse::config::Config;
use egopulse::error::EgoPulseError;
use egopulse::logging::init_logging;
use egopulse::runtime;

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
    /// Start all enabled channel adapters based on config.
    /// Microclaw-compatible: starts web, discord, telegram concurrently.
    Start,
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
        return Some(if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir().ok()?.join(path)
        });
    }
    Config::resolve_config_path().ok().flatten()
}

fn render_systemd_unit(exe_path: &str, config_path: Option<&PathBuf>) -> String {
    let config_arg = config_path
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/etc/egopulse/egopulse.config.yaml".to_string());

    format!(
        "[Unit]
Description=EgoPulse Agent Runtime
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={exe_path} --config {config_arg} start
Restart=always
RestartSec=10
Environment=HOME=/root

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/var/lib/egopulse /etc/egopulse
ProtectHome=true

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

async fn run() -> Result<(), EgoPulseError> {
    let cli = Cli::parse();
    let is_start = matches!(cli.command, Some(Command::Start));
    let resolved_config_path = match cli.config.as_deref() {
        Some(path) => Some(path.to_path_buf()),
        None => Config::resolve_config_path()?,
    };
    let config = if is_start {
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
        Some(Command::Start) => {
            let state = runtime::build_app_state_with_path(config, resolved_config_path.clone())?;
            runtime::start_channels(state).await
        }
        Some(Command::Gateway { action }) => {
            let Some(action) = action else {
                println!(
                    r#"Gateway service management

USAGE:
    egopulse gateway <ACTION>

ACTIONS:
    install      Install and enable the systemd service
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
                    let config_path = resolve_config_for_service(cli.config.as_ref());

                    let already_installed = std::path::Path::new(UNIT_PATH).exists();
                    let unit_content =
                        render_systemd_unit(&exe_path.to_string_lossy(), config_path.as_ref());
                    std::fs::write(UNIT_PATH, &unit_content).map_err(|e| {
                        EgoPulseError::Internal(format!("failed to write unit file: {e}"))
                    })?;

                    ensure_success(systemctl_cmd(&["daemon-reload"])?, "daemon-reload")?;

                    if already_installed {
                        ensure_success(
                            systemctl_cmd(&["restart", "egopulse"])?,
                            "restart service",
                        )?;
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
                    let status = ProcessCommand::new("systemctl")
                        .args(["restart", "egopulse"])
                        .status()
                        .map_err(|e| {
                            EgoPulseError::Internal(format!("failed to run systemctl: {e}"))
                        })?;
                    if !status.success() {
                        return Err(EgoPulseError::Internal(format!(
                            "systemctl restart egopulse exited with code {status:?}"
                        )));
                    }
                    println!("egopulse service restarted");
                    Ok(())
                }
            }
        }
        Some(Command::Update) => {
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
                .map_err(|e| {
                    EgoPulseError::Internal(format!("failed to run install script: {e}"))
                })?;

            if !status.success() {
                return Err(EgoPulseError::Internal(format!(
                    "update failed (exit code {status:?}). Run install script manually:\n  curl -fsSL {script_url} | bash"
                )));
            }

            println!("Update completed. Restarting service...");
            let _ = ProcessCommand::new("systemctl")
                .args(["restart", "egopulse"])
                .status();
            Ok(())
        }
        None => runtime::run_tui(config).await,
    }
}

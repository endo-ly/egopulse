use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use clap::{Parser, Subcommand};
use egopulse::channels::cli;
use egopulse::config::Config;
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
    /// Start all enabled channel adapters based on config.
    /// Microclaw-compatible: starts web, discord, telegram concurrently.
    Start,
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

fn render_systemd_unit(
    exe_path: &str,
    config_path: &std::path::Path,
    data_dir: &std::path::Path,
) -> String {
    let config_arg = config_path.to_string_lossy();
    let config_dir = config_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/etc/egopulse".into());
    let data_dir_str = data_dir.to_string_lossy();

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
ReadWritePaths={config_dir} {data_dir_str}
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

async fn run() -> Result<(), EgoPulseError> {
    let cli = Cli::parse();

    if matches!(cli.command, Some(Command::Setup)) {
        return setup::run_setup_wizard()
            .await
            .map_err(EgoPulseError::Internal);
    }

    match cli.command {
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
                eprintln!("No configuration found.");
                eprintln!("Run 'egopulse setup' first, then retry.");
                return Ok(());
            }

            let config_path = config_path.unwrap();
            if !config_path.exists() {
                eprintln!("Config not found at: {}", config_path.display());
                eprintln!("Run 'egopulse setup' first, then retry.");
                return Ok(());
            }

            let data_dir = config_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join(".egopulse");

            let already_installed = std::path::Path::new(UNIT_PATH).exists();
            let unit_content =
                render_systemd_unit(&exe_path.to_string_lossy(), &config_path, &data_dir);
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
    let is_start = matches!(cli.command, Some(Command::Start));
    let resolved_config_path = match cli.config.as_deref() {
        Some(path) => Some(path.to_path_buf()),
        None => match Config::resolve_config_path() {
            Ok(path) => path,
            Err(ConfigError::AutoConfigNotFound { .. }) => {
                if cli.command.is_none() {
                    eprintln!("No configuration found. Run 'egopulse setup' to create one.");
                    return Ok(());
                }
                return Err(EgoPulseError::Config(ConfigError::AutoConfigNotFound {
                    searched_paths: vec!["./egopulse.config.yaml".into()],
                }));
            }
            Err(e) => return Err(EgoPulseError::Config(e)),
        },
    };
    let config = if is_start {
        Config::load_allow_missing_api_key(resolved_config_path.as_deref())?
    } else {
        Config::load(resolved_config_path.as_deref())?
    };
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
        Some(Command::Start) => {
            let state = runtime::build_app_state_with_path(config, resolved_config_path.clone())?;
            runtime::start_channels(state).await
        }
        Some(Command::Setup) => unreachable!("handled before config loading"),
        Some(Command::Gateway { .. }) | Some(Command::Update) => {
            unreachable!("handled without config")
        }
        None => runtime::run_tui(config).await,
    }
}

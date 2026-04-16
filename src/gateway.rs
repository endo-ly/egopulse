//! systemd ゲートウェイ管理と自己更新処理。
//!
//! `egopulse gateway` サブコマンド向けに unit file の生成・systemctl 実行・
//! 最新リリースへの更新処理をまとめる。

use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use crate::config::Config;
use crate::error::EgoPulseError;
use clap::Subcommand;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const SERVICE_NAME: &str = "egopulse.service";

/// systemd ユーザーサービスのユニットファイルパスを返す。
fn unit_path() -> Result<PathBuf, EgoPulseError> {
    let home = dirs::home_dir()
        .ok_or_else(|| EgoPulseError::Internal("HOME directory could not be resolved".into()))?;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join(SERVICE_NAME))
}

/// Supported systemd service management actions for `egopulse gateway`.
#[derive(Debug, Subcommand)]
pub enum GatewayAction {
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

fn resolve_config_for_service(cli_config: Option<&PathBuf>) -> Result<PathBuf, EgoPulseError> {
    if let Some(path) = cli_config {
        return Ok(resolve_cli_config_path(path));
    }
    Config::resolve_config_path()
        .map_err(EgoPulseError::Config)?
        .ok_or_else(|| {
            EgoPulseError::Internal(
                "No configuration found. Run 'egopulse setup' first, then retry.".into(),
            )
        })
}

/// Resolves a CLI config path to an absolute filesystem path.
pub fn resolve_cli_config_path(path: &std::path::Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn render_systemd_unit(exe_path: &str, config_path: &std::path::Path) -> String {
    let config_arg = config_path.to_string_lossy();
    let escaped_config = config_arg.replace('\\', "\\\\").replace('"', "\\\"");
    let working_dir = config_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".egopulse").to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string())
        });

    format!(
        "[Unit]
Description=EgoPulse Agent Runtime
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory={working_dir}
ExecStart={exe_path} --config \"{escaped_config}\" run
Restart=always
RestartSec=10
KillMode=process

[Install]
WantedBy=default.target
"
    )
}

fn systemctl_cmd(args: &[&str]) -> Result<std::process::Output, EgoPulseError> {
    ProcessCommand::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| EgoPulseError::Internal(format!("failed to run systemctl --user: {e}")))
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
    let unit = unit_path()?;
    if !unit.exists() {
        println!("Service not installed, skipping restart");
        return Ok(());
    }

    let output = systemctl_cmd(&["restart", SERVICE_NAME])?;
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

/// Executes the requested gateway action for the EgoPulse systemd service.
pub async fn run_gateway(
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
            let config_path = resolve_config_for_service(cli_config)?;
            if !config_path.exists() {
                return Err(EgoPulseError::Internal(format!(
                    "Config not found at: {}. Run 'egopulse setup' first, then retry.",
                    config_path.display()
                )));
            }

            let unit = unit_path()?;
            let unit_dir = unit
                .parent()
                .ok_or_else(|| EgoPulseError::Internal("invalid unit file path".into()))?;
            std::fs::create_dir_all(unit_dir).map_err(|e| {
                EgoPulseError::Internal(format!("failed to create unit directory: {e}"))
            })?;

            let already_installed = unit.exists();
            let unit_content = render_systemd_unit(&exe_path.to_string_lossy(), &config_path);
            std::fs::write(&unit, &unit_content)
                .map_err(|e| EgoPulseError::Internal(format!("failed to write unit file: {e}")))?;

            ensure_success(systemctl_cmd(&["daemon-reload"])?, "daemon-reload")?;

            if already_installed {
                ensure_success(
                    systemctl_cmd(&["restart", SERVICE_NAME])?,
                    "restart service",
                )?;
                println!("Updated and restarted egopulse service: {}", unit.display());
            } else {
                ensure_success(
                    systemctl_cmd(&["enable", "--now", SERVICE_NAME])?,
                    "enable service",
                )?;
                println!("Installed and started egopulse service: {}", unit.display());
            }
            Ok(())
        }
        GatewayAction::Start => {
            ensure_success(systemctl_cmd(&["start", SERVICE_NAME])?, "start service")?;
            println!("egopulse service started");
            Ok(())
        }
        GatewayAction::Stop => {
            ensure_success(systemctl_cmd(&["stop", SERVICE_NAME])?, "stop service")?;
            println!("egopulse service stopped");
            Ok(())
        }
        GatewayAction::Uninstall => {
            let _ = systemctl_cmd(&["disable", "--now", SERVICE_NAME]);
            let _ = systemctl_cmd(&["daemon-reload"]);

            let unit = unit_path()?;
            if unit.exists() {
                std::fs::remove_file(&unit).map_err(|e| {
                    EgoPulseError::Internal(format!("failed to remove unit file: {e}"))
                })?;
            }
            ensure_success(systemctl_cmd(&["daemon-reload"])?, "daemon-reload")?;

            println!("Uninstalled egopulse service");
            Ok(())
        }
        GatewayAction::Status => {
            let output = systemctl_cmd(&["status", SERVICE_NAME, "--no-pager"])?;
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
            let output = systemctl_cmd(&["restart", SERVICE_NAME])?;
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

/// Updates the installed EgoPulse binary from the latest GitHub release.
pub async fn run_update() -> Result<(), EgoPulseError> {
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

#[cfg(test)]
mod tests {
    use super::render_systemd_unit;
    use std::path::PathBuf;

    #[test]
    fn render_systemd_unit_contains_expected_directives() {
        let config_path = PathBuf::from("/home/user/.egopulse/egopulse.config.yaml");

        let unit = render_systemd_unit("/usr/local/bin/egopulse", &config_path);

        assert!(unit.contains(
            "ExecStart=/usr/local/bin/egopulse --config \"/home/user/.egopulse/egopulse.config.yaml\" run"
        ));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("RestartSec=10"));
        assert!(unit.contains("KillMode=process"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(!unit.contains("NoNewPrivileges"));
        assert!(!unit.contains("ProtectSystem"));
        assert!(!unit.contains("ReadWritePaths"));
        assert!(!unit.contains("ProtectHome"));
        assert!(!unit.contains("Environment=HOME"));
    }

    #[test]
    fn render_systemd_unit_escapes_config_path_with_special_chars() {
        let config_path = PathBuf::from("/tmp/ego pulse/config dir/egopulse.config.yaml");

        let unit = render_systemd_unit("/usr/local/bin/egopulse", &config_path);

        assert!(unit.contains("/tmp/ego pulse/config dir/egopulse.config.yaml"));
        assert!(unit.contains("WantedBy=default.target"));
    }
}

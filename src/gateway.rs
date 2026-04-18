//! systemd ゲートウェイ管理と自己更新処理。
//!
//! `egopulse gateway` サブコマンド向けに unit file の生成・systemctl 実行・
//! 最新リリースへの更新処理をまとめる。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use crate::config::Config;
use crate::error::EgoPulseError;
use clap::Subcommand;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const SERVICE_NAME: &str = "egopulse.service";

fn unit_path() -> Result<PathBuf, EgoPulseError> {
    let home = dirs::home_dir()
        .ok_or_else(|| EgoPulseError::Internal("HOME directory could not be resolved".into()))?;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join(SERVICE_NAME))
}

fn build_service_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();

    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            env.insert("HOME".to_string(), home.clone());

            let mut parts = vec![format!("{home}/.local/bin")];
            if let Some(current_path) = std::env::var_os("PATH") {
                parts.extend(
                    std::env::split_paths(&current_path).map(|p| p.to_string_lossy().into_owned()),
                );
            }
            parts.extend([
                "/usr/local/bin".to_string(),
                "/usr/bin".to_string(),
                "/bin".to_string(),
            ]);
            let mut dedup = Vec::new();
            for p in parts {
                if !dedup.iter().any(|v| v == &p) {
                    dedup.push(p);
                }
            }
            env.insert("PATH".to_string(), dedup.join(":"));
        }
    }

    env
}

/// systemd user session が利用可能か検証する。
///
/// `systemctl --user status` が成功するか確認し、失敗時は
/// 原因を含むエラーメッセージを返す。
fn assert_systemd_user_available() -> Result<(), EgoPulseError> {
    assert_command_exists("systemctl")?;

    let output = systemctl_cmd(&["status"])?;
    if output.status.success() {
        return Ok(());
    }

    let detail = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    )
    .trim()
    .to_string();

    if detail.to_lowercase().contains("not found") {
        return Err(EgoPulseError::Internal(
            "systemctl is not available; systemd user services are required".into(),
        ));
    }

    Err(EgoPulseError::Internal(format!(
        "systemctl --user unavailable: {detail}"
    )))
}

fn ensure_user_session() -> Result<(), EgoPulseError> {
    if let Ok(output) = systemctl_cmd(&["status"]) {
        if output.status.success() {
            return Ok(());
        }
    }

    let uid_output = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| EgoPulseError::Internal(format!("failed to run id -u: {e}")))?;
    if !uid_output.status.success() {
        let stderr = String::from_utf8_lossy(&uid_output.stderr)
            .trim()
            .to_string();
        return Err(EgoPulseError::Internal(format!(
            "failed to resolve uid: {stderr}"
        )));
    }
    let uid = String::from_utf8_lossy(&uid_output.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|e| EgoPulseError::Internal(format!("failed to parse uid: {e}")))?;

    if std::env::var("XDG_RUNTIME_DIR").is_err() {
        let runtime_dir = format!("/run/user/{uid}");
        if !std::path::Path::new(&runtime_dir).exists() {
            let linger_output = ProcessCommand::new("loginctl")
                .args(["enable-linger", &uid.to_string()])
                .output()
                .map_err(|e| {
                    EgoPulseError::Internal(format!("failed to run loginctl enable-linger: {e}"))
                })?;
            if !linger_output.status.success() {
                let stderr = String::from_utf8_lossy(&linger_output.stderr)
                    .trim()
                    .to_string();
                return Err(EgoPulseError::Internal(format!(
                    "loginctl enable-linger failed: {stderr}"
                )));
            }
            println!("Enabled lingering for uid {uid}");
        }
        // SAFETY: XDG_RUNTIME_DIR の設定はプロセス内で安全。
        // 他スレッドへの影響はない（gateway コマンドはシングルスレッド）。
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
        }
    }

    assert_systemd_user_available()
}

fn assert_command_exists(cmd: &str) -> Result<(), EgoPulseError> {
    let output = ProcessCommand::new("which")
        .arg(cmd)
        .output()
        .map_err(|e| EgoPulseError::Internal(format!("failed to run which: {e}")))?;
    if !output.status.success() {
        return Err(EgoPulseError::Internal(format!(
            "'{cmd}' not found in PATH"
        )));
    }
    Ok(())
}

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

fn systemd_escape_env(value: &str) -> String {
    assert!(
        !value.contains('\n'),
        "environment variable must not contain newlines"
    );
    let needs_quoting = value.is_empty()
        || value.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\' || c == '\'');
    if !needs_quoting {
        return value.to_string();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// systemd ユニットファイルの内容を生成する。
fn render_systemd_unit(
    exe_path: &str,
    config_path: &std::path::Path,
    service_env: &BTreeMap<String, String>,
) -> String {
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

    let mut env_lines = String::new();
    for (key, value) in service_env {
        let kv = format!("{key}={}", systemd_escape_env(value));
        env_lines.push_str(&format!("Environment={kv}\n"));
    }

    format!(
        "[Unit]
Description=EgoPulse Agent Runtime
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory={working_dir}
ExecStart={exe_path} --config \"{escaped_config}\" run
{env_lines}\
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
            ensure_user_session()?;

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

            let service_env = build_service_env();

            let unit = unit_path()?;
            let unit_dir = unit
                .parent()
                .ok_or_else(|| EgoPulseError::Internal("invalid unit file path".into()))?;
            std::fs::create_dir_all(unit_dir).map_err(|e| {
                EgoPulseError::Internal(format!("failed to create unit directory: {e}"))
            })?;

            let already_installed = unit.exists();
            let unit_content =
                render_systemd_unit(&exe_path.to_string_lossy(), &config_path, &service_env);
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
            ensure_user_session()?;
            ensure_success(systemctl_cmd(&["start", SERVICE_NAME])?, "start service")?;
            println!("egopulse service started");
            Ok(())
        }
        GatewayAction::Stop => {
            ensure_user_session()?;
            ensure_success(systemctl_cmd(&["stop", SERVICE_NAME])?, "stop service")?;
            println!("egopulse service stopped");
            Ok(())
        }
        GatewayAction::Uninstall => {
            ensure_user_session()?;
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
            ensure_user_session()?;
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
            ensure_user_session()?;
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
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn render_systemd_unit_contains_expected_directives() {
        let config_path = PathBuf::from("/home/user/.egopulse/egopulse.config.yaml");
        let mut service_env = BTreeMap::new();
        service_env.insert("HOME".to_string(), "/home/user".to_string());
        service_env.insert(
            "PATH".to_string(),
            "/home/user/.local/bin:/usr/local/bin:/usr/bin:/bin".to_string(),
        );

        let unit = render_systemd_unit("/usr/local/bin/egopulse", &config_path, &service_env);

        assert!(unit.contains(
            "ExecStart=/usr/local/bin/egopulse --config \"/home/user/.egopulse/egopulse.config.yaml\" run"
        ));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("RestartSec=10"));
        assert!(unit.contains("KillMode=process"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Environment=HOME=/home/user"));
        assert!(unit.contains("Environment=PATH="));
    }

    #[test]
    fn render_systemd_unit_escapes_config_path_with_special_chars() {
        let config_path = PathBuf::from("/tmp/ego pulse/config dir/egopulse.config.yaml");
        let service_env = BTreeMap::new();

        let unit = render_systemd_unit("/usr/local/bin/egopulse", &config_path, &service_env);

        assert!(unit.contains("/tmp/ego pulse/config dir/egopulse.config.yaml"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn render_systemd_unit_without_service_env() {
        let config_path = PathBuf::from("/home/user/.egopulse/egopulse.config.yaml");
        let service_env = BTreeMap::new();

        let unit = render_systemd_unit("/usr/local/bin/egopulse", &config_path, &service_env);

        assert!(!unit.contains("Environment="));
    }

    #[test]
    fn systemd_escape_env_plain_value() {
        assert_eq!(systemd_escape_env("/usr/bin"), "/usr/bin");
    }

    #[test]
    fn systemd_escape_env_value_with_spaces() {
        assert_eq!(
            systemd_escape_env("/path with spaces"),
            "\"/path with spaces\""
        );
    }

    #[test]
    fn systemd_escape_env_value_with_quotes() {
        assert_eq!(systemd_escape_env("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn build_service_env_contains_expected_keys() {
        let env = build_service_env();

        assert!(env.contains_key("HOME"));
        assert!(env.contains_key("PATH"));
        assert!(!env.contains_key("TMPDIR"));
        assert!(!env.contains_key("EGOPULSE_CONFIG"));
    }

    #[test]
    #[should_panic(expected = "must not contain newlines")]
    fn systemd_escape_env_rejects_newlines() {
        systemd_escape_env("line1\nline2");
    }
}

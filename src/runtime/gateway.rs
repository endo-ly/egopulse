//! systemd ゲートウェイ管理と自己更新処理。
//!
//! `egopulse gateway` サブコマンド向けに unit file の生成・systemctl 実行・
//! 最新リリースへの更新処理をまとめる。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use crate::config::Config;
use crate::error::EgoPulseError;
use clap::Subcommand;
use sha2::{Digest, Sha256};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASE_TAG: Option<&str> = option_env!("EGOPULSE_RELEASE_TAG");

const SERVICE_NAME: &str = "egopulse.service";
const USER_BIN_DIR: &str = ".local/bin";
const BINARY_NAME: &str = "egopulse";

/// Minimum time (seconds) the service must remain `active` before declaring startup success.
const SERVICE_START_MIN_OBSERVE_SECS: u64 = 2;
/// Maximum time (seconds) to wait for the service to become active after start/restart.
const SERVICE_START_TIMEOUT_SECS: u64 = 10;
/// Interval (milliseconds) between `is-active` polls during startup verification.
const SERVICE_START_POLL_INTERVAL_MS: u64 = 500;
/// Number of journal log lines to show on startup failure.
const SERVICE_FAILURE_LOG_LINES: usize = 10;

struct ExtractedBinary {
    _tmp_dir: tempfile::TempDir,
    path: PathBuf,
}

/// Parsed responses collected from the live runtime endpoints.
///
/// Keeping the JSON values together means the gateway fetches and parses each
/// endpoint once, while the text and JSON renderers can share the same data.
struct LiveGatewayStatus {
    status: serde_json::Value,
    telemetry: serde_json::Value,
}

// ---------------------------------------------------------------------------
// systemd service context and lifecycle
// ---------------------------------------------------------------------------

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
fn assert_systemd_user_available(runtime_dir: Option<&str>) -> Result<(), EgoPulseError> {
    assert_command_exists("systemctl")?;

    let output = systemctl_cmd(&["status"], runtime_dir)?;
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

fn ensure_user_session() -> Result<Option<String>, EgoPulseError> {
    if let Ok(output) = systemctl_cmd(&["status"], None) {
        if output.status.success() {
            return Ok(None);
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
        assert_systemd_user_available(Some(&runtime_dir))?;
        return Ok(Some(runtime_dir));
    }

    assert_systemd_user_available(None)?;
    Ok(None)
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
    /// Show systemd service status with live runtime info
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Restart the systemd service
    Restart,
}

fn resolve_config_for_service(cli_config: Option<&Path>) -> Result<PathBuf, EgoPulseError> {
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

fn build_systemctl_command(args: &[&str], runtime_dir: Option<&str>) -> ProcessCommand {
    let mut command = ProcessCommand::new("systemctl");
    command.arg("--user").args(args);
    if let Some(runtime_dir) = runtime_dir {
        command.env("XDG_RUNTIME_DIR", runtime_dir).env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={runtime_dir}/bus"),
        );
    }
    command
}

fn systemctl_cmd(
    args: &[&str],
    runtime_dir: Option<&str>,
) -> Result<std::process::Output, EgoPulseError> {
    build_systemctl_command(args, runtime_dir)
        .output()
        .map_err(|e| EgoPulseError::Internal(format!("failed to run systemctl --user: {e}")))
}

struct SystemdUserService {
    runtime_dir: Option<String>,
}

impl SystemdUserService {
    fn connect() -> Result<Self, EgoPulseError> {
        Ok(Self {
            runtime_dir: ensure_user_session()?,
        })
    }

    fn from_current_environment() -> Self {
        Self {
            runtime_dir: std::env::var("XDG_RUNTIME_DIR").ok(),
        }
    }

    fn command(&self, args: &[&str]) -> Result<std::process::Output, EgoPulseError> {
        systemctl_cmd(args, self.runtime_dir.as_deref())
    }

    fn ensure_success(&self, args: &[&str], action: &str) -> Result<(), EgoPulseError> {
        let output = self.command(args)?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Err(EgoPulseError::Internal(format!(
            "{action} failed: {stderr} {stdout}"
        )))
    }

    fn verify_started(&self) -> Result<(), EgoPulseError> {
        let start = std::time::Instant::now();
        let min_observe = std::time::Duration::from_secs(SERVICE_START_MIN_OBSERVE_SECS);
        let timeout = std::time::Duration::from_secs(SERVICE_START_TIMEOUT_SECS);
        let interval = std::time::Duration::from_millis(SERVICE_START_POLL_INTERVAL_MS);

        loop {
            std::thread::sleep(interval);

            let output = self.command(&["is-active", SERVICE_NAME])?;
            let state = String::from_utf8_lossy(&output.stdout).trim().to_string();

            match state.as_str() {
                "active" if start.elapsed() >= min_observe => return Ok(()),
                "failed" => return Err(self.start_failure()),
                _ if start.elapsed() >= timeout => return Err(self.start_failure()),
                _ => {}
            }
        }
    }

    fn start_failure(&self) -> EgoPulseError {
        let mut msg = "egopulse service failed to start".to_string();
        if let Some(logs) = self.recent_logs() {
            msg.push_str("\n\nRecent logs:\n");
            msg.push_str(&logs);
        }
        EgoPulseError::Internal(msg)
    }

    fn recent_logs(&self) -> Option<String> {
        let mut cmd = ProcessCommand::new("journalctl");
        cmd.args([
            "--user",
            "-u",
            SERVICE_NAME,
            "--no-pager",
            "-n",
            &SERVICE_FAILURE_LOG_LINES.to_string(),
        ]);
        if let Some(runtime_dir) = self.runtime_dir.as_deref() {
            cmd.env("XDG_RUNTIME_DIR", runtime_dir).env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={runtime_dir}/bus"),
            );
        }
        let output = cmd.output().ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!stdout.is_empty()).then_some(stdout)
    }

    fn restart_and_verify(&self) -> Result<(), EgoPulseError> {
        self.ensure_success(&["restart", SERVICE_NAME], "restart egopulse service")?;
        self.verify_started()
    }

    fn show_systemctl_status(&self) -> Result<(), EgoPulseError> {
        let output = self.command(&["status", SERVICE_NAME, "--no-pager"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        print!("{stdout}{stderr}");
        Ok(())
    }
}

fn restart_installed_service() -> Result<(), EgoPulseError> {
    let unit = unit_path()?;
    if !unit.exists() {
        println!("Service not installed, skipping restart");
        return Ok(());
    }

    SystemdUserService::from_current_environment().restart_and_verify()?;
    println!("egopulse service restarted");
    Ok(())
}

// ---------------------------------------------------------------------------
// Live status collection and rendering
// ---------------------------------------------------------------------------

async fn fetch_live_status(cli_config: Option<&Path>) -> Option<LiveGatewayStatus> {
    let config = resolve_config_for_service(cli_config).ok()?;
    let loaded = Config::load_allow_missing_api_key(Some(&config)).ok()?;
    let auth_token = loaded.web_auth_token()?.to_owned();
    let base_url = format!("http://127.0.0.1:{}", loaded.web_port());

    fetch_live_status_at(&base_url, &auth_token).await
}

async fn fetch_live_status_at(base_url: &str, auth_token: &str) -> Option<LiveGatewayStatus> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;

    let health_resp = client.get(format!("{base_url}/health")).send().await.ok()?;
    if !matches!(
        health_resp.status(),
        reqwest::StatusCode::OK | reqwest::StatusCode::SERVICE_UNAVAILABLE
    ) {
        return None;
    }
    let health_text = health_resp.text().await.ok()?;
    // Validate the health response as JSON only; it is not retained in LiveGatewayStatus.
    let _: serde_json::Value = serde_json::from_str(&health_text).ok()?;

    let status_resp = client
        .get(format!("{base_url}/api/status"))
        .bearer_auth(auth_token)
        .send()
        .await
        .ok()?;
    if !status_resp.status().is_success() {
        return None;
    }
    let status_text = status_resp.text().await.ok()?;
    let status: serde_json::Value = serde_json::from_str(&status_text).ok()?;

    let telemetry_resp = client
        .get(format!("{base_url}/telemetry"))
        .bearer_auth(auth_token)
        .send()
        .await
        .ok()?;
    if !telemetry_resp.status().is_success() {
        return None;
    }
    let telemetry_text = telemetry_resp.text().await.ok()?;
    let telemetry: serde_json::Value = serde_json::from_str(&telemetry_text).ok()?;

    Some(LiveGatewayStatus { status, telemetry })
}

/// Sums all `value` fields from the array associated with `metric_name` in a
/// metrics object.
///
/// Returns `None` when the key is absent (so the caller can skip display).
fn sum_metric_values(
    metrics: &serde_json::Map<String, serde_json::Value>,
    metric_name: &str,
) -> Option<u64> {
    metrics.get(metric_name).and_then(|entries| {
        let arr = entries.as_array()?;
        if arr.is_empty() {
            return None;
        }
        Some(
            arr.iter()
                .map(|e| e["value"].as_f64().unwrap_or(0.0) as u64)
                .sum(),
        )
    })
}

fn sum_metric_by_label(
    metrics: &serde_json::Map<String, serde_json::Value>,
    metric_name: &str,
    label_key: &str,
    label_value: &str,
) -> Option<u64> {
    metrics.get(metric_name).and_then(|entries| {
        let arr = entries.as_array()?;
        if arr.is_empty() {
            return None;
        }
        let sum: u64 = arr
            .iter()
            .filter(|e| {
                e.get("labels")
                    .and_then(|l| l.get(label_key))
                    .and_then(|v| v.as_str())
                    .is_some_and(|v| v == label_value)
            })
            .map(|e| e["value"].as_f64().unwrap_or(0.0) as u64)
            .sum();
        if sum == 0
            && arr
                .iter()
                .all(|e| e["value"].as_f64().unwrap_or(0.0) == 0.0)
        {
            return None;
        }
        Some(sum)
    })
}

fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m {s}s")
    } else if mins > 0 {
        format!("{mins}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn format_gateway_status_values(
    health: &serde_json::Value,
    telemetry: Option<&serde_json::Value>,
) -> String {
    let mut out = String::new();
    append_health_status(health, telemetry.is_none(), &mut out);
    append_telemetry(telemetry, &mut out);
    out
}

fn append_health_status(
    health: &serde_json::Value,
    include_recent_error_count: bool,
    out: &mut String,
) {
    let status_label = if health["ok"].as_bool().unwrap_or(false) {
        "healthy"
    } else {
        "unhealthy"
    };
    out.push_str(&format!("Service: active (systemd)  [{status_label}]\n\n"));

    if let Some(version) = health["version"].as_str() {
        let pid = health["pid"].as_u64().unwrap_or(0);
        let uptime = format_uptime(health["uptime_secs"].as_u64().unwrap_or(0));
        out.push_str(&format!(
            "EgoPulse v{version}  PID {pid}  uptime {uptime}\n"
        ));
    }

    if let Some(db) = health.get("db") {
        let db_ok = db["ok"].as_bool().unwrap_or(false);
        let marker = if db_ok { "●" } else { "✗" };
        let label = if db_ok { "ok" } else { "unhealthy" };
        out.push_str(&format!("DB       {marker} {label}\n"));
    }

    if let Some(channels) = health.get("channels") {
        out.push('\n');
        out.push_str("Channels\n");
        for name in ["web", "discord", "telegram"] {
            if let Some(channel) = channels.get(name) {
                let state = channel["state"].as_str().unwrap_or("unknown");
                let marker = if state == "running" { "●" } else { "✗" };
                out.push_str(&format!("{name:>10} {marker} {state}\n"));
            }
        }
    }

    append_mcp_status(health, out);

    if let Some(active_turns) = health.get("active_turns").and_then(|v| v.as_u64()) {
        out.push_str(&format!("Active Turns: {active_turns}\n"));
    }

    if include_recent_error_count
        && let Some(count) = health.get("recent_errors_count").and_then(|v| v.as_u64())
        && count > 0
    {
        out.push('\n');
        out.push_str(&format!("Recent Errors (last 1h): {count}\n"));
    }
}

fn append_mcp_status(health: &serde_json::Value, out: &mut String) {
    let Some(mcp) = health.get("mcp").filter(|mcp| !mcp.is_null()) else {
        return;
    };

    let healthy = mcp["healthy"].as_u64().unwrap_or(0);
    let failed = mcp["failed"].as_u64().unwrap_or(0);
    let connected_names = mcp_server_names(mcp, true);
    let failed_names = mcp_server_names(mcp, false);

    if !connected_names.is_empty() {
        out.push_str(&format!(
            "MCP       {healthy} connected ({})\n",
            connected_names.join(", ")
        ));
    } else if healthy > 0 {
        out.push_str(&format!("MCP       {healthy} connected\n"));
    }

    if !failed_names.is_empty() {
        out.push_str(&format!(
            "          {failed} failed ({})\n",
            failed_names.join(", ")
        ));
    } else if failed > 0 {
        out.push_str(&format!("          {failed} failed\n"));
    }
}

fn mcp_server_names(mcp: &serde_json::Value, connected: bool) -> Vec<String> {
    mcp.get("servers")
        .and_then(serde_json::Value::as_array)
        .map(|servers| {
            servers
                .iter()
                .filter(|server| server["connected"].as_bool().unwrap_or(false) == connected)
                .filter_map(|server| server["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn append_telemetry(telemetry: Option<&serde_json::Value>, out: &mut String) {
    append_metrics(telemetry, out);
    append_telemetry_details(telemetry, out);
}

fn append_metrics(telemetry: Option<&serde_json::Value>, out: &mut String) {
    let Some(metrics) = telemetry
        .and_then(|telemetry| telemetry.get("metrics"))
        .and_then(serde_json::Value::as_object)
        .filter(|metrics| !metrics.is_empty())
    else {
        return;
    };

    let turns = sum_metric_values(metrics, "egopulse_turns_total");
    let errors = sum_metric_values(metrics, "egopulse_turn_errors_total");
    let tokens_in = sum_metric_by_label(metrics, "egopulse_llm_tokens_total", "direction", "input");
    let tokens_out =
        sum_metric_by_label(metrics, "egopulse_llm_tokens_total", "direction", "output");
    let tool_calls = sum_metric_values(metrics, "egopulse_tool_calls_total");

    if turns.is_none()
        && errors.is_none()
        && tokens_in.is_none()
        && tokens_out.is_none()
        && tool_calls.is_none()
    {
        return;
    }

    out.push('\n');
    out.push_str("Metrics\n");

    if turns.is_some() || errors.is_some() {
        let mut line = String::from("  ");
        if let Some(turns) = turns {
            line.push_str(&format!("Turns: {turns}  "));
        }
        if let Some(errors) = errors {
            line.push_str(&format!("Errors: {errors}  "));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }

    if tokens_in.is_some() || tokens_out.is_some() {
        let mut line = String::from("  Tokens:");
        if let Some(tokens_in) = tokens_in {
            line.push_str(&format!(" {tokens_in} in"));
        }
        if let Some(tokens_out) = tokens_out {
            line.push_str(&format!(" / {tokens_out} out"));
        }
        out.push_str(&line);
        out.push('\n');
    }

    if let Some(tool_calls) = tool_calls {
        out.push_str(&format!("  Tool Calls: {tool_calls}\n"));
    }
}

fn append_telemetry_details(telemetry: Option<&serde_json::Value>, out: &mut String) {
    let Some(telemetry) = telemetry else {
        return;
    };

    if let Some(errors) = telemetry.get("recent_errors").and_then(|v| v.as_array()) {
        if !errors.is_empty() {
            out.push('\n');
            out.push_str(&format!("Recent Errors (last 1h): {}\n", errors.len()));
            for error in errors.iter().rev().take(5) {
                let kind = error["error_kind"].as_str().unwrap_or("?");
                let trace = error["trace_id"].as_str().unwrap_or("?");
                let summary = error["summary"].as_str().unwrap_or("");
                out.push_str(&format!("  [{kind}] trace={trace} {summary}\n"));
            }
        }
    }

    if let Some(turns) = telemetry.get("recent_turns").and_then(|v| v.as_array()) {
        if !turns.is_empty() {
            out.push('\n');
            let last_turns: Vec<&serde_json::Value> = turns.iter().rev().take(5).collect();
            out.push_str(&format!(
                "Recent Turns (last {} shown):\n",
                last_turns.len()
            ));
            for turn in last_turns {
                let agent = turn["agent_id"].as_str().unwrap_or("?");
                let channel = turn["channel"].as_str().unwrap_or("?");
                let ok_marker = if turn["ok"].as_bool().unwrap_or(false) {
                    "ok"
                } else {
                    "FAIL"
                };
                let duration = turn["duration_secs"].as_f64().unwrap_or(0.0);
                out.push_str(&format!(
                    "  {agent}/{channel} [{ok_marker}] {duration:.1}s\n"
                ));
            }
        }
    }
}

fn merge_health_and_telemetry_values(
    health_json: &serde_json::Value,
    telemetry: Option<&serde_json::Value>,
) -> String {
    use crate::channels::web::health::{DetailedStatusResponse, GatewayStatusResponse};

    let health: DetailedStatusResponse = match serde_json::from_value(health_json.clone()) {
        Ok(h) => h,
        Err(_) => {
            let mut health = health_json.clone();
            if let (Some(telemetry), Some(obj)) = (telemetry, health.as_object_mut()) {
                obj.insert("telemetry".to_string(), telemetry.clone());
            }
            return serde_json::to_string_pretty(&health)
                .unwrap_or_else(|_| health_json.to_string());
        }
    };

    let telemetry = telemetry.and_then(|value| serde_json::from_value(value.clone()).ok());
    let merged = GatewayStatusResponse { health, telemetry };
    serde_json::to_string_pretty(&merged).unwrap_or_else(|_| health_json.to_string())
}

fn print_gateway_help() {
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
}

// ---------------------------------------------------------------------------
// Gateway command dispatch
// ---------------------------------------------------------------------------

fn install_service(cli_config: Option<&Path>) -> Result<(), EgoPulseError> {
    let systemd = SystemdUserService::connect()?;
    let exe_path = std::env::current_exe()
        .map_err(|e| EgoPulseError::Internal(format!("failed to resolve binary path: {e}")))?;
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
    std::fs::create_dir_all(unit_dir)
        .map_err(|e| EgoPulseError::Internal(format!("failed to create unit directory: {e}")))?;

    let already_installed = unit.exists();
    let unit_content = render_systemd_unit(
        &exe_path.to_string_lossy(),
        &config_path,
        &build_service_env(),
    );
    std::fs::write(&unit, unit_content)
        .map_err(|e| EgoPulseError::Internal(format!("failed to write unit file: {e}")))?;

    systemd.ensure_success(&["daemon-reload"], "daemon-reload")?;

    if already_installed {
        systemd.ensure_success(&["restart", SERVICE_NAME], "restart service")?;
        systemd.verify_started()?;
        println!("Updated and restarted egopulse service: {}", unit.display());
    } else {
        systemd.ensure_success(&["enable", "--now", SERVICE_NAME], "enable service")?;
        systemd.verify_started()?;
        println!("Installed and started egopulse service: {}", unit.display());
    }
    Ok(())
}

fn start_service() -> Result<(), EgoPulseError> {
    let systemd = SystemdUserService::connect()?;
    systemd.ensure_success(&["start", SERVICE_NAME], "start service")?;
    systemd.verify_started()?;
    println!("egopulse service started");
    Ok(())
}

fn stop_service() -> Result<(), EgoPulseError> {
    let systemd = SystemdUserService::connect()?;
    systemd.ensure_success(&["stop", SERVICE_NAME], "stop service")?;
    println!("egopulse service stopped");
    Ok(())
}

fn uninstall_service() -> Result<(), EgoPulseError> {
    let systemd = SystemdUserService::connect()?;
    let _ = systemd.command(&["disable", "--now", SERVICE_NAME]);

    let unit = unit_path()?;
    if unit.exists() {
        std::fs::remove_file(&unit)
            .map_err(|e| EgoPulseError::Internal(format!("failed to remove unit file: {e}")))?;
    }
    systemd.ensure_success(&["daemon-reload"], "daemon-reload")?;

    println!("Uninstalled egopulse service");
    Ok(())
}

async fn show_gateway_status(cli_config: Option<&Path>, json: bool) -> Result<(), EgoPulseError> {
    let systemd = SystemdUserService::connect()?;
    let is_active_output = systemd.command(&["is-active", SERVICE_NAME])?;
    let state = String::from_utf8_lossy(&is_active_output.stdout)
        .trim()
        .to_string();
    let is_active = state == "active";

    if is_active {
        match fetch_live_status(cli_config).await {
            Some(status) if json => {
                let merged =
                    merge_health_and_telemetry_values(&status.status, Some(&status.telemetry));
                println!("{merged}");
            }
            Some(status) => {
                print!(
                    "{}",
                    format_gateway_status_values(&status.status, Some(&status.telemetry))
                );
            }
            None => systemd.show_systemctl_status()?,
        }
    } else if json {
        let status_json = serde_json::json!({
            "ok": false,
            "service": state,
            "recent_logs": systemd.recent_logs(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&status_json).unwrap_or_default()
        );
    } else {
        systemd.show_systemctl_status()?;
        if let Some(logs) = systemd.recent_logs() {
            println!("\nLast error:\n{logs}");
        }
    }
    Ok(())
}

fn restart_service_action() -> Result<(), EgoPulseError> {
    SystemdUserService::connect()?.restart_and_verify()?;
    println!("egopulse service restarted");
    Ok(())
}

/// Executes the requested gateway action for the EgoPulse systemd service.
///
/// # Errors
///
/// Returns an error when systemd operations or configuration resolution fail.
pub async fn run_gateway(
    cli_config: Option<&Path>,
    action: Option<GatewayAction>,
) -> Result<(), EgoPulseError> {
    let Some(action) = action else {
        print_gateway_help();
        return Ok(());
    };

    match action {
        GatewayAction::Install => install_service(cli_config),
        GatewayAction::Start => start_service(),
        GatewayAction::Stop => stop_service(),
        GatewayAction::Uninstall => uninstall_service(),
        GatewayAction::Status { json } => show_gateway_status(cli_config, json).await,
        GatewayAction::Restart => restart_service_action(),
    }
}

// ---------------------------------------------------------------------------
// Self-update
// ---------------------------------------------------------------------------

/// Updates the installed EgoPulse binary from the latest GitHub release.
pub async fn run_update() -> Result<(), EgoPulseError> {
    println!("Current version: {VERSION}");
    if let Some(release_tag) = RELEASE_TAG {
        println!("Current release: {release_tag}");
    }

    let client = reqwest::Client::builder()
        .user_agent(format!("egopulse/{VERSION}"))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| EgoPulseError::Internal(format!("failed to create HTTP client: {e}")))?;

    ensure_user_updatable_install()?;

    print!("Checking for updates... ");
    let (tag_name, assets) = fetch_latest_release(&client).await?;
    if RELEASE_TAG == Some(tag_name.as_str()) {
        println!("already up to date.");
        return Ok(());
    }
    println!("found {tag_name}");

    let target = detect_target_triple();
    let asset_url = resolve_asset_url(&assets, &target).ok_or_else(|| {
        EgoPulseError::Internal(format!(
            "no binary found for {target} in the latest release ({tag_name})"
        ))
    })?;
    let checksum_url = resolve_checksum_url(&assets).ok_or_else(|| {
        EgoPulseError::Internal(format!(
            "no SHA256SUMS.txt found in the latest release ({tag_name})"
        ))
    })?;

    let new_binary = download_and_extract(&client, &asset_url, &checksum_url).await?;
    replace_binary(&new_binary.path)?;

    println!("Restarting service...");
    restart_installed_service()?;
    println!("Update completed: {VERSION} -> {tag_name}");
    Ok(())
}

fn repo_api_path() -> &'static str {
    const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
    const PREFIX: &str = "https://github.com/";
    if let Some(stripped) = REPO_URL.strip_prefix(PREFIX) {
        stripped
    } else {
        if let Some(pos) = REPO_URL.find("://") {
            let rest = &REPO_URL[pos + 3..];
            rest.trim_start_matches('/')
        } else {
            REPO_URL
        }
    }
}

/// Detects the current target triple matching the format used in release asset names.
fn detect_target_triple() -> String {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    match os {
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "macos" => format!("{arch}-apple-darwin"),
        _ => format!("{arch}-{os}"),
    }
}

fn user_binary_path() -> Result<PathBuf, EgoPulseError> {
    let home = dirs::home_dir()
        .ok_or_else(|| EgoPulseError::Internal("HOME directory could not be resolved".into()))?;
    Ok(home.join(USER_BIN_DIR).join(BINARY_NAME))
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn ensure_user_updatable_install() -> Result<(), EgoPulseError> {
    let expected = user_binary_path()?;

    let expected_metadata = std::fs::symlink_metadata(&expected).map_err(|e| {
        EgoPulseError::Internal(format!(
            "self-update requires a user-local install at {}: {e}",
            expected.display()
        ))
    })?;
    if expected_metadata.file_type().is_symlink() {
        return Err(EgoPulseError::Internal(format!(
            "self-update requires {} to be a regular file, not a symlink. Reinstall EgoPulse with:\n  install -m 0755 target/release/egopulse \"$HOME/.local/bin/egopulse\"",
            expected.display()
        )));
    }

    let current = std::env::current_exe()
        .map_err(|e| EgoPulseError::Internal(format!("failed to get current exe: {e}")))?;
    let current = normalize_existing_path(&current);
    let expected_dir = expected.parent().ok_or_else(|| {
        EgoPulseError::Internal("could not determine user binary directory".into())
    })?;

    if current.file_name() == Some(std::ffi::OsStr::new(BINARY_NAME))
        && current.parent() == Some(expected_dir)
    {
        return Ok(());
    }

    Err(EgoPulseError::Internal(format!(
        "self-update requires a user-local install at {}. Current binary is {}. Reinstall EgoPulse with:\n  mkdir -p \"$HOME/.local/bin\"\n  curl -fsSL https://raw.githubusercontent.com/endo-ly/egopulse/main/scripts/install.sh | bash\nThen refresh the systemd user service with:\n  egopulse gateway install",
        expected.display(),
        current.display()
    )))
}

/// Fetches tag_name and assets array from the GitHub Releases API.
async fn fetch_latest_release(
    client: &reqwest::Client,
) -> Result<(String, Vec<serde_json::Value>), EgoPulseError> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        repo_api_path()
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| EgoPulseError::Internal(format!("failed to fetch latest release: {e}")))?
        .error_for_status()
        .map_err(|e| EgoPulseError::Internal(format!("GitHub API error: {e}")))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| EgoPulseError::Internal(format!("failed to parse release JSON: {e}")))?;

    let tag_name = json["tag_name"]
        .as_str()
        .ok_or_else(|| EgoPulseError::Internal("missing 'tag_name' in release response".into()))?
        .to_string();

    let assets = json["assets"]
        .as_array()
        .ok_or_else(|| EgoPulseError::Internal("missing 'assets' in release response".into()))?
        .clone();

    Ok((tag_name, assets))
}

/// Finds the download URL of the tar.gz asset matching the given target triple.
fn resolve_asset_url(assets: &[serde_json::Value], target: &str) -> Option<String> {
    assets.iter().find_map(|asset| {
        let name = asset["name"].as_str().unwrap_or("");
        if name.contains(target) && name.ends_with(".tar.gz") {
            asset["browser_download_url"].as_str().map(String::from)
        } else {
            None
        }
    })
}

/// Finds the download URL of the release checksum manifest.
fn resolve_checksum_url(assets: &[serde_json::Value]) -> Option<String> {
    assets.iter().find_map(|asset| {
        let name = asset["name"].as_str().unwrap_or("");
        if name == "SHA256SUMS.txt" {
            asset["browser_download_url"].as_str().map(String::from)
        } else {
            None
        }
    })
}

/// Downloads, verifies, and extracts the `egopulse` binary from a tar.gz archive.
async fn download_and_extract(
    client: &reqwest::Client,
    url: &str,
    checksum_url: &str,
) -> Result<ExtractedBinary, EgoPulseError> {
    let bytes = download_archive(client, url).await?;

    eprint!("  Verifying checksum... ");
    verify_archive_checksum(client, url, checksum_url, &bytes).await?;
    eprintln!("ok");

    extract_binary(&bytes)
}

async fn download_archive(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, EgoPulseError> {
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| EgoPulseError::Internal(format!("download failed: {e}")))?
        .error_for_status()
        .map_err(|e| EgoPulseError::Internal(format!("download error: {e}")))?;

    let total_size = resp.content_length();
    let mut bytes = Vec::new();
    let mut downloaded: u64 = 0;
    let mut last_reported_percent: u8 = 0;

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| EgoPulseError::Internal(format!("download error: {e}")))?
    {
        bytes.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;

        if let Some(total) = total_size {
            let percent = (downloaded as f64 / total as f64 * 100.0) as u8;
            if percent >= last_reported_percent + 5 || percent >= 100 {
                eprint!(
                    "\r  {} / {} ({}%)  ",
                    format_bytes(downloaded),
                    format_bytes(total),
                    percent
                );
                let _ = std::io::Write::flush(&mut std::io::stderr());
                last_reported_percent = percent;
            }
        } else {
            eprint!("\r  {} downloaded  ", format_bytes(downloaded));
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }
    }
    eprintln!();

    Ok(bytes)
}

fn extract_binary(bytes: &[u8]) -> Result<ExtractedBinary, EgoPulseError> {
    eprint!("  Extracting binary... ");
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);

    let tmp_dir = tempfile::tempdir()
        .map_err(|e| EgoPulseError::Internal(format!("failed to create temp dir: {e}")))?;

    let mut entries = archive
        .entries()
        .map_err(|e| EgoPulseError::Internal(format!("failed to read archive entries: {e}")))?;

    let mut found = false;
    loop {
        let mut entry = match entries.next() {
            None => break,
            Some(Ok(entry)) => entry,
            Some(Err(e)) => {
                return Err(EgoPulseError::Internal(format!(
                    "error reading archive entry: {e}"
                )));
            }
        };

        let path = entry
            .path()
            .map_err(|e| EgoPulseError::Internal(format!("failed to resolve entry path: {e}")))?;

        if path.file_name().and_then(|n| n.to_str()) == Some(BINARY_NAME) {
            entry
                .unpack_in(tmp_dir.path())
                .map_err(|e| EgoPulseError::Internal(format!("failed to extract binary: {e}")))?;
            found = true;
        }
    }

    if !found {
        return Err(EgoPulseError::Internal(
            "could not find 'egopulse' binary in downloaded archive".into(),
        ));
    }

    let bin_path = tmp_dir.path().join(BINARY_NAME);
    if !bin_path.exists() {
        let bin_path = walkdir::WalkDir::new(tmp_dir.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .find(|e| e.file_name() == BINARY_NAME)
            .map(|e| e.path().to_path_buf())
            .ok_or_else(|| {
                EgoPulseError::Internal(
                    "could not find 'egopulse' binary in downloaded archive".into(),
                )
            })?;
        eprintln!("done");
        return Ok(ExtractedBinary {
            _tmp_dir: tmp_dir,
            path: bin_path,
        });
    }

    eprintln!("done");
    Ok(ExtractedBinary {
        _tmp_dir: tmp_dir,
        path: bin_path,
    })
}

async fn verify_archive_checksum(
    client: &reqwest::Client,
    archive_url: &str,
    checksum_url: &str,
    bytes: &[u8],
) -> Result<(), EgoPulseError> {
    let manifest = client
        .get(checksum_url)
        .send()
        .await
        .map_err(|e| EgoPulseError::Internal(format!("failed to fetch SHA256SUMS.txt: {e}")))?
        .error_for_status()
        .map_err(|e| EgoPulseError::Internal(format!("SHA256SUMS.txt download error: {e}")))?
        .text()
        .await
        .map_err(|e| EgoPulseError::Internal(format!("failed to read SHA256SUMS.txt: {e}")))?;

    let archive_name = archive_url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| EgoPulseError::Internal("failed to derive archive filename".into()))?;

    let expected = manifest
        .lines()
        .filter_map(parse_sha256sum_line)
        .find_map(|(digest, filename)| (filename == archive_name).then_some(digest))
        .ok_or_else(|| {
            EgoPulseError::Internal(format!(
                "SHA256SUMS.txt does not contain checksum for {archive_name}"
            ))
        })?;

    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        return Err(EgoPulseError::Internal(format!(
            "checksum mismatch for {archive_name}: expected {expected}, got {actual}"
        )));
    }

    Ok(())
}

fn parse_sha256sum_line(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.split_whitespace();
    let digest = parts.next()?;
    let filename = parts.next()?.trim_start_matches('*');
    let filename = filename.strip_prefix("./").unwrap_or(filename);
    if digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()) {
        Some((digest, filename))
    } else {
        None
    }
}

/// Atomically replaces the currently running binary with the provided new binary.
///
/// On success the old binary is kept as `.egopulse.old` in the same directory.
/// On failure the original binary is restored.
fn replace_binary(new_binary: &Path) -> Result<(), EgoPulseError> {
    let current_exe = std::env::current_exe()
        .map_err(|e| EgoPulseError::Internal(format!("failed to get current exe: {e}")))?;
    let current_exe = current_exe
        .canonicalize()
        .unwrap_or_else(|_| current_exe.clone());

    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| EgoPulseError::Internal("could not determine binary directory".into()))?;

    let staged = exe_dir.join(".egopulse.new");
    std::fs::copy(new_binary, &staged)
        .map_err(|e| EgoPulseError::Internal(format!("failed to copy new binary: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| EgoPulseError::Internal(format!("failed to set permissions: {e}")))?;
    }

    let backup = exe_dir.join(".egopulse.old");
    std::fs::rename(&current_exe, &backup).map_err(|e| {
        EgoPulseError::Internal(format!("failed to move current binary aside: {e}"))
    })?;

    if let Err(e) = std::fs::rename(&staged, &current_exe) {
        return match std::fs::rename(&backup, &current_exe) {
            Ok(()) => Err(EgoPulseError::Internal(format!(
                "failed to install new binary (rolled back): {e}"
            ))),
            Err(rollback_error) => Err(EgoPulseError::Internal(format!(
                "failed to install new binary and rollback failed: install error: {e}; rollback error: {rollback_error}"
            ))),
        };
    }

    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn format_gateway_status(health_json: &str, telemetry_json: Option<&str>) -> String {
        let health: serde_json::Value = match serde_json::from_str(health_json) {
            Ok(v) => v,
            Err(_) => return health_json.to_owned(),
        };
        let telemetry = telemetry_json.and_then(|t| serde_json::from_str(t).ok());

        super::format_gateway_status_values(&health, telemetry.as_ref())
    }

    fn merge_health_and_telemetry(health_json: &str, telemetry_json: Option<&str>) -> String {
        let health: serde_json::Value = match serde_json::from_str(health_json) {
            Ok(health) => health,
            Err(_) => return health_json.to_owned(),
        };
        let telemetry = telemetry_json.and_then(|json| serde_json::from_str(json).ok());
        super::merge_health_and_telemetry_values(&health, telemetry.as_ref())
    }

    #[test]
    fn render_systemd_unit_preserves_service_contract() {
        let config_path = PathBuf::from("/home/user/.egopulse/egopulse.config.yaml");
        let mut service_env = BTreeMap::new();
        service_env.insert("HOME".to_string(), "/home/user".to_string());
        service_env.insert(
            "PATH".to_string(),
            "/home/user/.local/bin:/usr/local/bin:/usr/bin:/bin".to_string(),
        );

        let unit =
            render_systemd_unit("/home/user/.local/bin/egopulse", &config_path, &service_env);

        assert!(unit.contains(
            "ExecStart=/home/user/.local/bin/egopulse --config \"/home/user/.egopulse/egopulse.config.yaml\" run"
        ));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("RestartSec=10"));
        assert!(unit.contains("KillMode=process"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Environment=HOME=/home/user"));
        assert!(unit.contains("Environment=PATH="));

        let without_env = render_systemd_unit(
            "/home/user/.local/bin/egopulse",
            &config_path,
            &BTreeMap::new(),
        );
        assert!(!without_env.contains("Environment="));
    }

    #[test]
    fn render_systemd_unit_escapes_config_path_with_special_chars() {
        let config_path = PathBuf::from("/tmp/ego pulse/config dir/egopulse.config.yaml");
        let unit = render_systemd_unit(
            "/home/user/.local/bin/egopulse",
            &config_path,
            &BTreeMap::new(),
        );

        assert!(unit.contains("/tmp/ego pulse/config dir/egopulse.config.yaml"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn parse_sha256sum_line_normalizes_generated_manifest_paths() {
        let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        assert_eq!(
            parse_sha256sum_line(&format!("{digest}  ./egopulse-0.1.0-linux.tar.gz")),
            Some((digest, "egopulse-0.1.0-linux.tar.gz"))
        );
        assert_eq!(
            parse_sha256sum_line(&format!("{digest} *./egopulse-0.1.0-linux.tar.gz")),
            Some((digest, "egopulse-0.1.0-linux.tar.gz"))
        );
    }

    #[test]
    fn systemd_escape_env_preserves_systemd_values() {
        for (input, expected) in [
            ("/usr/bin", "/usr/bin"),
            ("/path with spaces", "\"/path with spaces\""),
            ("a\"b", "\"a\\\"b\""),
        ] {
            assert_eq!(systemd_escape_env(input), expected);
        }
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
    fn build_systemctl_command_handles_optional_runtime_dir() {
        let with_runtime_dir = build_systemctl_command(&["status"], Some("/run/user/1000"));
        let with_envs: Vec<_> = with_runtime_dir.get_envs().collect();
        assert!(with_envs.iter().any(|(key, value)| {
            *key == OsStr::new("XDG_RUNTIME_DIR") && *value == Some(OsStr::new("/run/user/1000"))
        }));

        let without_runtime_dir = build_systemctl_command(&["status"], None);
        let without_envs: Vec<_> = without_runtime_dir.get_envs().collect();
        assert!(
            !without_envs
                .iter()
                .any(|(key, _)| *key == OsStr::new("XDG_RUNTIME_DIR"))
        );
    }

    #[test]
    #[should_panic(expected = "must not contain newlines")]
    fn systemd_escape_env_rejects_newlines() {
        systemd_escape_env("line1\nline2");
    }

    #[tokio::test]
    async fn fetch_live_status_uses_bearer_token_for_diagnostics() {
        use wiremock::matchers::{header as wiremock_header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(
                ResponseTemplate::new(503).set_body_json(serde_json::json!({"ok": false})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/status"))
            .and(wiremock_header("authorization", "Bearer gateway-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "version": "0.1.0",
                "instance_lock": {"held": true}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/telemetry"))
            .and(wiremock_header("authorization", "Bearer gateway-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metrics": {},
                "recent_turns": [],
                "recent_errors": []
            })))
            .mount(&server)
            .await;

        let status = fetch_live_status_at(&server.uri(), "gateway-secret")
            .await
            .expect("authenticated diagnostics response");
        assert_eq!(status.status["version"], "0.1.0");
        assert!(status.telemetry["metrics"].is_object());
    }

    #[tokio::test]
    async fn fetch_live_status_falls_back_on_unauthorized_diagnostics() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/status"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        assert!(
            fetch_live_status_at(&server.uri(), "gateway-secret")
                .await
                .is_none()
        );
    }

    #[test]
    fn gateway_status_json_flag_parses_with_and_without_json() {
        use clap::Parser;

        #[derive(Debug, Parser)]
        struct Cli {
            #[command(subcommand)]
            action: GatewayAction,
        }

        for (args, expected_json) in [
            (vec!["egopulse", "status", "--json"], true),
            (vec!["egopulse", "status"], false),
        ] {
            let cli: Cli = Parser::try_parse_from(args).expect("parse");
            match cli.action {
                GatewayAction::Status { json } => assert_eq!(json, expected_json),
                other => panic!("expected Status, got {other:?}"),
            }
        }
    }

    #[test]
    fn format_gateway_status_parses_ready_response() {
        let health = serde_json::json!({
            "ok": true,
            "version": "0.1.0",
            "uptime_secs": 5400,
            "pid": 12345,
            "db": { "ok": true },
            "channels": {
                "web": { "state": "running" },
                "discord": { "state": "running" },
                "telegram": { "state": "failed" }
            },
            "active_turns": 2,
            "recent_errors_count": 3
        })
        .to_string();

        let output = format_gateway_status(&health, None);

        assert!(
            output.contains("1h 30m 0s"),
            "expected formatted uptime, got: {output}"
        );
        assert!(
            output.contains("healthy"),
            "expected healthy status, got: {output}"
        );
        assert!(output.contains("v0.1.0"), "expected version, got: {output}");
        assert!(output.contains("PID 12345"), "expected PID, got: {output}");
        assert!(
            output.contains("● running"),
            "expected running channel, got: {output}"
        );
        assert!(
            output.contains("✗ failed"),
            "expected failed channel, got: {output}"
        );
        assert!(
            output.contains("Active Turns: 2"),
            "expected active turns, got: {output}"
        );
        assert!(
            output.contains("Recent Errors (last 1h): 3"),
            "expected errors count, got: {output}"
        );
        assert_eq!(
            output.matches("Recent Errors (last 1h): 3").count(),
            1,
            "error count should be shown once, got: {output}"
        );

        let without_errors = serde_json::json!({
            "ok": true,
            "version": "0.1.0",
            "uptime_secs": 3600,
            "pid": 99,
            "channels": {
                "web": { "state": "running" }
            }
        })
        .to_string();

        let output = format_gateway_status(&without_errors, None);
        assert!(
            !output.contains("Recent Errors"),
            "should not show errors section when count is 0 or absent, got: {output}"
        );
        assert!(
            output.contains("1h 0m 0s"),
            "expected 1h uptime, got: {output}"
        );
    }

    #[test]
    fn format_uptime_various() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(45), "45s");
        assert_eq!(format_uptime(90), "1m 30s");
        assert_eq!(format_uptime(5400), "1h 30m 0s");
        assert_eq!(format_uptime(90061), "1d 1h 1m");
    }

    #[test]
    fn format_gateway_status_invalid_json_passthrough() {
        let output = format_gateway_status("not json at all", None);
        assert_eq!(output, "not json at all");
    }

    #[test]
    fn format_gateway_status_with_telemetry_shows_errors_and_turns() {
        let health = serde_json::json!({
            "ok": true,
            "version": "0.1.0",
            "uptime_secs": 100,
            "pid": 1,
            "channels": {
                "web": { "state": "running" }
            },
            "active_turns": 1,
            "recent_errors_count": 2
        })
        .to_string();

        let telemetry = serde_json::json!({
            "metrics": {},
            "recent_errors": [
                {
                    "at": "2025-01-01T00:00:00Z",
                    "trace_id": "abc-123",
                    "error_kind": "turn_failure",
                    "agent_id": "alice",
                    "channel": "discord",
                    "summary": "rate limited"
                },
                {
                    "at": "2025-01-01T00:01:00Z",
                    "trace_id": "def-456",
                    "error_kind": "timeout",
                    "agent_id": "bob",
                    "channel": "web",
                    "summary": "connection lost"
                }
            ],
            "recent_turns": [
                {
                    "trace_id": "t1",
                    "agent_id": "alice",
                    "channel": "discord",
                    "started_at": "2025-01-01T00:00:00Z",
                    "duration_secs": 5.2,
                    "ok": true
                },
                {
                    "trace_id": "t2",
                    "agent_id": "bob",
                    "channel": "web",
                    "started_at": "2025-01-01T00:01:00Z",
                    "duration_secs": 0.3,
                    "ok": false
                }
            ]
        })
        .to_string();

        let output = format_gateway_status(&health, Some(&telemetry));

        assert!(
            output.contains("Recent Errors"),
            "should show errors section: {output}"
        );
        assert!(
            output.contains("trace=abc-123"),
            "should show error trace_id: {output}"
        );
        assert!(
            output.contains("turn_failure"),
            "should show error kind: {output}"
        );
        assert!(
            output.contains("rate limited"),
            "should show error summary: {output}"
        );
        assert!(
            output.contains("Recent Turns"),
            "should show turns section: {output}"
        );
        assert!(
            output.contains("alice/discord [ok]"),
            "should show successful turn: {output}"
        );
        assert!(
            output.contains("bob/web [FAIL]"),
            "should show failed turn: {output}"
        );
    }

    #[test]
    fn merge_health_and_telemetry_combines_json() {
        let health = serde_json::json!({
            "ok": true,
            "version": "0.1.0"
        })
        .to_string();

        let telemetry = serde_json::json!({
            "metrics": { "turns": 5 },
            "recent_errors": [],
            "recent_turns": []
        })
        .to_string();

        let merged = merge_health_and_telemetry(&health, Some(&telemetry));
        let parsed: serde_json::Value = serde_json::from_str(&merged).expect("valid json");

        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["version"], "0.1.0");
        assert!(parsed["telemetry"].is_object());
        assert_eq!(parsed["telemetry"]["metrics"]["turns"], 5);
    }

    #[test]
    fn merge_health_and_telemetry_without_telemetry() {
        let health = serde_json::json!({
            "ok": true,
            "version": "0.1.0"
        })
        .to_string();

        let merged = merge_health_and_telemetry(&health, None);
        let parsed: serde_json::Value = serde_json::from_str(&merged).expect("valid json");

        assert_eq!(parsed["ok"], true);
        assert!(
            parsed.get("telemetry").is_none(),
            "should not have telemetry key when no telemetry provided"
        );
    }

    #[test]
    fn format_gateway_status_db_section_handles_health_and_absence() {
        for (db, expected) in [
            (
                Some(serde_json::json!({ "ok": true })),
                Some("DB       ● ok"),
            ),
            (
                Some(serde_json::json!({ "ok": false })),
                Some("DB       ✗ unhealthy"),
            ),
            (None, None),
        ] {
            let mut health = serde_json::json!({
                "ok": true,
                "version": "0.1.0",
                "uptime_secs": 100,
                "pid": 1
            });
            if let Some(db) = db {
                health["db"] = db;
            }

            let output = format_gateway_status(&health.to_string(), None);
            match expected {
                Some(expected) => assert!(
                    output.contains(expected),
                    "expected DB section {expected:?}, got: {output}"
                ),
                None => assert!(
                    !output.contains("DB"),
                    "should not show DB section when absent, got: {output}"
                ),
            }
        }
    }

    #[test]
    fn format_gateway_status_mcp_section_shows_connected_servers() {
        // Arrange
        let health = serde_json::json!({
            "ok": true,
            "version": "0.1.0",
            "uptime_secs": 100,
            "pid": 1,
            "mcp": {
                "healthy": 2,
                "failed": 1,
                "servers": [
                    { "name": "context7", "connected": true },
                    { "name": "egograph", "connected": true },
                    { "name": "broken-svc", "connected": false }
                ]
            }
        })
        .to_string();

        // Act
        let output = format_gateway_status(&health, None);

        // Assert
        assert!(
            output.contains("MCP       2 connected (context7, egograph)"),
            "expected MCP connected section, got: {output}"
        );
        assert!(
            output.contains("1 failed (broken-svc)"),
            "expected MCP failed section, got: {output}"
        );
    }

    #[test]
    fn format_gateway_status_mcp_section_skips_missing_or_null_data() {
        for mcp in [Some(serde_json::Value::Null), None] {
            let mut health = serde_json::json!({
                "ok": true,
                "version": "0.1.0",
                "uptime_secs": 100,
                "pid": 1
            });
            if let Some(mcp) = mcp {
                health["mcp"] = mcp;
            }

            let output = format_gateway_status(&health.to_string(), None);
            assert!(
                !output.contains("MCP"),
                "should not show MCP section when data is missing, got: {output}"
            );
        }
    }

    #[test]
    fn format_gateway_status_metrics_section_shows_values() {
        // Arrange
        let health = serde_json::json!({
            "ok": true,
            "version": "0.1.0",
            "uptime_secs": 100,
            "pid": 1
        })
        .to_string();

        let telemetry = serde_json::json!({
            "metrics": {
                "egopulse_turns_total": [
                    { "labels": {}, "value": 42 }
                ],
                "egopulse_turn_errors_total": [
                    { "labels": {}, "value": 3 }
                ],
                "egopulse_llm_tokens_total": [
                    { "labels": { "direction": "input" }, "value": 15000 },
                    { "labels": { "direction": "output" }, "value": 3200 }
                ],
                "egopulse_tool_calls_total": [
                    { "labels": {}, "value": 28 }
                ]
            },
            "recent_errors": [],
            "recent_turns": []
        })
        .to_string();

        // Act
        let output = format_gateway_status(&health, Some(&telemetry));

        // Assert
        assert!(
            output.contains("Metrics\n"),
            "expected Metrics section, got: {output}"
        );
        assert!(
            output.contains("Turns: 42  Errors: 3"),
            "expected Turns/Errors line, got: {output}"
        );
        assert!(
            output.contains("Tokens: 15000 in / 3200 out"),
            "expected Tokens line, got: {output}"
        );
        assert!(
            output.contains("Tool Calls: 28"),
            "expected Tool Calls line, got: {output}"
        );
    }

    #[test]
    fn format_gateway_status_metrics_section_sums_multiple_entries() {
        // Arrange
        let health = serde_json::json!({
            "ok": true,
            "version": "0.1.0",
            "uptime_secs": 100,
            "pid": 1
        })
        .to_string();

        let telemetry = serde_json::json!({
            "metrics": {
                "egopulse_turns_total": [
                    { "labels": {"channel": "discord"}, "value": 10 },
                    { "labels": {"channel": "web"}, "value": 32 }
                ]
            },
            "recent_errors": [],
            "recent_turns": []
        })
        .to_string();

        // Act
        let output = format_gateway_status(&health, Some(&telemetry));

        // Assert
        assert!(
            output.contains("Turns: 42"),
            "expected summed turns, got: {output}"
        );
    }

    #[test]
    fn format_gateway_status_metrics_section_skips_without_values() {
        let health = serde_json::json!({
            "ok": true,
            "version": "0.1.0",
            "uptime_secs": 100,
            "pid": 1
        })
        .to_string();
        let empty_telemetry = serde_json::json!({
            "metrics": {},
            "recent_errors": [],
            "recent_turns": []
        })
        .to_string();

        for telemetry in [Some(empty_telemetry.as_str()), None] {
            let output = format_gateway_status(&health, telemetry);
            assert!(
                !output.contains("Metrics"),
                "should not show Metrics section without values, got: {output}"
            );
        }
    }

    #[test]
    fn format_gateway_status_metrics_partial_only_tokens() {
        // Arrange — only token metrics present, turns/errors/tool_calls absent
        let health = serde_json::json!({
            "ok": true,
            "version": "0.1.0",
            "uptime_secs": 100,
            "pid": 1
        })
        .to_string();

        let telemetry = serde_json::json!({
            "metrics": {
                "egopulse_llm_tokens_total": [
                    { "labels": { "direction": "input" }, "value": 500 },
                    { "labels": { "direction": "output" }, "value": 100 }
                ]
            },
            "recent_errors": [],
            "recent_turns": []
        })
        .to_string();

        // Act
        let output = format_gateway_status(&health, Some(&telemetry));

        // Assert
        assert!(
            output.contains("Metrics\n"),
            "expected Metrics section, got: {output}"
        );
        assert!(
            output.contains("Tokens: 500 in / 100 out"),
            "expected Tokens line, got: {output}"
        );
        assert!(
            !output.contains("Turns:"),
            "should not show Turns when absent, got: {output}"
        );
        assert!(
            !output.contains("Tool Calls:"),
            "should not show Tool Calls when absent, got: {output}"
        );
    }
}

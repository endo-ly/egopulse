//! ステータススナップショットの型定義とファイル I/O。
//!
//! `egopulse run` の起動時に書き出される `status.json` の読み書きと、
//! 人間可読な ASCII フォーマットへの変換を提供する。

use std::fmt;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

const STATUS_FILE: &str = "status.json";

/// ステータススナップショットの全体。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StatusSnapshot {
    pub version: String,
    pub pid: u32,
    pub started_at: String,
    pub config_path: String,
    pub mcp: McpStatus,
    pub channels: ChannelsStatus,
    pub provider: ProviderStatus,
}

/// MCP サーバーの接続結果。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpStatus {
    #[serde(default)]
    pub connected: Vec<ConnectedMcpServer>,
    #[serde(default)]
    pub failed: Vec<FailedMcpServer>,
}

/// 接続成功した MCP サーバー。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConnectedMcpServer {
    pub name: String,
    pub transport: TransportType,
    pub tools: Vec<String>,
}

/// 接続失敗した MCP サーバー。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FailedMcpServer {
    pub name: String,
    pub error: String,
}

/// トランスポート種別。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportType {
    Stdio,
    StreamableHttp,
}

impl fmt::Display for TransportType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio => write!(f, "stdio"),
            Self::StreamableHttp => write!(f, "streamable_http"),
        }
    }
}

/// 各チャネルの起動設定。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChannelsStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web: Option<WebChannelStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discord: Option<ChannelEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telegram: Option<ChannelEntry>,
}

/// チャネルの基本エントリ。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChannelEntry {
    pub enabled: bool,
}

/// Web チャネルの設定。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WebChannelStatus {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// LLM Provider の設定。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderStatus {
    pub default: String,
    pub model: String,
}

/// `status.json` にスナップショットを書き出す。
pub fn write_status(state_root: &Path, snapshot: &StatusSnapshot) -> std::io::Result<()> {
    let runtime_dir = state_root.join("runtime");
    std::fs::create_dir_all(&runtime_dir)?;
    let path = runtime_dir.join(STATUS_FILE);
    let json = serde_json::to_string_pretty(snapshot)?;
    fs::write(path, json)
}

/// `status.json` からスナップショットを読み取る。
///
/// ファイルが存在しない、またはパースに失敗した場合は `None` を返す。
pub fn read_status(state_root: &Path) -> Option<StatusSnapshot> {
    let path = state_root.join("runtime").join(STATUS_FILE);
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// スナップショットを人間可読な ASCII テキストにフォーマットする。
pub fn format_snapshot(snapshot: &StatusSnapshot) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!(
        "EgoPulse v{}  PID {}  started {}",
        snapshot.version, snapshot.pid, snapshot.started_at
    ));
    lines.push(format!("Config: {}", snapshot.config_path));
    lines.push(String::new());
    lines.push(format!(
        "Provider: {} / {}",
        snapshot.provider.default, snapshot.provider.model
    ));
    lines.push(String::new());

    lines.push("Channels".to_string());
    format_channels(&snapshot.channels, &mut lines);

    lines.push(String::new());
    let total = snapshot.mcp.connected.len() + snapshot.mcp.failed.len();
    lines.push(format!(
        "MCP Servers ({}/{} connected)",
        snapshot.mcp.connected.len(),
        total,
    ));
    for server in &snapshot.mcp.connected {
        let tool_count = server.tools.len();
        lines.push(format!(
            "  ✓ {:<20} {:<15} {} tool{}",
            server.name,
            server.transport,
            tool_count,
            if tool_count == 1 { "" } else { "s" },
        ));
    }
    for server in &snapshot.mcp.failed {
        lines.push(format!("  ✗ {:<20} {}", server.name, server.error));
    }

    lines.join("\n")
}

fn format_channels(channels: &ChannelsStatus, lines: &mut Vec<String>) {
    if let Some(web) = &channels.web {
        let addr = match (&web.host, web.port) {
            (Some(host), Some(port)) => format!(" ({}:{})", host, port),
            (Some(host), None) => format!(" ({})", host),
            (None, Some(port)) => format!(" (:{})", port),
            (None, None) => String::new(),
        };
        lines.push(format!(
            "  {:<9}{}{}",
            "web",
            if web.enabled { "enabled" } else { "disabled" },
            addr,
        ));
    }
    if let Some(discord) = &channels.discord {
        lines.push(format!(
            "  {:<9}{}",
            "discord",
            if discord.enabled {
                "enabled"
            } else {
                "disabled"
            },
        ));
    }
    if let Some(telegram) = &channels.telegram {
        lines.push(format!(
            "  {:<9}{}",
            "telegram",
            if telegram.enabled {
                "enabled"
            } else {
                "disabled"
            },
        ));
    }
}

/// `status.json` からスナップショットを読み取り、表示する。
pub fn run_status(json_output: bool) -> Result<(), String> {
    let state_root = crate::config::default_state_root().map_err(|e| e.to_string())?;
    let snapshot = read_status(&state_root).ok_or("EgoPulse has not been started yet")?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&snapshot).unwrap());
    } else {
        print!("{}", format_snapshot(&snapshot));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> StatusSnapshot {
        StatusSnapshot {
            version: "0.1.0".to_string(),
            pid: 293918,
            started_at: "2026-04-12T14:03:58Z".to_string(),
            config_path: "/root/.egopulse/egopulse.config.yaml".to_string(),
            mcp: McpStatus {
                connected: vec![ConnectedMcpServer {
                    name: "context7".to_string(),
                    transport: TransportType::Stdio,
                    tools: vec!["resolve_library_id".to_string(), "query_docs".to_string()],
                }],
                failed: vec![FailedMcpServer {
                    name: "github".to_string(),
                    error: "connection timed out after 30s".to_string(),
                }],
            },
            channels: ChannelsStatus {
                web: Some(WebChannelStatus {
                    enabled: true,
                    host: Some("127.0.0.1".to_string()),
                    port: Some(10961),
                }),
                discord: Some(ChannelEntry { enabled: true }),
                telegram: Some(ChannelEntry { enabled: true }),
            },
            provider: ProviderStatus {
                default: "openrouter".to_string(),
                model: "gpt-5".to_string(),
            },
        }
    }

    #[test]
    fn write_read_roundtrip() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let snapshot = sample_snapshot();

        // Act
        write_status(dir.path(), &snapshot).unwrap();
        let loaded = read_status(dir.path()).unwrap();

        // Assert
        assert_eq!(snapshot, loaded);
    }

    #[test]
    fn read_missing_file_returns_none() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();

        // Act
        let result = read_status(dir.path());

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn read_invalid_json_returns_none() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().join("runtime");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let path = runtime_dir.join(STATUS_FILE);
        fs::write(&path, "not json").unwrap();

        // Act
        let result = read_status(dir.path());

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn format_contains_header() {
        // Arrange
        let snapshot = sample_snapshot();

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("EgoPulse v0.1.0  PID 293918"));
        assert!(output.contains("Config: /root/.egopulse/egopulse.config.yaml"));
    }

    #[test]
    fn format_contains_provider() {
        // Arrange
        let snapshot = sample_snapshot();

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("Provider: openrouter / gpt-5"));
    }

    #[test]
    fn format_contains_channels() {
        // Arrange
        let snapshot = sample_snapshot();

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("Channels"));
        assert!(output.contains("  web      enabled (127.0.0.1:10961)"));
        assert!(output.contains("  discord  enabled"));
        assert!(output.contains("  telegram enabled"));
    }

    #[test]
    fn format_contains_mcp_connected() {
        // Arrange
        let snapshot = sample_snapshot();

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("MCP Servers (1/2 connected)"));
        assert!(output.contains("✓ context7"));
        assert!(output.contains("2 tools"));
    }

    #[test]
    fn format_contains_mcp_failed() {
        // Arrange
        let snapshot = sample_snapshot();

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("✗ github"));
        assert!(output.contains("connection timed out after 30s"));
    }

    #[test]
    fn format_empty_mcp() {
        // Arrange
        let mut snapshot = sample_snapshot();
        snapshot.mcp = McpStatus::default();

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("MCP Servers (0/0 connected)"));
        refute_contains_connected_or_failed(&output);
    }

    #[test]
    fn format_only_connected() {
        // Arrange
        let mut snapshot = sample_snapshot();
        snapshot.mcp.failed.clear();

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("MCP Servers (1/1 connected)"));
        assert!(output.contains("✓ context7"));
        assert!(!output.contains("✗"));
    }

    #[test]
    fn format_only_failed() {
        // Arrange
        let mut snapshot = sample_snapshot();
        snapshot.mcp.connected.clear();

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("MCP Servers (0/1 connected)"));
        assert!(output.contains("✗ github"));
        assert!(!output.contains("✓"));
    }

    #[test]
    fn format_single_tool_singular() {
        // Arrange
        let mut snapshot = sample_snapshot();
        snapshot.mcp.connected = vec![ConnectedMcpServer {
            name: "single".to_string(),
            transport: TransportType::Stdio,
            tools: vec!["one_tool".to_string()],
        }];

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("1 tool"));
        assert!(!output.contains("1 tools"));
    }

    #[test]
    fn format_disabled_channel() {
        // Arrange
        let mut snapshot = sample_snapshot();
        snapshot.channels.web = Some(WebChannelStatus {
            enabled: false,
            host: None,
            port: None,
        });
        snapshot.channels.discord = Some(ChannelEntry { enabled: false });

        // Act
        let output = format_snapshot(&snapshot);

        // Assert
        assert!(output.contains("  web      disabled"));
        assert!(output.contains("  discord  disabled"));
    }

    fn refute_contains_connected_or_failed(output: &str) {
        assert!(
            !output.contains("✓") && !output.contains("✗"),
            "should not contain connected or failed markers"
        );
    }

    #[test]
    fn transport_type_display() {
        assert_eq!(TransportType::Stdio.to_string(), "stdio");
        assert_eq!(TransportType::StreamableHttp.to_string(), "streamable_http");
    }

    #[test]
    fn transport_type_serde_roundtrip() {
        let json = serde_json::to_string(&TransportType::Stdio).unwrap();
        assert_eq!(json, "\"stdio\"");

        let parsed: TransportType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, TransportType::Stdio);
    }
}

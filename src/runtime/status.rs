//! MCP サーバーの接続結果を表す型定義。
//!
//! `McpManager::status_snapshot()` が返す構造体を定義する。

use std::fmt;

use serde::{Deserialize, Serialize};

/// MCP サーバーの接続結果。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct McpStatus {
    #[serde(default)]
    pub connected: Vec<ConnectedMcpServer>,
    #[serde(default)]
    pub failed: Vec<FailedMcpServer>,
}

/// 接続成功した MCP サーバー。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ConnectedMcpServer {
    pub name: String,
    pub transport: TransportType,
    pub tools: Vec<String>,
}

/// 接続失敗した MCP サーバー。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct FailedMcpServer {
    pub name: String,
    pub error: String,
}

/// トランスポート種別。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TransportType {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn mcp_status_default_is_empty() {
        let status = McpStatus::default();
        assert!(status.connected.is_empty());
        assert!(status.failed.is_empty());
    }
}

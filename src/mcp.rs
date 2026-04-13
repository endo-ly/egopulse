//! MCP (Model Context Protocol) クライアント。
//!
//! 外部 MCP server に接続し、tool を動的に LLM に公開する。
//! `mcp.json` / `mcp.d/*.json` から global + workspace の設定をマージし、
//! stdio / streamable_http 両対応で接続する。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use tokio::process::Command as TokioCommand;
use tokio::time::{Duration, timeout};
use tracing::warn;

use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, Tool};
use rmcp::service::{DynService, RoleClient, RunningService, ServiceExt};
use rmcp::transport::{
    StreamableHttpClientTransport, TokioChildProcess,
    streamable_http_client::StreamableHttpClientTransportConfig,
};

use crate::config::default_state_root;
use crate::error::{ConfigError, McpError};
use crate::llm::ToolDefinition;

const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 60;
const DEFAULT_CONNECTION_TIMEOUT_SECS: u64 = 30;
const TOOL_NAME_MAX_LEN: usize = 64;

type DynClient = RunningService<RoleClient, Box<dyn DynService<RoleClient>>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportType {
    Stdio,
    StreamableHttp,
}

#[derive(Clone, Debug, Deserialize)]
pub struct McpServerConfig {
    #[serde(alias = "type")]
    pub transport: TransportType,
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

fn default_request_timeout_secs() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_SECS
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigFile {
    #[serde(default)]
    pub default_protocol_version: Option<String>,
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

pub struct McpManager {
    servers: Vec<ConnectedServer>,
}

struct ConnectedServer {
    name: String,
    config: McpServerConfig,
    client: DynClient,
    tool_name_map: HashMap<String, String>,
    cached_tools: Vec<Tool>,
}

impl ConnectedServer {
    fn client(&self) -> &DynClient {
        &self.client
    }
}

pub fn mcp_config_paths(workspace_dir: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let state_root = default_state_root()?;
    Ok(vec![
        state_root.join("mcp.json"),
        state_root.join("mcp.d"),
        workspace_dir.join("mcp.json"),
        workspace_dir.join("mcp.d"),
    ])
}

pub fn load_and_merge_mcp_configs(
    workspace_dir: &Path,
) -> Result<HashMap<String, McpServerConfig>, ConfigError> {
    let paths = mcp_config_paths(workspace_dir)?;
    let mut merged: HashMap<String, McpServerConfig> = HashMap::new();

    for path in &paths {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                let mut json_files: Vec<PathBuf> = entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
                    .collect();
                json_files.sort();
                for file_path in json_files {
                    let file_config = match read_mcp_config_file(&file_path) {
                        Ok(config) => config,
                        Err(error) => {
                            warn!(path = %file_path.display(), "skipping MCP config: {error}");
                            continue;
                        }
                    };
                    for (name, server_config) in file_config.mcp_servers {
                        merged.insert(name, server_config);
                    }
                }
            }
        } else if path.extension().is_some_and(|ext| ext == "json") && path.exists() {
            let file_config = match read_mcp_config_file(path) {
                Ok(config) => config,
                Err(error) => {
                    warn!(path = %path.display(), "skipping MCP config: {error}");
                    continue;
                }
            };
            for (name, server_config) in file_config.mcp_servers {
                merged.insert(name, server_config);
            }
        }
    }

    Ok(merged)
}

fn read_mcp_config_file(path: &Path) -> Result<McpConfigFile, McpError> {
    let contents = std::fs::read_to_string(path).map_err(|source| McpError::ConfigReadFailed {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&contents).map_err(|detail| McpError::ConfigParseFailed {
        path: path.to_path_buf(),
        detail: detail.to_string(),
    })
}

pub fn sanitize_tool_name(server: &str, tool: &str) -> String {
    let sanitize = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let server_part = sanitize(server);
    let tool_part = sanitize(tool);
    let full = format!("mcp_{server_part}_{tool_part}");
    if full.len() > TOOL_NAME_MAX_LEN {
        let hash = sha2_short(&full);
        format!("mcp_{hash}")
    } else {
        full
    }
}

fn sha2_short(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    result[..4].iter().map(|b| format!("{b:02x}")).collect()
}

impl McpManager {
    pub async fn new(workspace_dir: &Path) -> Result<Self, ConfigError> {
        let configs = load_and_merge_mcp_configs(workspace_dir)?;
        let mut servers = Vec::new();

        for (name, config) in &configs {
            match connect_server(name, config, workspace_dir).await {
                Ok((client, tools)) => {
                    let mut tool_name_map = HashMap::new();
                    let mut filtered_tools = Vec::new();
                    let mut tool_display_names = Vec::new();
                    for t in &tools {
                        let full = sanitize_tool_name(name, t.name.as_ref());
                        if tool_name_map.contains_key(&full) {
                            warn!(
                                server = name,
                                original = %t.name,
                                sanitized = %full,
                                "skipping MCP tool: sanitized name collides with existing tool"
                            );
                            continue;
                        }
                        tool_name_map.insert(full.clone(), t.name.to_string());
                        tool_display_names.push(full);
                        filtered_tools.push(t.clone());
                    }

                    tracing::info!(
                        server = name,
                        tools = ?tool_display_names,
                        "MCP server connected"
                    );
                    servers.push(ConnectedServer {
                        name: name.clone(),
                        config: (*config).clone(),
                        client,
                        tool_name_map,
                        cached_tools: filtered_tools,
                    });
                }
                Err(error) => {
                    warn!(server = name, "MCP server connection failed: {error}");
                }
            }
        }

        tracing::info!(
            connected = servers.len(),
            total = configs.len(),
            "MCP initialization complete"
        );
        Ok(Self { servers })
    }

    pub fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.servers
            .iter()
            .flat_map(|server| {
                server.cached_tools.iter().map(|tool| {
                    let full_name = sanitize_tool_name(&server.name, tool.name.as_ref());
                    ToolDefinition {
                        name: full_name,
                        description: tool.description.clone().unwrap_or_default().to_string(),
                        parameters: serde_json::to_value(&tool.input_schema)
                            .unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
                    }
                })
            })
            .collect()
    }

    /// Execute an MCP tool by name.
    /// Takes pre-extracted server info to avoid holding RwLock across await.
    pub async fn execute_tool(
        &self,
        server_idx: usize,
        original_tool_name: String,
        request_timeout_secs: u64,
        input: serde_json::Value,
    ) -> Result<String, McpError> {
        let server = self
            .servers
            .get(server_idx)
            .ok_or_else(|| McpError::ToolCallFailed {
                server: "unknown".to_string(),
                tool: original_tool_name.clone(),
                detail: "server index not found".to_string(),
            })?;

        let request_timeout = Duration::from_secs(request_timeout_secs);
        let arguments = match input {
            serde_json::Value::Object(map) => map,
            other => {
                return Err(McpError::ToolCallFailed {
                    server: server.name.clone(),
                    tool: original_tool_name.clone(),
                    detail: format!("expected JSON object for arguments, got {}", other),
                });
            }
        };
        let params =
            CallToolRequestParams::new(original_tool_name.clone()).with_arguments(arguments);

        let result = match timeout(request_timeout, server.client.peer().call_tool(params)).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                return Err(McpError::ToolCallFailed {
                    server: server.name.clone(),
                    tool: original_tool_name.clone(),
                    detail: error.to_string(),
                });
            }
            Err(_) => {
                return Err(McpError::ToolCallFailed {
                    server: server.name.clone(),
                    tool: original_tool_name.clone(),
                    detail: format!("timed out after {}s", request_timeout_secs),
                });
            }
        };

        let mut parts: Vec<String> = result
            .content
            .into_iter()
            .map(|content| match content.raw {
                rmcp::model::RawContent::Text(text) => text.text.clone(),
                rmcp::model::RawContent::Image(img) => {
                    format!("[image: {} ({} bytes)]", img.mime_type, img.data.len())
                }
                rmcp::model::RawContent::Resource(res) => {
                    let desc = match &res.resource {
                        rmcp::model::ResourceContents::TextResourceContents {
                            uri,
                            mime_type,
                            ..
                        } => {
                            format!(
                                "resource: {uri} ({})",
                                mime_type.as_deref().unwrap_or("unknown")
                            )
                        }
                        rmcp::model::ResourceContents::BlobResourceContents {
                            uri,
                            mime_type,
                            ..
                        } => {
                            format!(
                                "blob: {uri} ({})",
                                mime_type.as_deref().unwrap_or("unknown")
                            )
                        }
                    };
                    format!("[{desc}]")
                }
                rmcp::model::RawContent::Audio(audio) => {
                    format!("[audio: {} ({} bytes)]", audio.mime_type, audio.data.len())
                }
                rmcp::model::RawContent::ResourceLink(link) => {
                    format!("[resource_link: {} ({})]", link.uri, link.name)
                }
            })
            .collect();

        if let Some(structured) = result.structured_content {
            parts.push(format!(
                "[structured_content: {}]",
                serde_json::to_string(&structured).unwrap_or_default()
            ));
        }

        let output = parts.join("\n");

        Ok(if output.is_empty() {
            "(no output)".to_string()
        } else {
            output
        })
    }

    pub fn is_mcp_tool(&self, name: &str) -> Option<(usize, String, String, u64)> {
        for (i, s) in self.servers.iter().enumerate() {
            if let Some(original_name) = s.tool_name_map.get(name) {
                return Some((
                    i,
                    s.name.clone(),
                    original_name.clone(),
                    s.config.request_timeout_secs,
                ));
            }
        }
        None
    }

    pub fn get_client_by_index(&self, index: usize) -> Option<&DynClient> {
        self.servers.get(index).map(|s| s.client())
    }
}

async fn connect_server(
    name: &str,
    config: &McpServerConfig,
    workspace_dir: &Path,
) -> Result<(DynClient, Vec<Tool>), McpError> {
    match config.transport {
        TransportType::Stdio => connect_stdio(name, config, workspace_dir).await,
        TransportType::StreamableHttp => connect_http(name, config).await,
    }
}

async fn connect_stdio(
    name: &str,
    config: &McpServerConfig,
    workspace_dir: &Path,
) -> Result<(DynClient, Vec<Tool>), McpError> {
    let command_str = config
        .command
        .as_deref()
        .ok_or_else(|| McpError::ConnectionFailed {
            server: name.to_string(),
            detail: "stdio transport requires 'command' field".to_string(),
        })?;

    let mut cmd = TokioCommand::new(command_str);
    cmd.args(&config.args);
    cmd.current_dir(workspace_dir);
    cmd.envs(&config.env);

    // systemd (ProtectHome=read-only) 下でパッケージマネージャが $HOME へ
    // キャッシュを書けず起動に失敗する問題を回避するため writable HOME を注入。
    // ユーザーが mcp.json の env で HOME を明示した場合はそちらを優先。
    let state_root = default_state_root().map_err(|e| McpError::ConnectionFailed {
        server: name.to_string(),
        detail: format!("failed to resolve state root: {e}"),
    })?;
    let mcp_home = state_root.join("mcp-home").join(name);
    std::fs::create_dir_all(&mcp_home).map_err(|e| McpError::ConnectionFailed {
        server: name.to_string(),
        detail: format!("failed to create MCP home: {e}"),
    })?;
    if !config.env.contains_key("HOME") {
        cmd.env("HOME", &mcp_home);
    }

    let child = TokioChildProcess::new(cmd).map_err(|error| McpError::ConnectionFailed {
        server: name.to_string(),
        detail: error.to_string(),
    })?;

    let client_info = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("egopulse", env!("CARGO_PKG_VERSION")),
    );

    let connect_timeout = Duration::from_secs(DEFAULT_CONNECTION_TIMEOUT_SECS);

    let client = timeout(connect_timeout, client_info.into_dyn().serve(child))
        .await
        .map_err(|_| McpError::ConnectionFailed {
            server: name.to_string(),
            detail: format!(
                "connection timed out after {}s",
                DEFAULT_CONNECTION_TIMEOUT_SECS
            ),
        })?
        .map_err(|error| McpError::ConnectionFailed {
            server: name.to_string(),
            detail: error.to_string(),
        })?;

    let tools = timeout(connect_timeout, client.list_all_tools())
        .await
        .map_err(|_| McpError::ToolListFailed {
            server: name.to_string(),
            detail: format!(
                "tool listing timed out after {}s",
                DEFAULT_CONNECTION_TIMEOUT_SECS
            ),
        })?
        .map_err(|error| McpError::ToolListFailed {
            server: name.to_string(),
            detail: error.to_string(),
        })?;

    Ok((client, tools))
}

async fn connect_http(
    name: &str,
    config: &McpServerConfig,
) -> Result<(DynClient, Vec<Tool>), McpError> {
    let endpoint = config
        .endpoint
        .as_deref()
        .ok_or_else(|| McpError::ConnectionFailed {
            server: name.to_string(),
            detail: "streamable_http transport requires 'endpoint' field".to_string(),
        })?;

    let mut transport_config =
        StreamableHttpClientTransportConfig::with_uri(endpoint).reinit_on_expired_session(true);

    for (key, value) in &config.headers {
        if key.eq_ignore_ascii_case("authorization") {
            transport_config = transport_config.auth_header(value);
        } else if let (Ok(header_name), Ok(header_value)) =
            (HeaderName::from_str(key), HeaderValue::from_str(value))
        {
            let mut map: HashMap<HeaderName, HeaderValue> =
                std::mem::take(&mut transport_config.custom_headers);
            map.insert(header_name, header_value);
            transport_config.custom_headers = map;
        } else {
            warn!(
                server = name,
                header = %key,
                "skipping invalid HTTP header in MCP config"
            );
        }
    }

    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let client_info = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("egopulse", env!("CARGO_PKG_VERSION")),
    );

    let connect_timeout = Duration::from_secs(DEFAULT_CONNECTION_TIMEOUT_SECS);

    let client = timeout(connect_timeout, client_info.into_dyn().serve(transport))
        .await
        .map_err(|_| McpError::ConnectionFailed {
            server: name.to_string(),
            detail: format!(
                "connection timed out after {}s",
                DEFAULT_CONNECTION_TIMEOUT_SECS
            ),
        })?
        .map_err(|error| McpError::ConnectionFailed {
            server: name.to_string(),
            detail: error.to_string(),
        })?;

    let tools = timeout(connect_timeout, client.list_all_tools())
        .await
        .map_err(|_| McpError::ToolListFailed {
            server: name.to_string(),
            detail: format!(
                "tool listing timed out after {}s",
                DEFAULT_CONNECTION_TIMEOUT_SECS
            ),
        })?
        .map_err(|error| McpError::ToolListFailed {
            server: name.to_string(),
            detail: error.to_string(),
        })?;

    Ok((client, tools))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvVarGuard;
    use serial_test::serial;

    #[test]
    fn config_paths_include_global_and_workspace() {
        let workspace = Path::new("/tmp/test-workspace");
        let paths = mcp_config_paths(workspace).unwrap();
        assert_eq!(paths.len(), 4);
        assert!(paths[0].ends_with("mcp.json"));
        assert!(paths[1].ends_with("mcp.d"));
        assert_eq!(paths[2], workspace.join("mcp.json"));
        assert_eq!(paths[3], workspace.join("mcp.d"));
    }

    #[test]
    fn sanitize_normalizes_special_chars() {
        assert_eq!(
            sanitize_tool_name("my-server", "read_file"),
            "mcp_my_server_read_file"
        );
        assert_eq!(sanitize_tool_name("db", "query(1)"), "mcp_db_query_1_");
    }

    #[test]
    fn sanitize_truncates_long_names() {
        let long_server = "a".repeat(30);
        let long_tool = "b".repeat(40);
        let result = sanitize_tool_name(&long_server, &long_tool);
        assert!(result.starts_with("mcp_"));
        assert!(result.len() <= 64);
    }

    #[test]
    #[serial]
    fn load_merges_global_and_workspace_configs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().join(".egopulse");
        let workspace = state_root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        std::fs::create_dir_all(state_root.join("mcp.d")).expect("global mcp.d");

        let global_config = r#"{"mcpServers":{"shared":{"transport":"stdio","command":"npx","args":["-y","shared-server"]}}}"#;
        let ws_config = r#"{"mcpServers":{"local":{"transport":"stdio","command":"node","args":["local.js"]},"shared":{"transport":"stdio","command":"npx","args":["-y","override-server"]}}}"#;

        std::fs::write(
            state_root.join("mcp.d").join("01-global.json"),
            global_config,
        )
        .expect("write global");
        std::fs::write(workspace.join("mcp.json"), ws_config).expect("write workspace");

        let _home = EnvVarGuard::set("HOME", dir.path());

        let configs = load_and_merge_mcp_configs(&workspace).expect("load_and_merge_mcp_configs");
        assert_eq!(configs.len(), 2);
        assert!(configs.contains_key("shared"));
        assert!(configs.contains_key("local"));
        assert_eq!(
            configs["shared"].args,
            vec!["-y", "override-server"],
            "workspace config should override global"
        );
    }

    #[test]
    #[serial]
    fn load_handles_missing_files_gracefully() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().join(".egopulse");
        let workspace = state_root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let _home = EnvVarGuard::set("HOME", dir.path());

        let configs = load_and_merge_mcp_configs(&workspace).expect("load_and_merge_mcp_configs");
        assert!(configs.is_empty());
    }

    #[test]
    #[serial]
    fn load_skips_invalid_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().join(".egopulse");
        let workspace = state_root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");

        std::fs::write(state_root.join("mcp.json"), "not valid json {{{{").expect("write bad");

        let valid_config = r#"{"mcpServers":{"good":{"transport":"stdio","command":"node"}}}"#;
        std::fs::write(workspace.join("mcp.json"), valid_config).expect("write good");

        let _home = EnvVarGuard::set("HOME", dir.path());

        let configs = load_and_merge_mcp_configs(&workspace).expect("load_and_merge_mcp_configs");
        assert_eq!(configs.len(), 1);
        assert!(configs.contains_key("good"));
    }

    #[test]
    fn parse_server_config_stdio() {
        let json = r#"{
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-filesystem", "."],
            "request_timeout_secs": 120
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).expect("parse");
        assert!(matches!(config.transport, TransportType::Stdio));
        assert_eq!(config.command.as_deref(), Some("npx"));
        assert_eq!(config.args.len(), 3);
        assert_eq!(config.request_timeout_secs, 120);
    }

    #[test]
    fn parse_server_config_streamable_http() {
        let json = r#"{
            "transport": "streamable_http",
            "endpoint": "http://127.0.0.1:8080/mcp",
            "headers": {"Authorization": "Bearer token123"}
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).expect("parse");
        assert!(matches!(config.transport, TransportType::StreamableHttp));
        assert_eq!(
            config.endpoint.as_deref(),
            Some("http://127.0.0.1:8080/mcp")
        );
        assert_eq!(
            config.headers.get("Authorization").unwrap(),
            "Bearer token123"
        );
    }

    #[test]
    fn parse_full_config_file() {
        let json = r#"{
            "defaultProtocolVersion": "2024-11-05",
            "mcpServers": {
                "filesystem": {
                    "transport": "stdio",
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
                },
                "remote": {
                    "transport": "streamable_http",
                    "endpoint": "http://127.0.0.1:8080/mcp"
                }
            }
        }"#;
        let config: McpConfigFile = serde_json::from_str(json).expect("parse");
        assert_eq!(config.mcp_servers.len(), 2);
        assert!(config.mcp_servers.contains_key("filesystem"));
        assert!(config.mcp_servers.contains_key("remote"));
    }

    #[test]
    fn parse_accepts_type_alias_for_transport() {
        let json = r#"{
            "mcpServers": {
                "context7": {
                    "type": "stdio",
                    "command": "npx",
                    "args": ["-y", "@upstash/context7-mcp"]
                }
            }
        }"#;
        let config: McpConfigFile = serde_json::from_str(json).expect("parse");
        let server = &config.mcp_servers["context7"];
        assert!(matches!(server.transport, TransportType::Stdio));
        assert_eq!(server.command.as_deref(), Some("npx"));
    }

    #[test]
    #[serial]
    fn mcp_home_dir_is_created_under_state_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().join(".egopulse");

        let _home = EnvVarGuard::set("HOME", dir.path());

        let expected_mcp_home = state_root.join("mcp-home").join("test-server");
        assert!(!expected_mcp_home.exists(), "should not exist before");

        std::fs::create_dir_all(&expected_mcp_home).expect("create mcp-home");
        assert!(expected_mcp_home.exists(), "should exist after creation");
    }
}

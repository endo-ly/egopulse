//! MCP (Model Context Protocol) クライアント。
//!
//! 外部 MCP server に接続し、tool を動的に LLM に公開する。
//! `mcp.json` / `mcp.d/*.json` から global + workspace の設定をマージし、
//! stdio / streamable_http 両対応で接続する。

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use tokio::process::Command as TokioCommand;
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientInfo, Implementation,
    RawContent, ResourceContents, Tool,
};
use rmcp::service::{DynService, RoleClient, RunningService, ServiceExt};
use rmcp::transport::{
    StreamableHttpClientTransport, TokioChildProcess,
    streamable_http_client::StreamableHttpClientTransportConfig,
};

use crate::config::default_state_root;
use crate::config::secret_ref::{ResolvedValue, StringOrRef, read_dotenv, resolve_string_or_ref};
use crate::error::{ConfigError, EgoPulseError, McpError};
use crate::llm::ToolDefinition;
use crate::tools::is_plausible_secret_value;

const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 60;
const DEFAULT_CONNECTION_TIMEOUT_SECS: u64 = 30;
const TOOL_NAME_MAX_LEN: usize = 64;

type DynClient = RunningService<RoleClient, Box<dyn DynService<RoleClient>>>;

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

/// Snapshot of MCP server connection state exposed by the runtime health API.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct McpStatus {
    #[serde(default)]
    pub connected: Vec<ConnectedMcpServer>,
    #[serde(default)]
    pub failed: Vec<FailedMcpServer>,
}

/// Connected MCP server information included in [`McpStatus`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ConnectedMcpServer {
    pub name: String,
    pub transport: TransportType,
    pub tools: Vec<String>,
}

/// Failed MCP server information included in [`McpStatus`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct FailedMcpServer {
    pub name: String,
    pub error: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct McpServerConfig {
    #[serde(alias = "type")]
    pub transport: TransportType,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, StringOrRef>,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, StringOrRef>,
}

/// `env` / `headers` の値を解決した結果。
///
/// 参照（`{"source": "env"|"exec", ...}`）の解決値は接続のたびに作り直すため、
/// 設定の `McpServerConfig` とは別に保持する。
#[derive(Clone, Debug, Default)]
pub(crate) struct ResolvedMcpSecrets {
    env: HashMap<String, ResolvedValue>,
    headers: HashMap<String, ResolvedValue>,
}

impl ResolvedMcpSecrets {
    fn resolve(
        config: &McpServerConfig,
        dotenv: &HashMap<String, String>,
    ) -> Result<Self, ConfigError> {
        Ok(Self {
            env: resolve_values(&config.env, dotenv)?,
            headers: resolve_values(&config.headers, dotenv)?,
        })
    }

    /// 解決できない値を飛ばして解決する。接続には使わず、リダクション対象の収集専用。
    fn resolve_lenient(config: &McpServerConfig, dotenv: &HashMap<String, String>) -> Self {
        let lenient = |values: &HashMap<String, StringOrRef>| {
            values
                .iter()
                .filter_map(|(key, value)| {
                    let resolved = resolve_string_or_ref(Some(value.clone()), dotenv).ok()??;
                    Some((key.clone(), resolved))
                })
                .collect()
        };
        Self {
            env: lenient(&config.env),
            headers: lenient(&config.headers),
        }
    }

    /// リダクション対象にする値を `mcp.<server>.<kind>.<key>` 名で返す。
    ///
    /// 参照で宣言された値は秘密として扱い、リテラルは長さとエントロピーが
    /// 十分な場合だけ対象にする。`Authorization: Bearer <token>` は
    /// 値全体に加えて `<token>` 単体も対象にする。
    fn redaction_entries(&self, server: &str) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        for (kind, values) in [("env", &self.env), ("headers", &self.headers)] {
            for (key, resolved) in values {
                let label = format!("mcp.{server}.{kind}.{key}");
                let include = |value: &str| match resolved {
                    ResolvedValue::Literal(_) => is_plausible_secret_value(value),
                    ResolvedValue::EnvRef { .. } | ResolvedValue::ExecRef { .. } => {
                        !value.is_empty()
                    }
                };
                let value = resolved.value();
                if include(value) {
                    entries.push((label.clone(), value.to_string()));
                }
                if let Some(credential) = auth_scheme_credential(value)
                    && include(credential)
                {
                    entries.push((format!("{label}.credential"), credential.to_string()));
                }
            }
        }
        entries
    }
}

fn resolve_values(
    values: &HashMap<String, StringOrRef>,
    dotenv: &HashMap<String, String>,
) -> Result<HashMap<String, ResolvedValue>, ConfigError> {
    let mut resolved = HashMap::with_capacity(values.len());
    for (key, value) in values {
        if let Some(value) = resolve_string_or_ref(Some(value.clone()), dotenv)? {
            resolved.insert(key.clone(), value);
        }
    }
    Ok(resolved)
}

/// `Bearer <token>` のような認証スキーム付きの値から `<token>` を取り出す。
fn auth_scheme_credential(value: &str) -> Option<&str> {
    let (scheme, credential) = value.split_once(char::is_whitespace)?;
    let credential = credential.trim();
    (["bearer", "basic", "token"]
        .iter()
        .any(|known| scheme.eq_ignore_ascii_case(known))
        && !credential.is_empty())
    .then_some(credential)
}

/// 解決済みの MCP `env` / `headers` 値を、ツール出力のリダクションと共有する。
#[derive(Clone, Default)]
pub(crate) struct McpSecrets(Arc<std::sync::RwLock<Vec<(String, String)>>>);

impl McpSecrets {
    pub(crate) fn snapshot(&self) -> Vec<(String, String)> {
        self.0.read().expect("mcp secrets lock").clone()
    }

    pub(super) fn replace(&self, entries: Vec<(String, String)>) {
        *self.0.write().expect("mcp secrets lock") = entries;
    }
}

fn default_request_timeout_secs() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_SECS
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpConfigFile {
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

struct FailedServer {
    name: String,
    config: McpServerConfig,
    error: String,
}

pub(crate) struct McpManager {
    dotenv_path: PathBuf,
    secrets: McpSecrets,
    /// Redaction entries for every server definition in the config files,
    /// connected or not: the files stay readable even while a server is down.
    configured_secrets: Vec<(String, String)>,
    servers: Vec<ConnectedServer>,
    failed_servers: Vec<FailedServer>,
    /// Pre-computed index: sanitized tool name → (server index, original tool name).
    tool_name_index: HashMap<String, (usize, String)>,
}

struct ConnectedServer {
    name: String,
    config: McpServerConfig,
    resolved: ResolvedMcpSecrets,
    client: DynClient,
    cached_tools: Vec<Tool>,
}

pub(crate) fn mcp_config_paths(workspace_dir: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let state_root = default_state_root()?;
    Ok(vec![
        state_root.join("mcp.json"),
        state_root.join("mcp.d"),
        workspace_dir.join("mcp.json"),
        workspace_dir.join("mcp.d"),
    ])
}

/// Every server definition found in the config files in precedence order,
/// including definitions a later file overrides. Collecting into a map lets
/// later definitions (workspace over global) win.
fn load_all_server_configs(
    workspace_dir: &Path,
) -> Result<Vec<(String, McpServerConfig)>, ConfigError> {
    let paths = mcp_config_paths(workspace_dir)?;
    let mut all = Vec::new();

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
                    all.extend(file_config.mcp_servers);
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
            all.extend(file_config.mcp_servers);
        }
    }

    Ok(all)
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

pub(crate) fn sanitize_tool_name(server: &str, tool: &str) -> String {
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
    /// `dotenv_path` は MCP 設定の秘密参照（`{"source": "env", ...}`）の解決元。
    pub(crate) async fn new(
        workspace_dir: &Path,
        dotenv_path: PathBuf,
    ) -> Result<Self, ConfigError> {
        let file_configs = load_all_server_configs(workspace_dir)?;
        let configured_secrets = collect_configured_secrets(&file_configs, &dotenv_path).await;
        let configs: HashMap<String, McpServerConfig> = file_configs.into_iter().collect();
        let mut servers = Vec::new();
        let mut failed_servers = Vec::new();

        for (name, config) in &configs {
            match connect_server(name, config, workspace_dir, &dotenv_path).await {
                Ok((client, tools, resolved)) => {
                    let filtered_tools = filter_tools_for_server(name, &tools);
                    let tool_display_names: Vec<String> = filtered_tools
                        .iter()
                        .map(|tool| sanitize_tool_name(name, tool.name.as_ref()))
                        .collect();

                    tracing::info!(
                        server = name,
                        tools = ?tool_display_names,
                        "MCP server connected"
                    );
                    servers.push(ConnectedServer {
                        name: name.clone(),
                        config: (*config).clone(),
                        resolved,
                        client,
                        cached_tools: filtered_tools,
                    });
                }
                Err(error) => {
                    warn!(server = name, "MCP server connection failed: {error}");
                    failed_servers.push(FailedServer {
                        name: name.clone(),
                        config: (*config).clone(),
                        error: error.to_string(),
                    });
                }
            }
        }

        tracing::info!(
            connected = servers.len(),
            total = configs.len(),
            "MCP initialization complete"
        );
        let tool_name_index = build_tool_name_index(&servers);
        let manager = Self {
            dotenv_path,
            secrets: McpSecrets::default(),
            configured_secrets,
            servers,
            failed_servers,
            tool_name_index,
        };
        manager.publish_secrets();
        Ok(manager)
    }

    /// Handle to the resolved header/env values of connected servers, kept
    /// current across reconnects, for output redaction.
    pub(crate) fn secrets(&self) -> McpSecrets {
        self.secrets.clone()
    }

    fn publish_secrets(&self) {
        let mut entries = self.configured_secrets.clone();
        entries.extend(
            self.servers
                .iter()
                .flat_map(|server| server.resolved.redaction_entries(&server.name)),
        );
        self.secrets.replace(entries);
    }

    /// Closes every connected server concurrently.
    ///
    /// Stdio servers get their stdin closed, a short grace period to exit, and
    /// are then killed together with their process group. Tool lookups return
    /// "unknown tool" afterwards, so call this only once the runtime has
    /// drained its turns.
    pub(crate) async fn shutdown(&mut self) {
        self.tool_name_index.clear();
        self.secrets.replace(Vec::new());
        let closes = self.servers.drain(..).map(|server| async move {
            if let Err(error) = server.client.cancel().await {
                warn!(server = %server.name, %error, "failed to close MCP server");
            }
        });
        futures_util::future::join_all(closes).await;
    }

    pub(crate) fn has_failed_servers(&self) -> bool {
        !self.failed_servers.is_empty()
    }

    fn failed_server_configs(&self) -> Vec<(String, McpServerConfig)> {
        self.failed_servers
            .iter()
            .map(|server| (server.name.clone(), server.config.clone()))
            .collect()
    }

    fn apply_reconnected_server(
        &mut self,
        name: String,
        config: McpServerConfig,
        resolved: ResolvedMcpSecrets,
        client: DynClient,
        tools: Vec<Tool>,
    ) -> bool {
        if self.servers.iter().any(|connected| connected.name == name) {
            return false;
        }
        let Some(failed_index) = self
            .failed_servers
            .iter()
            .position(|failed| failed.name == name)
        else {
            return false;
        };

        let filtered_tools = filter_tools_for_server(&name, &tools);
        let tool_display_names: Vec<String> = filtered_tools
            .iter()
            .map(|tool| sanitize_tool_name(&name, tool.name.as_ref()))
            .collect();

        info!(
            server = %name,
            tools = ?tool_display_names,
            "MCP server reconnected"
        );
        self.failed_servers.remove(failed_index);
        self.servers.push(ConnectedServer {
            name,
            config,
            resolved,
            client,
            cached_tools: filtered_tools,
        });
        self.tool_name_index = build_tool_name_index(&self.servers);
        self.publish_secrets();
        true
    }

    fn update_failed_server_error(&mut self, name: &str, error: String) {
        if let Some(server) = self
            .failed_servers
            .iter_mut()
            .find(|server| server.name == name)
        {
            server.error = error;
        }
    }

    /// 現在の接続状態をスナップショットとして返す。
    pub(crate) fn status_snapshot(&self) -> McpStatus {
        let mut connected: Vec<ConnectedMcpServer> = self
            .servers
            .iter()
            .map(|server| {
                let tools: Vec<String> = server
                    .cached_tools
                    .iter()
                    .map(|t| t.name.as_ref().to_string())
                    .collect();
                ConnectedMcpServer {
                    name: server.name.clone(),
                    transport: server.config.transport,
                    tools,
                }
            })
            .collect();

        let mut failed: Vec<FailedMcpServer> = self
            .failed_servers
            .iter()
            .map(|server| FailedMcpServer {
                name: server.name.clone(),
                error: server.error.clone(),
            })
            .collect();

        connected.sort_by(|a, b| a.name.cmp(&b.name));
        failed.sort_by(|a, b| a.name.cmp(&b.name));

        McpStatus { connected, failed }
    }

    pub(crate) fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
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

    /// Returns `true` if the MCP tool's annotations indicate it is read-only.
    ///
    /// Returns `false` if the tool is not found, has no annotations, or
    /// `read_only_hint` is not explicitly `true`.
    pub(crate) fn is_tool_read_only(&self, sanitized_name: &str) -> bool {
        let Some((server_idx, original_name)) = self.tool_name_index.get(sanitized_name) else {
            return false;
        };
        let Some(server) = self.servers.get(*server_idx) else {
            return false;
        };
        server
            .cached_tools
            .iter()
            .find(|t| t.name.as_ref() == original_name)
            .and_then(|t| t.annotations.as_ref())
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false)
    }

    pub(crate) async fn execute_tool_by_name(
        &self,
        sanitized_name: &str,
        input: serde_json::Value,
    ) -> Option<Result<String, McpError>> {
        let (server_idx, original_name) = self.tool_name_index.get(sanitized_name)?;
        let server = self.servers.get(*server_idx)?;
        Some(
            self.execute_tool(
                *server_idx,
                original_name.clone(),
                server.config.request_timeout_secs,
                input,
            )
            .await,
        )
    }

    /// Execute an MCP tool by name.
    /// Takes pre-extracted server info to avoid holding RwLock across await.
    pub(crate) async fn execute_tool(
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

        let output = format_mcp_tool_result(result);
        Ok(truncate_to_max_bytes(output))
    }
}

/// Runs the background retry loop for MCP servers that failed during startup.
pub(crate) async fn run_reconnect_loop(
    mcp_manager: Arc<tokio::sync::RwLock<McpManager>>,
    workspace_dir: PathBuf,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), EgoPulseError> {
    const INITIAL_RETRY_SECS: u64 = 5;
    const MAX_RETRY_SECS: u64 = 300;

    let mut retry_secs = INITIAL_RETRY_SECS;
    loop {
        let has_failed_servers = {
            let guard = mcp_manager.read().await;
            guard.has_failed_servers()
        };

        let sleep_secs = if has_failed_servers {
            retry_secs
        } else {
            MAX_RETRY_SECS
        };

        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {}
        }

        if !has_failed_servers {
            retry_secs = INITIAL_RETRY_SECS;
            continue;
        }

        let (reconnect_targets, dotenv_path) = {
            let guard = mcp_manager.read().await;
            (guard.failed_server_configs(), guard.dotenv_path.clone())
        };
        if reconnect_targets.is_empty() {
            retry_secs = INITIAL_RETRY_SECS;
            continue;
        }

        let mut reconnected = 0;
        for (name, config) in reconnect_targets {
            let connection_result = tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                result = connect_server(&name, &config, &workspace_dir, &dotenv_path) => result,
            };
            match connection_result {
                Ok((client, tools, resolved)) => {
                    let applied = {
                        let mut guard = mcp_manager.write().await;
                        guard.apply_reconnected_server(name, config, resolved, client, tools)
                    };
                    reconnected += usize::from(applied);
                }
                Err(error) => {
                    let error_text = error.to_string();
                    warn!(server = %name, "MCP server reconnect failed: {error_text}");
                    let mut guard = mcp_manager.write().await;
                    guard.update_failed_server_error(&name, error_text);
                }
            }
        }

        if reconnected > 0 {
            retry_secs = INITIAL_RETRY_SECS;
        } else {
            retry_secs = (retry_secs * 2).min(MAX_RETRY_SECS);
        }
    }
}

fn truncate_to_max_bytes(output: String) -> String {
    let max = super::DEFAULT_MAX_BYTES;
    if output.len() <= max {
        return output;
    }
    let mut cut = max;
    while cut > 0 && !output.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n... (output truncated to {max} bytes)", &output[..cut])
}

fn format_mcp_tool_result(result: CallToolResult) -> String {
    let mut parts: Vec<String> = result
        .content
        .into_iter()
        .map(|content| format_mcp_content(content.raw))
        .collect();

    if let Some(structured) = result.structured_content {
        parts.push(format!(
            "[structured_content: {}]",
            serde_json::to_string(&structured).unwrap_or_default()
        ));
    }

    let output = parts.join("\n");
    if output.is_empty() {
        if result.is_error == Some(true) {
            "[error]".to_string()
        } else {
            "(no output)".to_string()
        }
    } else {
        output
    }
}

fn format_mcp_content(raw: RawContent) -> String {
    match raw {
        RawContent::Text(text) => text.text,
        RawContent::Image(image) => {
            format!("[image: {} ({} bytes)]", image.mime_type, image.data.len())
        }
        RawContent::Resource(resource) => {
            let description = match resource.resource {
                ResourceContents::TextResourceContents { uri, mime_type, .. } => {
                    format!(
                        "resource: {uri} ({})",
                        mime_type.as_deref().unwrap_or("unknown")
                    )
                }
                ResourceContents::BlobResourceContents { uri, mime_type, .. } => {
                    format!(
                        "blob: {uri} ({})",
                        mime_type.as_deref().unwrap_or("unknown")
                    )
                }
            };
            format!("[{description}]")
        }
        RawContent::Audio(audio) => {
            format!("[audio: {} ({} bytes)]", audio.mime_type, audio.data.len())
        }
        RawContent::ResourceLink(link) => {
            format!("[resource_link: {} ({})]", link.uri, link.name)
        }
    }
}

fn build_tool_name_index(servers: &[ConnectedServer]) -> HashMap<String, (usize, String)> {
    let mut index = HashMap::new();
    for (server_idx, server) in servers.iter().enumerate() {
        for tool in &server.cached_tools {
            let sanitized = sanitize_tool_name(&server.name, tool.name.as_ref());
            index
                .entry(sanitized)
                .or_insert((server_idx, tool.name.to_string()));
        }
    }
    index
}

fn filter_tools_for_server(server_name: &str, tools: &[Tool]) -> Vec<Tool> {
    let mut seen_names = std::collections::HashSet::new();
    let mut filtered_tools = Vec::new();

    for tool in tools {
        let full = sanitize_tool_name(server_name, tool.name.as_ref());
        if !seen_names.insert(full.clone()) {
            warn!(
                server = server_name,
                original = %tool.name,
                sanitized = %full,
                "skipping MCP tool: sanitized name collides with existing tool"
            );
            continue;
        }
        filtered_tools.push(tool.clone());
    }

    filtered_tools
}

/// 秘密参照を解決してから接続する。参照が解決できないサーバーは接続せず、
/// 平文へのフォールバックもしない。
async fn connect_server(
    name: &str,
    config: &McpServerConfig,
    workspace_dir: &Path,
    dotenv_path: &Path,
) -> Result<(DynClient, Vec<Tool>, ResolvedMcpSecrets), McpError> {
    let resolved = resolve_server_secrets(name, config, dotenv_path).await?;
    let (client, tools) = match config.transport {
        TransportType::Stdio => connect_stdio(name, config, &resolved, workspace_dir).await?,
        TransportType::StreamableHttp => connect_http(name, config, &resolved).await?,
    };
    Ok((client, tools, resolved))
}

/// 設定ファイルにある全 server 定義（上書きされたものを含む）の秘密値を集める。
async fn collect_configured_secrets(
    file_configs: &[(String, McpServerConfig)],
    dotenv_path: &Path,
) -> Vec<(String, String)> {
    let file_configs = file_configs.to_vec();
    let dotenv_path = dotenv_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let dotenv = read_dotenv(&dotenv_path);
        file_configs
            .iter()
            .flat_map(|(name, config)| {
                ResolvedMcpSecrets::resolve_lenient(config, &dotenv).redaction_entries(name)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// `exec` 参照はコマンドを同期実行するため、ランタイムを塞がないよう別スレッドで解決する。
async fn resolve_server_secrets(
    name: &str,
    config: &McpServerConfig,
    dotenv_path: &Path,
) -> Result<ResolvedMcpSecrets, McpError> {
    let config = config.clone();
    let dotenv_path = dotenv_path.to_path_buf();
    let to_error = |detail: String| McpError::ConnectionFailed {
        server: name.to_string(),
        detail,
    };
    tokio::task::spawn_blocking(move || {
        ResolvedMcpSecrets::resolve(&config, &read_dotenv(&dotenv_path))
    })
    .await
    .map_err(|error| to_error(format!("secret resolution task failed: {error}")))?
    .map_err(|error| to_error(format!("secret resolution failed: {error}")))
}

/// Spawns a stdio MCP server as the leader of its own process group.
///
/// Launchers such as `npx` or `uv run` start the actual server as a grandchild.
/// Running the server in a dedicated group lets transport shutdown kill the
/// whole tree instead of only the launcher.
fn spawn_stdio_transport(command: TokioCommand) -> std::io::Result<StdioTransport> {
    let mut command = process_wrap::tokio::CommandWrap::from(command);
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    let inner = TokioChildProcess::new(command)?;
    let pgid = inner.id().expect("newly spawned process has a PID");
    Ok(StdioTransport {
        inner,
        pgid: Some(pgid),
    })
}

/// Keeps the dedicated group alive as a shutdown responsibility even after
/// rmcp has reaped its launcher. Orphaned descendants are not waitable children
/// of this process, so process-wrap's wait alone cannot account for them.
struct StdioTransport {
    inner: TokioChildProcess,
    pgid: Option<u32>,
}

impl StdioTransport {
    async fn graceful_shutdown(&mut self) -> std::io::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let result = self.inner.graceful_shutdown().await;
        if let Some(pgid) = self.pgid {
            // The descendants also receive EOF on stdin. Give them the same
            // grace deadline as the launcher before stopping any survivors.
            while process_group_exists(pgid) && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if process_group_exists(pgid) {
                signal_process_group(pgid, libc::SIGKILL)?;
            }
            self.pgid = None;
        }
        result
    }
}

fn process_group_exists(pgid: u32) -> bool {
    // SAFETY: positive pgid is the leader PID of our dedicated process group;
    // signal 0 checks existence without changing process state.
    unsafe { libc::kill(-(pgid as i32), 0) == 0 }
}

fn signal_process_group(pgid: u32, signal: i32) -> std::io::Result<()> {
    // SAFETY: only a dedicated process group created by spawn_stdio_transport
    // is addressed, never the runtime's own group.
    if unsafe { libc::kill(-(pgid as i32), signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            let _ = signal_process_group(pgid, libc::SIGKILL);
        }
    }
}

impl rmcp::transport::Transport<RoleClient> for StdioTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<RoleClient>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    fn receive(
        &mut self,
    ) -> impl std::future::Future<Output = Option<rmcp::service::RxJsonRpcMessage<RoleClient>>> + Send
    {
        self.inner.receive()
    }

    fn close(&mut self) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        self.graceful_shutdown()
    }
}

async fn connect_stdio(
    name: &str,
    config: &McpServerConfig,
    resolved: &ResolvedMcpSecrets,
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
    cmd.envs(resolved.env.iter().map(|(key, value)| (key, value.value())));

    let child = spawn_stdio_transport(cmd).map_err(|error| McpError::ConnectionFailed {
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
    resolved: &ResolvedMcpSecrets,
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

    for (key, value) in &resolved.headers {
        let value = value.value();
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
    use crate::config::secret_ref::SecretSource;
    use crate::test_env::EnvVarGuard;
    use serial_test::serial;

    /// Returns false once the process is gone or only a zombie remains.
    #[cfg(target_os = "linux")]
    fn process_running(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .and_then(|(_, rest)| rest.chars().next())
            })
            .is_some_and(|state| state != 'Z')
    }

    #[cfg(target_os = "linux")]
    struct TestProcessGroup(u32);

    #[cfg(target_os = "linux")]
    impl Drop for TestProcessGroup {
        fn drop(&mut self) {
            // SAFETY: this PID names a dedicated group created by this test.
            unsafe {
                libc::kill(-(self.0 as i32), libc::SIGKILL);
            }
        }
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn stdio_transport_shutdown_kills_child_after_launcher_exits() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("child.pid");
        let mut command = TokioCommand::new("sh");
        command
            .arg("-c")
            .arg("sleep 60 & echo $! > \"$1\"; exit 0")
            .arg("sh")
            .arg(&pid_file);
        let mut transport = spawn_stdio_transport(command).expect("transport");
        let parent = transport.inner.id().expect("parent pid");
        let _cleanup = TestProcessGroup(parent);
        let child = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&pid_file) {
                    if let Ok(pid) = text.trim().parse::<u32>() {
                        if !process_running(parent) {
                            break pid;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("launcher must exit first");
        assert!(process_running(child));

        // Act
        timeout(Duration::from_secs(5), transport.graceful_shutdown())
            .await
            .expect("shutdown must not hang")
            .expect("shutdown");

        // Assert
        timeout(Duration::from_secs(2), async {
            while process_running(child) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("child must not outlive transport");
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn manager_cancels_multiple_stdio_servers_concurrently_and_preserves_graceful_exit() {
        // Arrange: real MCP handshakes, including a launcher which leaves a
        // live server behind, an unresponsive tree and an EOF-aware server.
        let dir = tempfile::tempdir().unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_server.py");
        let mut manager = McpManager {
            dotenv_path: PathBuf::new(),
            secrets: McpSecrets::default(),
            configured_secrets: Vec::new(),
            servers: Vec::new(),
            failed_servers: Vec::new(),
            tool_name_index: HashMap::new(),
        };
        let mut cleanups = Vec::new();
        let mut pids = Vec::new();
        for mode in ["early", "waiting", "graceful"] {
            let config: McpServerConfig = serde_json::from_value(serde_json::json!({
                "transport": "stdio", "command": "python3", "args": [fixture, dir.path(), mode]
            }))
            .unwrap();
            // Install failure cleanup before connecting (including handshake failures).
            let resolved = ResolvedMcpSecrets::default();
            let connecting = connect_stdio(mode, &config, &resolved, dir.path());
            let (connected, launcher) = tokio::join!(connecting, async {
                timeout(Duration::from_secs(2), async {
                    loop {
                        if let Ok(text) =
                            std::fs::read_to_string(dir.path().join(format!("{mode}.launcher")))
                        {
                            break TestProcessGroup(text.trim().parse().unwrap());
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap()
            });
            cleanups.push(launcher);
            let (client, tools) = connected.unwrap();
            let pid: u32 = std::fs::read_to_string(dir.path().join(format!("{mode}.pid")))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(process_running(pid));
            pids.push(pid);
            manager.servers.push(ConnectedServer {
                name: mode.to_string(),
                config,
                resolved: ResolvedMcpSecrets::default(),
                client,
                cached_tools: tools,
            });
        }

        // Act: two stubborn groups each require 3s; sequential close exceeds 5s.
        timeout(Duration::from_secs(5), manager.shutdown())
            .await
            .expect("parallel shutdown must be bounded");

        // Assert
        timeout(Duration::from_secs(2), async {
            while pids.iter().any(|pid| process_running(*pid)) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("all servers must stop");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("graceful.closed")).unwrap(),
            "closed"
        );
        assert!(manager.servers.is_empty());
        assert!(manager.tool_name_index.is_empty());
        assert!(cleanups.iter().all(|group| !process_running(group.0)));
    }

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

        let configs = load_all_server_configs(&workspace)
            .expect("load_all_server_configs")
            .into_iter()
            .collect::<HashMap<_, _>>();
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

        let configs = load_all_server_configs(&workspace)
            .expect("load_all_server_configs")
            .into_iter()
            .collect::<HashMap<_, _>>();
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

        let configs = load_all_server_configs(&workspace)
            .expect("load_all_server_configs")
            .into_iter()
            .collect::<HashMap<_, _>>();
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
            config.headers.get("Authorization"),
            Some(&StringOrRef::Literal("Bearer token123".to_string()))
        );
    }

    fn server_config(value: serde_json::Value) -> McpServerConfig {
        serde_json::from_value(value).expect("server config")
    }

    fn dotenv(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn parse_headers_and_env_accept_secret_refs() {
        // Arrange
        let config = server_config(serde_json::json!({
            "transport": "streamable_http",
            "endpoint": "http://127.0.0.1:8080/mcp",
            "headers": {"Authorization": {"source": "env", "id": "EGOGRAPH_AUTH"}},
            "env": {"TOKEN": {"source": "exec", "command": "echo x"}, "MODE": "plain"}
        }));

        // Assert
        assert!(matches!(
            config.headers["Authorization"],
            StringOrRef::Ref(SecretSource::Env { .. })
        ));
        assert!(matches!(
            config.env["TOKEN"],
            StringOrRef::Ref(SecretSource::Exec { .. })
        ));
        assert_eq!(config.env["MODE"], StringOrRef::Literal("plain".into()));
    }

    #[test]
    fn resolve_reads_env_refs_from_dotenv() {
        // Arrange
        let config = server_config(serde_json::json!({
            "transport": "streamable_http",
            "endpoint": "http://127.0.0.1:8080/mcp",
            "headers": {"Authorization": {"source": "env", "id": "MCP_TEST_AUTH_FROM_DOTENV"}}
        }));
        let dotenv = dotenv(&[("MCP_TEST_AUTH_FROM_DOTENV", "Bearer abc123XYZ")]);

        // Act
        let resolved = ResolvedMcpSecrets::resolve(&config, &dotenv).expect("resolve");

        // Assert
        assert_eq!(
            resolved.headers["Authorization"].value(),
            "Bearer abc123XYZ"
        );
    }

    #[test]
    fn resolve_fails_when_env_ref_is_missing() {
        let config = server_config(serde_json::json!({
            "transport": "streamable_http",
            "endpoint": "http://127.0.0.1:8080/mcp",
            "headers": {"Authorization": {"source": "env", "id": "MCP_TEST_UNSET_REFERENCE"}}
        }));

        let error = ResolvedMcpSecrets::resolve(&config, &HashMap::new()).expect_err("unresolved");

        assert!(error.to_string().contains("MCP_TEST_UNSET_REFERENCE"));
    }

    #[tokio::test]
    async fn connect_server_does_not_connect_when_a_secret_ref_is_unresolved() {
        // Arrange: nothing listens on the endpoint; a connection attempt would
        // time out instead of failing on the unresolved reference.
        let dir = tempfile::tempdir().unwrap();
        let config = server_config(serde_json::json!({
            "transport": "streamable_http",
            "endpoint": "http://127.0.0.1:9/mcp",
            "headers": {"Authorization": {"source": "env", "id": "MCP_TEST_UNSET_REFERENCE"}}
        }));

        // Act
        let result = timeout(
            Duration::from_secs(5),
            connect_server("remote", &config, dir.path(), &dir.path().join(".env")),
        )
        .await
        .expect("must fail fast");

        // Assert
        let error = result.err().expect("unresolved ref").to_string();
        assert!(error.contains("secret resolution failed"), "{error}");
    }

    #[tokio::test]
    #[serial]
    async fn manager_redacts_secrets_of_servers_that_failed_or_were_overridden() {
        // Arrange: the global definition is overridden by the workspace one and
        // the surviving server cannot start; both config files stay readable.
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().join(".egopulse");
        let workspace = state_root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        std::fs::write(
            state_root.join("mcp.json"),
            r#"{"mcpServers":{"remote":{"transport":"stdio","command":"/nonexistent/mcp","env":{"GLOBAL_KEY":"Zk3pQ9vLm2XrT8wYb4Nc"}}}}"#,
        )
        .expect("write global");
        std::fs::write(
            workspace.join("mcp.json"),
            r#"{"mcpServers":{"remote":{"transport":"stdio","command":"/nonexistent/mcp","env":{"WS_KEY":{"source":"env","id":"MCP_TEST_WS_KEY"}}}}}"#,
        )
        .expect("write workspace");
        let dotenv_path = state_root.join(".env");
        std::fs::write(&dotenv_path, "MCP_TEST_WS_KEY=short\n").expect("write dotenv");
        let _home = EnvVarGuard::set("HOME", dir.path());

        // Act
        let manager = McpManager::new(&workspace, dotenv_path)
            .await
            .expect("manager");

        // Assert
        assert!(manager.has_failed_servers());
        let mut secrets = manager.secrets().snapshot();
        secrets.sort();
        assert_eq!(
            secrets,
            vec![
                (
                    "mcp.remote.env.GLOBAL_KEY".to_string(),
                    "Zk3pQ9vLm2XrT8wYb4Nc".to_string()
                ),
                ("mcp.remote.env.WS_KEY".to_string(), "short".to_string()),
            ]
        );
    }

    #[test]
    fn redaction_entries_trust_refs_and_filter_literals() {
        // Arrange
        let config = server_config(serde_json::json!({
            "transport": "streamable_http",
            "endpoint": "http://127.0.0.1:8080/mcp",
            "headers": {
                "Authorization": {"source": "env", "id": "MCP_TEST_AUTH"},
                "X-Short-Ref": {"source": "env", "id": "MCP_TEST_SHORT"},
                "X-Literal-Secret": "Zk3pQ9vLm2XrT8wYb4Nc",
                "X-Client": "web"
            }
        }));
        let dotenv = dotenv(&[
            ("MCP_TEST_AUTH", "Bearer Qw8eRt5yUi2oPa7sDf3g"),
            ("MCP_TEST_SHORT", "abc"),
        ]);
        let resolved = ResolvedMcpSecrets::resolve(&config, &dotenv).expect("resolve");

        // Act
        let mut entries = resolved.redaction_entries("remote");
        entries.sort();

        // Assert
        assert_eq!(
            entries,
            vec![
                (
                    "mcp.remote.headers.Authorization".to_string(),
                    "Bearer Qw8eRt5yUi2oPa7sDf3g".to_string()
                ),
                (
                    "mcp.remote.headers.Authorization.credential".to_string(),
                    "Qw8eRt5yUi2oPa7sDf3g".to_string()
                ),
                (
                    "mcp.remote.headers.X-Literal-Secret".to_string(),
                    "Zk3pQ9vLm2XrT8wYb4Nc".to_string()
                ),
                (
                    "mcp.remote.headers.X-Short-Ref".to_string(),
                    "abc".to_string()
                ),
            ]
        );
    }

    #[test]
    fn parse_full_config_file() {
        let json = r#"{
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
    fn tool_annotations_read_only_hint_is_checked() {
        use rmcp::model::{Tool, ToolAnnotations};
        use std::sync::Arc;

        let schema: Arc<serde_json::Map<String, serde_json::Value>> = Arc::new(
            serde_json::json!({"type": "object"})
                .as_object()
                .expect("object")
                .clone(),
        );

        let read_only_tool = Tool::new("search", "search docs", schema.clone())
            .annotate(ToolAnnotations::new().read_only(true));
        assert_eq!(
            read_only_tool
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint),
            Some(true)
        );

        let write_tool = Tool::new("write", "write data", schema.clone())
            .annotate(ToolAnnotations::new().read_only(false));
        assert_eq!(
            write_tool
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint),
            Some(false)
        );

        let no_annotations = Tool::new("bare", "bare tool", schema);
        assert_eq!(
            no_annotations
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint),
            None
        );
    }

    #[test]
    fn truncate_to_max_bytes_preserves_output_under_limit() {
        let output = "hello world".to_string();
        assert_eq!(truncate_to_max_bytes(output.clone()), output);
    }

    #[test]
    fn truncate_to_max_bytes_preserves_output_at_exact_boundary() {
        let max = super::super::DEFAULT_MAX_BYTES;
        let exactly_at_limit = "a".repeat(max);
        assert_eq!(
            truncate_to_max_bytes(exactly_at_limit.clone()),
            exactly_at_limit
        );
    }

    #[test]
    fn truncate_to_max_bytes_truncates_output_over_limit() {
        let max = super::super::DEFAULT_MAX_BYTES;
        let large = "a".repeat(max + 1000);
        let result = truncate_to_max_bytes(large);
        let notice = format!("\n... (output truncated to {max} bytes)");
        assert!(result.ends_with(&notice));
        let content = &result[..result.len() - notice.len()];
        assert!(content.len() <= max);
    }

    #[test]
    fn truncate_to_max_bytes_preserves_utf8_boundaries() {
        let large = "あ".repeat(super::super::DEFAULT_MAX_BYTES / "あ".len() + 1);
        let result = truncate_to_max_bytes(large);
        assert!(result.contains("... (output truncated to"));
        let first_line = result.lines().next().expect("content before notice");
        assert!(first_line.chars().all(|c| c == 'あ'));
    }
}

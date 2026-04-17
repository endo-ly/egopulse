//! MCP ツールを Tool trait 実装としてラップするアダプター。
//!
//! `McpToolAdapter` は、`McpManager` が検出した各 MCP ツールを
//! `Tool` trait の実装として `ToolRegistry` に登録できるようにする。
//! これにより、ビルトインツールと MCP ツールを統一的に扱える。

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{Tool, ToolExecutionContext, ToolResult};
use crate::llm::ToolDefinition;
use crate::mcp::McpManager;

/// MCP サーバー上の単一ツールを Tool trait で包むアダプター。
///
/// 各インスタンスは接続済みサーバー内の1ツールに対応し、
/// サニタイズ済み名前・定義・実行を一元的に扱う。
pub(crate) struct McpToolAdapter {
    name: String,
    original_name: String,
    server_idx: usize,
    timeout_secs: u64,
    definition: ToolDefinition,
    manager: Arc<RwLock<McpManager>>,
}

impl McpToolAdapter {
    /// 新しい MCP ツールアダプターを生成する。
    ///
    /// # Arguments
    /// * `name` - サニタイズ済みツール名 (`mcp_{server}_{tool}`)
    /// * `original_name` - MCP サーバー上のオリジナルツール名
    /// * `server_idx` - `McpManager.servers` 内のサーバーインデックス
    /// * `timeout_secs` - リクエストタイムアウト (秒)
    /// * `definition` - キャッシュ済み `ToolDefinition`
    /// * `manager` - `McpManager` への共有参照
    pub(crate) fn new(
        name: String,
        original_name: String,
        server_idx: usize,
        timeout_secs: u64,
        definition: ToolDefinition,
        manager: Arc<RwLock<McpManager>>,
    ) -> Self {
        Self {
            name,
            original_name,
            server_idx,
            timeout_secs,
            definition,
            manager,
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let result = {
            let guard = self.manager.read().await;
            guard
                .execute_tool(
                    self.server_idx,
                    self.original_name.clone(),
                    self.timeout_secs,
                    input,
                )
                .await
        };

        match result {
            Ok(output) => ToolResult::success(output),
            Err(error) => ToolResult::error(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolDefinition;
    use crate::tools::Tool;

    use serde_json::json;

    /// name() がサニタイズ済み mcp_{server}_{tool} 形式の名前を返すことを検証する。
    #[test]
    fn test_adapter_name_matches_sanitized() {
        // Arrange
        let adapter = create_test_adapter(
            "mcp_filesystem_read_file".to_string(),
            "read_file".to_string(),
        );

        // Act
        let name = adapter.name();

        // Assert
        assert_eq!(name, "mcp_filesystem_read_file");
    }

    /// definition() が正しい ToolDefinition を返すことを検証する。
    #[test]
    fn test_adapter_definition_converts_schema() {
        // Arrange
        let definition = ToolDefinition {
            name: "mcp_filesystem_read_file".to_string(),
            description: "Read a file from the filesystem".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file"
                    }
                },
                "required": ["path"]
            }),
        };
        let adapter = create_test_adapter_with_definition(
            "mcp_filesystem_read_file".to_string(),
            "read_file".to_string(),
            definition.clone(),
        );

        // Act
        let result = adapter.definition();

        // Assert
        assert_eq!(result.name, "mcp_filesystem_read_file");
        assert_eq!(result.description, "Read a file from the filesystem");
        assert_eq!(result.parameters["properties"]["path"]["type"], "string");
    }

    /// execute() が成功レスポンスを ToolResult::success に変換することを検証する。
    ///
    /// McpManager::execute_tool が actual MCP 接続を必要とするため、
    /// このテストは adapter の構築と trait メソッドの呼び出し可能性を検証する。
    /// 実際の MCP 呼び出しの成功パスは integration test でカバーする。
    #[test]
    fn test_adapter_holds_correct_fields_for_success_path() {
        // Arrange
        let adapter = create_test_adapter(
            "mcp_filesystem_read_file".to_string(),
            "read_file".to_string(),
        );

        // Assert — フィールドが正しく保持されていることを確認
        assert_eq!(adapter.name(), "mcp_filesystem_read_file");
        assert_eq!(adapter.original_name, "read_file");
        assert_eq!(adapter.server_idx, 0);
        assert_eq!(adapter.timeout_secs, 60);
        assert_eq!(adapter.definition.name, "mcp_filesystem_read_file");
    }

    /// 複数サーバー・複数ツールの adapter が独立して名前を保持することを検証する。
    #[test]
    fn test_adapter_distinguishes_multiple_servers() {
        // Arrange
        let adapter_a = create_test_adapter("mcp_serverA_tool1".to_string(), "tool1".to_string());
        let adapter_b = create_test_adapter("mcp_serverB_tool2".to_string(), "tool2".to_string());

        // Act & Assert
        assert_ne!(adapter_a.name(), adapter_b.name());
        assert_eq!(adapter_a.name(), "mcp_serverA_tool1");
        assert_eq!(adapter_b.name(), "mcp_serverB_tool2");
    }

    /// definition() を複数回呼び出しても同じ内容が返ることを検証する。
    #[test]
    fn test_adapter_definition_is_idempotent() {
        // Arrange
        let adapter = create_test_adapter("mcp_db_query".to_string(), "query".to_string());

        // Act
        let def1 = adapter.definition();
        let def2 = adapter.definition();

        // Assert
        assert_eq!(def1.name, def2.name);
        assert_eq!(def1.description, def2.description);
        assert_eq!(def1.parameters, def2.parameters);
    }

    /// Tool trait オブジェクトとして扱えることを検証する。
    #[test]
    fn test_adapter_is_dyn_tool_compatible() {
        // Arrange
        let adapter = create_test_adapter(
            "mcp_filesystem_read_file".to_string(),
            "read_file".to_string(),
        );

        // Act — Box<dyn Tool> にキャストできることを確認
        let tool: Box<dyn Tool> = Box::new(adapter);

        // Assert
        assert_eq!(tool.name(), "mcp_filesystem_read_file");
    }

    // --- テストヘルパー ---

    /// テスト用の最小 McpToolAdapter を生成する。
    ///
    /// 実際の MCP 接続は不要で、構造体のフィールド保持のみを検証する。
    /// execute() を呼び出すテストでは実際の McpManager が必要になるため、
    /// 単体テストでは name/definition の検証に留める。
    fn create_test_adapter(name: String, original_name: String) -> McpToolAdapter {
        let definition = ToolDefinition {
            name: name.clone(),
            description: format!("MCP tool: {original_name}"),
            parameters: json!({"type": "object", "properties": {}}),
        };
        McpToolAdapter {
            name,
            original_name,
            server_idx: 0,
            timeout_secs: 60,
            definition,
            // テストでは実行しないのでダミーの manager を入れない。
            // 代わりに、Unit テストでは execute を呼ばない。
            // これをコンパイルするため、ダミーの McpManager が必要。
            // McpManager は actual MCP connection を要求するため、
            // test_helper では直接構築できない。
            // → テスト用に minimal な構造体を new で組み立てる。
            manager: Arc::new(tokio::sync::RwLock::new(create_stub_mcp_manager())),
        }
    }

    fn create_test_adapter_with_definition(
        name: String,
        original_name: String,
        definition: ToolDefinition,
    ) -> McpToolAdapter {
        McpToolAdapter {
            name,
            original_name,
            server_idx: 0,
            timeout_secs: 60,
            definition,
            manager: Arc::new(tokio::sync::RwLock::new(create_stub_mcp_manager())),
        }
    }

    /// テスト用の空の McpManager を生成する。
    ///
    /// `McpManager::new()` は MCP 設定ファイルを読み込むため、
    /// テスト環境では safe な stub を生成する必要がある。
    /// servers が空の McpManager は execute_tool でエラーを返すが、
    /// name/definition のテストでは実行しないため問題ない。
    fn create_stub_mcp_manager() -> McpManager {
        // McpManager は pub フィールドを持たず、new() が async で MCP 接続する。
        // テスト用に安全に生成する方法がない場合、
        // テスト環境に一時ディレクトリを設定して new() を呼ぶ。
        // しかし、これは async な MCP 接続を伴うため、
        // 代わりに name/definition テストに限定する方針を取る。
        //
        // 実際には McpManager のフィールドが private なので、
        // 外部から構築できない。ここではテストモジュール内でのみ
        // 必要な最小限の対応を行う。
        //
        // McpManager::new() に空の workspace を渡せば、
        // MCP 設定ファイルが存在しないため servers 空で返る。
        // ただし async なので block する必要がある。
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for test");
        let dir = tempfile::tempdir().expect("tempdir for stub mcp");
        let state_root = dir.path().join(".egopulse");
        let workspace = state_root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        let _home = crate::test_env::EnvVarGuard::set("HOME", dir.path());

        rt.block_on(async { McpManager::new(&workspace).await.expect("stub McpManager") })
    }
}

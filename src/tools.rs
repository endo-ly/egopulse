use std::cmp::min;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::config::Config;
use crate::llm::ToolDefinition;
use crate::skills::{LoadedSkill, SkillManager};

const DEFAULT_MAX_LINES: usize = 2000;
const DEFAULT_MAX_BYTES: usize = 50 * 1024;
const DEFAULT_FIND_LIMIT: usize = 1000;
const DEFAULT_GREP_LIMIT: usize = 100;
const DEFAULT_LS_LIMIT: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionContext {
    pub chat_id: i64,
    pub channel: String,
    pub surface_thread: String,
    pub chat_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(content: String) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    pub fn error(content: String) -> Self {
        Self {
            content,
            is_error: true,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: serde_json::Value, context: &ToolExecutionContext)
    -> ToolResult;
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(config: &Config, skill_manager: Arc<SkillManager>) -> Self {
        let workspace_dir = config.workspace_dir();
        if let Err(error) = std::fs::create_dir_all(&workspace_dir) {
            tracing::warn!(
                workspace_dir = %workspace_dir.display(),
                "failed to create workspace dir: {error}"
            );
        }

        Self {
            tools: vec![
                Box::new(ReadTool::new(workspace_dir.clone())),
                Box::new(BashTool::new(workspace_dir.clone())),
                Box::new(EditTool::new(workspace_dir.clone())),
                Box::new(WriteTool::new(workspace_dir.clone())),
                Box::new(GrepTool::new(workspace_dir.clone())),
                Box::new(FindTool::new(workspace_dir.clone())),
                Box::new(LsTool::new(workspace_dir)),
                Box::new(ActivateSkillTool::new(skill_manager)),
            ],
        }
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        for tool in &self.tools {
            if tool.name() == name {
                return tool.execute(input, context).await;
            }
        }
        ToolResult::error(format!("Unknown tool: {name}"))
    }
}

#[derive(Debug, Clone)]
struct TruncationResult {
    content: String,
    truncated: bool,
    truncated_by: Option<&'static str>,
    total_lines: usize,
    output_lines: usize,
    max_bytes: usize,
    first_line_exceeds_limit: bool,
}

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn truncate_head(content: &str, max_lines: usize, max_bytes: usize) -> TruncationResult {
    let total_bytes = content.len();
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            output_lines: total_lines,
            max_bytes,
            first_line_exceeds_limit: false,
        };
    }

    if lines
        .first()
        .map(|line| line.len() > max_bytes)
        .unwrap_or(false)
    {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some("bytes"),
            total_lines,
            output_lines: 0,
            max_bytes,
            first_line_exceeds_limit: true,
        };
    }

    let mut selected = Vec::new();
    let mut bytes = 0usize;
    let mut truncated_by = Some("lines");
    for (index, line) in lines.iter().enumerate() {
        if index >= max_lines {
            truncated_by = Some("lines");
            break;
        }
        let line_bytes = line.len() + usize::from(index > 0);
        if bytes + line_bytes > max_bytes {
            truncated_by = Some("bytes");
            break;
        }
        selected.push(*line);
        bytes += line_bytes;
    }

    let output = selected.join("\n");
    TruncationResult {
        output_lines: selected.len(),
        content: output,
        truncated: true,
        truncated_by,
        total_lines,
        max_bytes,
        first_line_exceeds_limit: false,
    }
}

fn truncate_tail(content: &str, max_lines: usize, max_bytes: usize) -> TruncationResult {
    let total_bytes = content.len();
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            output_lines: total_lines,
            max_bytes,
            first_line_exceeds_limit: false,
        };
    }

    let mut selected = Vec::new();
    let mut bytes = 0usize;
    let mut truncated_by = Some("lines");
    for (reverse_index, line) in lines.iter().rev().enumerate() {
        if reverse_index >= max_lines {
            truncated_by = Some("lines");
            break;
        }
        let line_bytes = line.len() + usize::from(reverse_index > 0);
        if bytes + line_bytes > max_bytes {
            truncated_by = Some("bytes");
            break;
        }
        selected.push(*line);
        bytes += line_bytes;
    }
    selected.reverse();
    let output = selected.join("\n");
    TruncationResult {
        output_lines: selected.len(),
        content: output,
        truncated: true,
        truncated_by,
        total_lines,
        max_bytes,
        first_line_exceeds_limit: false,
    }
}

struct ReadTool {
    workspace_dir: PathBuf,
}

impl ReadTool {
    fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".to_string(),
            description: "Read the contents of a file. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.".to_string(),
            parameters: schema_object(
                json!({
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative or absolute)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (1-indexed)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read"
                    }
                }),
                &["path"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(path) = input.get("path").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: path".to_string());
        };
        let resolved = match resolve_workspace_path(&self.workspace_dir, path) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        };

        let bytes = match tokio::fs::read(&resolved).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("File not found: {path}"));
            }
            Err(error) => return ToolResult::error(format!("Failed to read file: {error}")),
        };
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                return ToolResult::error(
                    "Only UTF-8 text files are supported by read in egopulse right now."
                        .to_string(),
                );
            }
        };

        let all_lines = content.split('\n').collect::<Vec<_>>();
        let start_line = input
            .get("offset")
            .and_then(|value| value.as_u64())
            .map(|value| value.saturating_sub(1) as usize)
            .unwrap_or(0);
        if start_line >= all_lines.len() && !all_lines.is_empty() {
            let requested = input
                .get("offset")
                .and_then(|value| value.as_u64())
                .unwrap_or(1);
            return ToolResult::error(format!(
                "Offset {requested} is beyond end of file ({} lines total)",
                all_lines.len()
            ));
        }

        let user_limit = input
            .get("limit")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize);
        let selected_content = if let Some(limit) = user_limit {
            let end = min(start_line + limit, all_lines.len());
            all_lines[start_line..end].join("\n")
        } else {
            all_lines[start_line..].join("\n")
        };

        let truncation = truncate_head(&selected_content, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
        let start_display = start_line + 1;
        let total_lines = all_lines.len();
        let mut output = if truncation.first_line_exceeds_limit {
            format!(
                "[Line {} exceeds {} limit. Use a more targeted offset/limit.]",
                start_display,
                format_size(DEFAULT_MAX_BYTES)
            )
        } else {
            truncation.content.clone()
        };

        if truncation.truncated && !truncation.first_line_exceeds_limit {
            let end_display = start_display + truncation.output_lines.saturating_sub(1);
            let next_offset = end_display + 1;
            if truncation.truncated_by == Some("lines") {
                output.push_str(&format!(
                    "\n\n[Showing lines {start_display}-{end_display} of {total_lines}. Use offset={next_offset} to continue.]"
                ));
            } else {
                output.push_str(&format!(
                    "\n\n[Showing lines {start_display}-{end_display} of {total_lines} ({} limit). Use offset={next_offset} to continue.]",
                    format_size(DEFAULT_MAX_BYTES)
                ));
            }
        } else if let Some(limit) = user_limit
            && start_line + limit < all_lines.len()
        {
            let remaining = all_lines.len() - (start_line + limit);
            let next_offset = start_line + limit + 1;
            output.push_str(&format!(
                "\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]"
            ));
        }

        ToolResult::success(output)
    }
}

struct WriteTool {
    workspace_dir: PathBuf,
}

impl WriteTool {
    fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".to_string(),
            description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_string(),
            parameters: schema_object(
                json!({
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write (relative or absolute)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                }),
                &["path", "content"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(path) = input.get("path").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: path".to_string());
        };
        let Some(content) = input.get("content").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: content".to_string());
        };
        let resolved = match resolve_workspace_path(&self.workspace_dir, path) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        };
        if let Some(parent) = resolved.parent()
            && let Err(error) = tokio::fs::create_dir_all(parent).await
        {
            return ToolResult::error(format!("Failed to create directories: {error}"));
        }
        match tokio::fs::write(&resolved, content).await {
            Ok(()) => ToolResult::success(format!("Successfully wrote {}", resolved.display())),
            Err(error) => ToolResult::error(format!("Failed to write file: {error}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditSpec {
    old_text: String,
    new_text: String,
}

struct EditTool {
    workspace_dir: PathBuf,
}

impl EditTool {
    fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit".to_string(),
            description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits.".to_string(),
            parameters: schema_object(
                json!({
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative or absolute)"
                    },
                    "edits": {
                        "type": "array",
                        "description": "One or more targeted replacements matched against the original file.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "oldText": {
                                    "type": "string",
                                    "description": "Exact text for one targeted replacement. It must be unique in the original file."
                                },
                                "newText": {
                                    "type": "string",
                                    "description": "Replacement text for this targeted edit."
                                }
                            },
                            "required": ["oldText", "newText"]
                        }
                    }
                }),
                &["path", "edits"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(path) = input.get("path").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: path".to_string());
        };
        let edits = match parse_edits(&input) {
            Ok(edits) => edits,
            Err(error) => return ToolResult::error(error),
        };
        let resolved = match resolve_workspace_path(&self.workspace_dir, path) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        };
        let raw_content = match tokio::fs::read_to_string(&resolved).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("File not found: {path}"));
            }
            Err(error) => return ToolResult::error(format!("Failed to read file: {error}")),
        };

        let has_bom = raw_content.starts_with('\u{feff}');
        let content_without_bom = raw_content.strip_prefix('\u{feff}').unwrap_or(&raw_content);
        let uses_crlf = content_without_bom.contains("\r\n");
        let normalized_original = content_without_bom.replace("\r\n", "\n");
        let normalized_edits = edits
            .into_iter()
            .map(|edit| EditSpec {
                old_text: edit.old_text.replace("\r\n", "\n"),
                new_text: edit.new_text.replace("\r\n", "\n"),
            })
            .collect::<Vec<_>>();

        let updated = match apply_edits_to_original(&normalized_original, &normalized_edits) {
            Ok(updated) => updated,
            Err(error) => return ToolResult::error(error),
        };
        let restored = if uses_crlf {
            updated.replace('\n', "\r\n")
        } else {
            updated
        };
        let final_content = if has_bom {
            format!("\u{feff}{restored}")
        } else {
            restored
        };
        match tokio::fs::write(&resolved, final_content).await {
            Ok(()) => ToolResult::success(format!(
                "Successfully replaced {} block(s) in {}.",
                normalized_edits.len(),
                path
            )),
            Err(error) => ToolResult::error(format!("Failed to write file: {error}")),
        }
    }
}

struct BashTool {
    workspace_dir: PathBuf,
}

impl BashTool {
    fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute a bash command in the workspace. Returns the tail of stdout/stderr, truncated to 2000 lines or 50KB.".to_string(),
            parameters: schema_object(
                json!({
                    "command": {
                        "type": "string",
                        "description": "Bash command to execute"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (optional, no default timeout)"
                    }
                }),
                &["command"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(command) = input.get("command").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: command".to_string());
        };
        let timeout_secs = input.get("timeout").and_then(|value| value.as_u64());
        let mut command_builder = Command::new("bash");
        command_builder
            .arg("-lc")
            .arg(command)
            .current_dir(&self.workspace_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let output = if let Some(timeout_secs) = timeout_secs {
            match timeout(Duration::from_secs(timeout_secs), command_builder.output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    return ToolResult::error(format!("Failed to execute bash command: {error}"));
                }
                Err(_) => {
                    return ToolResult::error(format!(
                        "Command timed out after {timeout_secs} seconds"
                    ));
                }
            }
        } else {
            match command_builder.output().await {
                Ok(output) => output,
                Err(error) => {
                    return ToolResult::error(format!("Failed to execute bash command: {error}"));
                }
            }
        };

        let combined = combine_command_output(&output.stdout, &output.stderr);
        let truncation = truncate_tail(&combined, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
        let mut text = if truncation.content.is_empty() {
            "(no output)".to_string()
        } else {
            truncation.content
        };
        if truncation.truncated {
            let start_line = truncation
                .total_lines
                .saturating_sub(truncation.output_lines)
                + 1;
            let end_line = truncation.total_lines;
            if truncation.truncated_by == Some("lines") {
                text.push_str(&format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {}.]",
                    truncation.total_lines
                ));
            } else {
                text.push_str(&format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit).]",
                    truncation.total_lines,
                    format_size(truncation.max_bytes)
                ));
            }
        }
        if !output.status.success() {
            if let Some(code) = output.status.code() {
                text.push_str(&format!("\n\nCommand exited with code {code}"));
            }
            return ToolResult::error(text);
        }

        ToolResult::success(text)
    }
}

struct GrepTool {
    workspace_dir: PathBuf,
}

impl GrepTool {
    fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".to_string(),
            description: "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore.".to_string(),
            parameters: schema_object(
                json!({
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (regex or literal string)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search (default: current directory)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'"
                    },
                    "ignoreCase": {
                        "type": "boolean",
                        "description": "Case-insensitive search (default: false)"
                    },
                    "literal": {
                        "type": "boolean",
                        "description": "Treat pattern as literal string instead of regex (default: false)"
                    },
                    "context": {
                        "type": "integer",
                        "description": "Number of lines to show before and after each match (default: 0)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of matches to return (default: 100)"
                    }
                }),
                &["pattern"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(pattern) = input.get("pattern").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: pattern".to_string());
        };
        let requested_path = input
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(".");
        let resolved = match resolve_workspace_path(&self.workspace_dir, requested_path) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        };
        if !resolved.exists() {
            return ToolResult::error(format!("Path not found: {}", resolved.display()));
        }
        let limit = input
            .get("limit")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_GREP_LIMIT);
        let ignore_case = input
            .get("ignoreCase")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let literal = input
            .get("literal")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let context_lines = input
            .get("context")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(0);
        let file_glob = input.get("glob").and_then(|value| value.as_str());

        let (cwd, target) = command_scope_for_path(&resolved);
        let mut command = Command::new("rg");
        command
            .arg("--line-number")
            .arg("--color=never")
            .arg("--hidden");
        if ignore_case {
            command.arg("--ignore-case");
        }
        if literal {
            command.arg("--fixed-strings");
        }
        if context_lines > 0 {
            command.arg("-C").arg(context_lines.to_string());
        }
        if let Some(file_glob) = file_glob {
            command.arg("--glob").arg(file_glob);
        }
        command
            .arg(pattern)
            .arg(target)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = match command.output().await {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(
                    "ripgrep (rg) is not available. Install rg to use grep.".to_string(),
                );
            }
            Err(error) => return ToolResult::error(format!("Failed to run rg: {error}")),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
        if stdout.trim().is_empty() && output.status.code() == Some(1) {
            return ToolResult::success("No matches found.".to_string());
        }
        if !output.status.success() && output.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return ToolResult::error(format!("Search failed: {}", stderr.trim()));
        }

        let lines = stdout.lines().map(truncate_grep_line).collect::<Vec<_>>();
        let result_limit_reached = lines.len() > limit;
        let limited = if result_limit_reached {
            lines[..limit].to_vec()
        } else {
            lines
        };
        let raw_output = limited.join("\n");
        let truncation = truncate_head(&raw_output, usize::MAX, DEFAULT_MAX_BYTES);
        let mut text = if truncation.content.is_empty() {
            "No matches found.".to_string()
        } else {
            truncation.content
        };
        let mut notices = Vec::new();
        if result_limit_reached {
            notices.push(format!("{limit} matches limit"));
        }
        if truncation.truncated {
            notices.push(format!("{} limit", format_size(DEFAULT_MAX_BYTES)));
        }
        if !notices.is_empty() {
            text.push_str(&format!("\n\n[Truncated: {}]", notices.join(", ")));
        }

        ToolResult::success(text)
    }
}

struct FindTool {
    workspace_dir: PathBuf,
}

impl FindTool {
    fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "find".to_string(),
            description: "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore.".to_string(),
            parameters: schema_object(
                json!({
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in (default: current directory)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 1000)"
                    }
                }),
                &["pattern"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(pattern) = input.get("pattern").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: pattern".to_string());
        };
        let requested_path = input
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(".");
        let resolved = match resolve_workspace_path(&self.workspace_dir, requested_path) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        };
        if !resolved.exists() {
            return ToolResult::error(format!("Path not found: {}", resolved.display()));
        }
        let limit = input
            .get("limit")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_FIND_LIMIT);
        let search_dir = if resolved.is_dir() {
            resolved.clone()
        } else {
            resolved
                .parent()
                .unwrap_or(&self.workspace_dir)
                .to_path_buf()
        };

        let mut command = Command::new("fd");
        command
            .arg("--glob")
            .arg("--color=never")
            .arg("--hidden")
            .arg("--max-results")
            .arg(limit.to_string())
            .arg(pattern)
            .arg(".")
            .current_dir(&search_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = match command.output().await {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(
                    "fd is not available. Install fd to use find.".to_string(),
                );
            }
            Err(error) => return ToolResult::error(format!("Failed to run fd: {error}")),
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return ToolResult::error(format!("Search failed: {}", stderr.trim()));
        }

        let results = String::from_utf8_lossy(&output.stdout)
            .replace("\r\n", "\n")
            .lines()
            .map(|line| line.trim_end_matches('/').to_string())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        if results.is_empty() {
            return ToolResult::success("No files found matching pattern".to_string());
        }

        let result_limit_reached = results.len() >= limit;
        let raw_output = results.join("\n");
        let truncation = truncate_head(&raw_output, usize::MAX, DEFAULT_MAX_BYTES);
        let mut text = truncation.content;
        let mut notices = Vec::new();
        if result_limit_reached {
            notices.push(format!("{limit} results limit reached"));
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        }
        if !notices.is_empty() {
            text.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }
        ToolResult::success(text)
    }
}

struct LsTool {
    workspace_dir: PathBuf,
}

impl LsTool {
    fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "ls".to_string(),
            description: "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles.".to_string(),
            parameters: schema_object(
                json!({
                    "path": {
                        "type": "string",
                        "description": "Directory to list (default: current directory)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of entries to return (default: 500)"
                    }
                }),
                &[],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let requested_path = input
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(".");
        let resolved = match resolve_workspace_path(&self.workspace_dir, requested_path) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        };
        if !resolved.exists() {
            return ToolResult::error(format!("Path not found: {}", resolved.display()));
        }
        if !resolved.is_dir() {
            return ToolResult::error(format!("Not a directory: {}", resolved.display()));
        }
        let limit = input
            .get("limit")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_LS_LIMIT);

        let mut entries = match std::fs::read_dir(&resolved) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let mut name = entry.file_name().to_string_lossy().to_string();
                    if entry.file_type().ok()?.is_dir() {
                        name.push('/');
                    }
                    Some(name)
                })
                .collect::<Vec<_>>(),
            Err(error) => return ToolResult::error(format!("Cannot read directory: {error}")),
        };

        entries.sort_by_key(|entry| entry.to_ascii_lowercase());
        if entries.is_empty() {
            return ToolResult::success("(empty directory)".to_string());
        }

        let entry_limit_reached = entries.len() > limit;
        let limited = if entry_limit_reached {
            entries[..limit].to_vec()
        } else {
            entries
        };
        let raw_output = limited.join("\n");
        let truncation = truncate_head(&raw_output, usize::MAX, DEFAULT_MAX_BYTES);
        let mut text = truncation.content;
        let mut notices = Vec::new();
        if entry_limit_reached {
            notices.push(format!(
                "{limit} entries limit reached. Use limit={} for more",
                limit * 2
            ));
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        }
        if !notices.is_empty() {
            text.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }
        ToolResult::success(text)
    }
}

struct ActivateSkillTool {
    skill_manager: Arc<SkillManager>,
}

impl ActivateSkillTool {
    fn new(skill_manager: Arc<SkillManager>) -> Self {
        Self { skill_manager }
    }
}

#[async_trait]
impl Tool for ActivateSkillTool {
    fn name(&self) -> &str {
        "activate_skill"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "activate_skill".to_string(),
            description: "Load the full instructions for a discovered skill. Use this when a skill from the available skills catalog matches the task.".to_string(),
            parameters: schema_object(
                json!({
                    "skill_name": {
                        "type": "string",
                        "description": "The skill name to load"
                    }
                }),
                &["skill_name"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(skill_name) = input.get("skill_name").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: skill_name".to_string());
        };

        match self.skill_manager.load_skill_checked(skill_name) {
            Ok(LoadedSkill {
                metadata,
                instructions,
            }) => ToolResult::success(format!(
                "# Skill: {}\n\nDescription: {}\nSkill directory: {}\n\n## Instructions\n\n{}",
                metadata.name,
                metadata.description,
                metadata.dir_path.display(),
                instructions
            )),
            Err(error) => ToolResult::error(error),
        }
    }
}

fn parse_edits(input: &serde_json::Value) -> Result<Vec<EditSpec>, String> {
    if let Some(edits) = input.get("edits").and_then(|value| value.as_array()) {
        if edits.is_empty() {
            return Err(
                "Edit tool input is invalid. edits must contain at least one replacement."
                    .to_string(),
            );
        }
        let mut parsed = Vec::with_capacity(edits.len());
        for edit in edits {
            let Some(old_text) = edit.get("oldText").and_then(|value| value.as_str()) else {
                return Err("Each edit must include oldText".to_string());
            };
            let Some(new_text) = edit.get("newText").and_then(|value| value.as_str()) else {
                return Err("Each edit must include newText".to_string());
            };
            parsed.push(EditSpec {
                old_text: old_text.to_string(),
                new_text: new_text.to_string(),
            });
        }
        return Ok(parsed);
    }

    if let (Some(old_text), Some(new_text)) = (
        input.get("oldText").and_then(|value| value.as_str()),
        input.get("newText").and_then(|value| value.as_str()),
    ) {
        return Ok(vec![EditSpec {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        }]);
    }

    Err("Edit tool input is invalid. edits must contain at least one replacement.".to_string())
}

fn apply_edits_to_original(original: &str, edits: &[EditSpec]) -> Result<String, String> {
    let mut ranges = Vec::with_capacity(edits.len());
    for edit in edits {
        let matches = original.match_indices(&edit.old_text).collect::<Vec<_>>();
        if matches.is_empty() {
            return Err("oldText not found in file. Make sure it matches exactly.".to_string());
        }
        if matches.len() > 1 {
            return Err(format!(
                "oldText found {} times in file. It must be unique.",
                matches.len()
            ));
        }
        let (start, matched) = matches[0];
        let end = start + matched.len();
        ranges.push((start, end, edit.new_text.as_str()));
    }
    ranges.sort_by_key(|(start, _, _)| *start);
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(
                "Edit ranges overlap. Merge nearby changes into one edit instead.".to_string(),
            );
        }
    }

    let mut result = String::with_capacity(original.len());
    let mut cursor = 0usize;
    for (start, end, replacement) in ranges {
        result.push_str(&original[cursor..start]);
        result.push_str(replacement);
        cursor = end;
    }
    result.push_str(&original[cursor..]);
    Ok(result)
}

fn truncate_grep_line(line: &str) -> String {
    const GREP_MAX_LINE_LENGTH: usize = 500;
    if line.chars().count() <= GREP_MAX_LINE_LENGTH {
        return line.to_string();
    }
    format!(
        "{}...",
        line.chars().take(GREP_MAX_LINE_LENGTH).collect::<String>()
    )
}

fn command_scope_for_path(path: &Path) -> (&Path, &str) {
    if path.is_dir() {
        (path, ".")
    } else {
        (
            path.parent().unwrap_or_else(|| Path::new(".")),
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("."),
        )
    }
}

fn combine_command_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout).replace("\r\n", "\n");
    let stderr = String::from_utf8_lossy(stderr).replace("\r\n", "\n");
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (false, false) => format!("{}\n{}", stdout.trim_end(), stderr.trim_end()),
        (false, true) => stdout.trim_end().to_string(),
        (true, false) => stderr.trim_end().to_string(),
        (true, true) => String::new(),
    }
}

fn resolve_workspace_path(workspace_dir: &Path, requested_path: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(requested_path);
    let candidate = if requested.is_absolute() {
        requested
    } else {
        workspace_dir.join(requested)
    };

    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str())
            }
        }
    }

    if !normalized.starts_with(workspace_dir) {
        return Err(format!(
            "Refusing to access path outside workspace: {}",
            normalized.display()
        ));
    }

    Ok(normalized)
}

fn schema_object(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

#[cfg(test)]
mod tests {
    use super::{ToolExecutionContext, ToolRegistry};
    use crate::config::{ChannelConfig, Config};
    use crate::skills::SkillManager;

    use serde_json::json;
    use serial_test::serial;
    use std::sync::Arc;

    struct HomeGuard {
        original: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let original = std::env::var_os("HOME");
            unsafe {
                std::env::set_var("HOME", path);
            }
            Self { original }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => unsafe {
                    std::env::set_var("HOME", value);
                },
                None => unsafe {
                    std::env::remove_var("HOME");
                },
            }
        }
    }

    fn test_config(data_dir: &str) -> Config {
        Config {
            model: "gpt-4o-mini".to_string(),
            api_key: None,
            llm_base_url: "http://127.0.0.1:1234/v1".to_string(),
            data_dir: data_dir.to_string(),
            log_level: "info".to_string(),
            channels: std::collections::HashMap::from([(
                "web".to_string(),
                ChannelConfig {
                    enabled: Some(true),
                    ..Default::default()
                },
            )]),
        }
    }

    fn test_context() -> ToolExecutionContext {
        ToolExecutionContext {
            chat_id: 1,
            channel: "cli".to_string(),
            surface_thread: "demo".to_string(),
            chat_type: "cli".to_string(),
        }
    }

    fn test_registry(config: &Config) -> ToolRegistry {
        ToolRegistry::new(
            config,
            Arc::new(SkillManager::from_skills_dir(config.skills_dir())),
        )
    }

    #[tokio::test]
    #[serial]
    async fn read_respects_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir();
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("notes.txt"), "hello\nworld").expect("write file");
        let registry = test_registry(&config);

        let result = registry
            .execute("read", json!({"path": "notes.txt"}), &test_context())
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
        assert!(!result.content.contains("1\t"));
    }

    #[tokio::test]
    #[serial]
    async fn write_creates_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let registry = test_registry(&config);

        let result = registry
            .execute(
                "write",
                json!({"path": "src/demo.txt", "content": "hello world"}),
                &test_context(),
            )
            .await;
        assert!(!result.is_error);
        assert_eq!(
            std::fs::read_to_string(config.workspace_dir().join("src/demo.txt")).expect("read"),
            "hello world"
        );
    }

    #[tokio::test]
    #[serial]
    async fn edit_replaces_exact_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir();
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("notes.txt"), "alpha\nbeta\ngamma\n").expect("write");
        let registry = test_registry(&config);

        let result = registry
            .execute(
                "edit",
                json!({
                    "path": "notes.txt",
                    "edits": [{"oldText": "beta", "newText": "delta"}]
                }),
                &test_context(),
            )
            .await;
        assert!(!result.is_error);
        let content = std::fs::read_to_string(workspace.join("notes.txt")).expect("read");
        assert!(content.contains("delta"));
        assert!(!content.contains("beta"));
    }

    #[tokio::test]
    #[serial]
    async fn ls_lists_directory_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir();
        std::fs::create_dir_all(workspace.join("nested")).expect("nested");
        std::fs::write(workspace.join("a.txt"), "a").expect("a");
        std::fs::write(workspace.join(".hidden"), "b").expect("hidden");
        let registry = test_registry(&config);

        let result = registry.execute("ls", json!({}), &test_context()).await;
        assert!(!result.is_error);
        assert!(result.content.contains(".hidden"));
        assert!(result.content.contains("nested/"));
    }

    #[tokio::test]
    #[serial]
    async fn find_discovers_matching_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir();
        std::fs::create_dir_all(workspace.join("src")).expect("src");
        std::fs::write(workspace.join("src/lib.rs"), "pub fn demo() {}").expect("lib");
        let registry = test_registry(&config);

        let result = registry
            .execute("find", json!({"pattern": "*.rs"}), &test_context())
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("src/lib.rs"));
    }

    #[tokio::test]
    #[serial]
    async fn grep_finds_matching_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir();
        std::fs::create_dir_all(workspace.join("src")).expect("src");
        std::fs::write(workspace.join("src/lib.rs"), "pub fn demo() {}\n").expect("lib");
        let registry = test_registry(&config);

        let result = registry
            .execute("grep", json!({"pattern": "demo"}), &test_context())
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("src/lib.rs:1:pub fn demo() {}"));
    }

    #[tokio::test]
    #[serial]
    async fn bash_runs_in_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir();
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("notes.txt"), "hello").expect("notes");
        let registry = test_registry(&config);

        let result = registry
            .execute(
                "bash",
                json!({"command": "printf 'ok\\n'; cat notes.txt"}),
                &test_context(),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("ok"));
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    #[serial]
    async fn activate_skill_loads_skill_instructions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let skills_dir = config.skills_dir();
        std::fs::create_dir_all(skills_dir.join("pdf")).expect("skill dir");
        std::fs::write(
            skills_dir.join("pdf").join("SKILL.md"),
            "---\nname: pdf\ndescription: PDF helper\n---\nUse the PDF flow.\n",
        )
        .expect("write skill");
        let registry = test_registry(&config);

        let result = registry
            .execute(
                "activate_skill",
                json!({"skill_name": "pdf"}),
                &test_context(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("# Skill: pdf"));
        assert!(result.content.contains("Use the PDF flow."));
    }
}

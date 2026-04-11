//! LLM エージェント向けファイル操作・シェルツール群。
//!
//! ワークスペース内で動作する read / write / edit / bash / grep / find / ls の
//! 7 種のファイル操作ツールと、スキル遅延読み込み用の activate_skill を提供する。
//! 各ツールは出力を行数・バイト数で切り詰め、LLM のコンテキストウィンドウに収まるよう制御する。

use std::cmp::min;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use serde_json::json;
use similar::TextDiff;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::config::Config;
use crate::llm::{MessageContent, MessageContentPart, ToolDefinition};
use crate::skills::{LoadedSkill, SkillManager};

const DEFAULT_MAX_LINES: usize = 2000;
const DEFAULT_MAX_BYTES: usize = 50 * 1024;
const DEFAULT_FIND_LIMIT: usize = 1000;
const DEFAULT_GREP_LIMIT: usize = 100;
const DEFAULT_LS_LIMIT: usize = 500;
const GREP_MAX_LINE_LENGTH: usize = 500;
const DEFAULT_BASH_TIMEOUT_SECS: u64 = 30;

/// Contextual metadata passed to every tool execution (chat identity, channel, thread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionContext {
    pub chat_id: i64,
    pub channel: String,
    pub surface_thread: String,
    pub chat_type: String,
}

/// Uniform result type returned by all tool implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    pub details: Option<serde_json::Value>,
    pub llm_content: MessageContent,
}

impl ToolResult {
    /// Create a successful result with plain text content.
    pub fn success(content: String) -> Self {
        Self {
            llm_content: MessageContent::text(content.clone()),
            content,
            is_error: false,
            details: None,
        }
    }

    /// Create a successful result with structured details (e.g. truncation metadata).
    pub fn success_with_details(content: String, details: serde_json::Value) -> Self {
        Self {
            llm_content: MessageContent::text(content.clone()),
            content,
            is_error: false,
            details: Some(details),
        }
    }

    /// Create a successful result with separate LLM-facing multimodal content (e.g. images).
    pub fn success_with_llm_content(content: String, llm_content: MessageContent) -> Self {
        Self {
            content,
            is_error: false,
            details: None,
            llm_content,
        }
    }

    /// Create an error result with plain text content.
    pub fn error(content: String) -> Self {
        Self {
            llm_content: MessageContent::text(content.clone()),
            content,
            is_error: true,
            details: None,
        }
    }

    /// Create an error result with structured details.
    pub fn error_with_details(content: String, details: serde_json::Value) -> Self {
        Self {
            llm_content: MessageContent::text(content.clone()),
            content,
            is_error: true,
            details: Some(details),
        }
    }
}

/// Trait implemented by every tool available to the LLM agent.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: serde_json::Value, context: &ToolExecutionContext)
    -> ToolResult;
}

/// Owns all tool instances and dispatches execution by tool name.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    mcp_manager: Option<std::sync::Arc<tokio::sync::RwLock<crate::mcp::McpManager>>>,
}

impl ToolRegistry {
    /// Instantiate all built-in tools scoped to the configured workspace.
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
            mcp_manager: None,
        }
    }

    /// Register a dynamically created tool (e.g. MCP tool wrapper).
    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Set the MCP manager for dynamic tool dispatch.
    pub fn set_mcp_manager(
        &mut self,
        manager: std::sync::Arc<tokio::sync::RwLock<crate::mcp::McpManager>>,
    ) {
        self.mcp_manager = Some(manager);
    }

    /// Collect tool definitions synchronously (internal only).
    /// External callers must use [`definitions_async`] to avoid blocking an async runtime.
    #[allow(dead_code)]
    pub(crate) fn definitions(&self) -> Vec<ToolDefinition> {
        let mut defs: Vec<ToolDefinition> =
            self.tools.iter().map(|tool| tool.definition()).collect();

        if let Some(mcp) = &self.mcp_manager {
            let mcp_defs = mcp.blocking_read().all_tool_definitions();
            defs.extend(mcp_defs);
        }

        defs
    }

    /// Collect tool definitions asynchronously (preferrred when MCP is present).
    pub async fn definitions_async(&self) -> Vec<ToolDefinition> {
        let mut defs: Vec<ToolDefinition> =
            self.tools.iter().map(|tool| tool.definition()).collect();

        if let Some(mcp) = &self.mcp_manager {
            defs.extend(mcp.read().await.all_tool_definitions());
        }

        defs
    }

    /// Find and execute a tool by name. Returns an error result for unknown tools.
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

        if let Some(mcp) = &self.mcp_manager {
            let is_mcp = {
                let guard = mcp.read().await;
                guard.is_mcp_tool(name)
            };
            if is_mcp {
                let guard = mcp.read().await;
                match guard.execute_tool(name, input).await {
                    Ok(output) => return ToolResult::success(output),
                    Err(error) => return ToolResult::error(error.to_string()),
                }
            }
        }

        ToolResult::error(format!("Unknown tool: {name}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TruncationResult {
    content: String,
    truncated: bool,
    truncated_by: Option<&'static str>,
    total_lines: usize,
    total_bytes: usize,
    output_lines: usize,
    output_bytes: usize,
    last_line_partial: bool,
    first_line_exceeds_limit: bool,
    max_lines: usize,
    max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditSpec {
    old_text: String,
    new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedEditsResult {
    base_content: String,
    new_content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
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
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let first_line_bytes = lines.first().map(|line| line.len()).unwrap_or(0);
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some("bytes"),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
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
        content: output.clone(),
        truncated: true,
        truncated_by,
        total_lines,
        total_bytes,
        output_lines: selected.len(),
        output_bytes: output.len(),
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

fn truncate_string_to_bytes_from_end(value: &str, max_bytes: usize) -> String {
    let bytes = value.as_bytes();
    if bytes.len() <= max_bytes {
        return value.to_string();
    }
    // UTF-8 境界まで前方にシフトしてマルチバイト文字の切断を防ぐ
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).to_string()
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
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let mut selected = Vec::new();
    let mut bytes = 0usize;
    let mut truncated_by = Some("lines");
    let mut last_line_partial = false;

    for line in lines.iter().rev() {
        if selected.len() >= max_lines {
            truncated_by = Some("lines");
            break;
        }
        let line_bytes = line.len() + usize::from(!selected.is_empty());
        if bytes + line_bytes > max_bytes {
            truncated_by = Some("bytes");
            if selected.is_empty() {
                selected.push(truncate_string_to_bytes_from_end(line, max_bytes));
                last_line_partial = true;
            }
            break;
        }
        selected.push((*line).to_string());
        bytes += line_bytes;
    }
    selected.reverse();
    let output = selected.join("\n");
    TruncationResult {
        content: output.clone(),
        truncated: true,
        truncated_by,
        total_lines,
        total_bytes,
        output_lines: selected.len(),
        output_bytes: output.len(),
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn normalize_newlines(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn normalize_for_fuzzy_match(value: &str) -> String {
    normalize_newlines(value)
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(normalize_fuzzy_char)
        .collect()
}

fn fuzzy_byte_pos_to_original_byte_pos(original: &str, fuzzy_byte_pos: usize) -> usize {
    let fuzzy = normalize_for_fuzzy_match(original);
    let fuzzy_char_pos = fuzzy[..fuzzy_byte_pos].chars().count();
    original
        .char_indices()
        .nth(fuzzy_char_pos)
        .map(|(pos, _)| pos)
        .unwrap_or(original.len())
}

fn normalize_fuzzy_char(value: char) -> char {
    match value {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
        | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
        | '\u{3000}' => ' ',
        _ => value,
    }
}

fn detect_line_ending(content: &str) -> &'static str {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (crlf_idx, lf_idx) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}

fn restore_line_endings(content: &str, ending: &str) -> String {
    if ending == "\r\n" {
        content.replace('\n', "\r\n")
    } else {
        content.to_string()
    }
}

fn strip_bom(content: &str) -> (&str, &str) {
    if let Some(rest) = content.strip_prefix('\u{feff}') {
        ("\u{feff}", rest)
    } else {
        ("", content)
    }
}

fn fuzzy_find_text(content: &str, old_text: &str) -> Option<(usize, usize, bool)> {
    if let Some(index) = content.find(old_text) {
        return Some((index, old_text.len(), false));
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    fuzzy_content
        .find(&fuzzy_old_text)
        .map(|index| (index, fuzzy_old_text.len(), true))
}

fn count_occurrences(content: &str, needle: &str) -> usize {
    let normalized_content = normalize_for_fuzzy_match(content);
    let normalized_needle = normalize_for_fuzzy_match(needle);
    if normalized_needle.is_empty() {
        return 0;
    }
    normalized_content.match_indices(&normalized_needle).count()
}

fn get_not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        )
    }
}

fn get_duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> String {
    if total_edits == 1 {
        format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        )
    }
}

fn get_empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

fn get_no_change_error(path: &str, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        )
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[EditSpec],
    path: &str,
) -> Result<AppliedEditsResult, String> {
    let normalized_edits = edits
        .iter()
        .map(|edit| EditSpec {
            old_text: normalize_newlines(&edit.old_text),
            new_text: normalize_newlines(&edit.new_text),
        })
        .collect::<Vec<_>>();

    for (index, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(get_empty_old_text_error(
                path,
                index,
                normalized_edits.len(),
            ));
        }
    }

    let initial_matches = normalized_edits
        .iter()
        .map(|edit| fuzzy_find_text(normalized_content, &edit.old_text))
        .collect::<Vec<_>>();

    let mut base_content = normalized_content.to_string();
    let mut matched_edits = Vec::with_capacity(normalized_edits.len());
    // 編集適用位置を補正するための累積オフセット。fuzzy 置換で元と正規化後の
    // バイト長が異なる場合に差分を蓄積し、後続編集の開始位置を調整する。
    let mut cumulative_offset: isize = 0;

    for (index, result) in initial_matches.iter().enumerate() {
        let Some((fuzzy_pos, fuzzy_len, used_fuzzy)) = result else {
            return Err(get_not_found_error(path, index, normalized_edits.len()));
        };

        let (span_start, span_end, match_len) = if *used_fuzzy {
            let orig_start = fuzzy_byte_pos_to_original_byte_pos(normalized_content, *fuzzy_pos);
            let fuzzy_old_text = normalize_for_fuzzy_match(&normalized_edits[index].old_text);
            let char_count = fuzzy_old_text.chars().count();
            let orig_end = normalized_content[orig_start..]
                .char_indices()
                .nth(char_count)
                .map(|(pos, _)| orig_start + pos)
                .unwrap_or(normalized_content.len());
            (orig_start, orig_end, fuzzy_old_text.len())
        } else {
            (*fuzzy_pos, *fuzzy_pos + *fuzzy_len, *fuzzy_len)
        };

        let adjusted_start = (span_start as isize + cumulative_offset) as usize;
        let adjusted_end = (span_end as isize + cumulative_offset) as usize;

        if *used_fuzzy {
            let original_span = &base_content[adjusted_start..adjusted_end];
            let normalized_span = normalize_for_fuzzy_match(original_span);
            cumulative_offset += normalized_span.len() as isize - (span_end - span_start) as isize;
            base_content = format!(
                "{}{}{}",
                &base_content[..adjusted_start],
                normalized_span,
                &base_content[adjusted_end..]
            );
        }

        let occurrences = count_occurrences(&base_content, &normalized_edits[index].old_text);
        if occurrences > 1 {
            return Err(get_duplicate_error(
                path,
                index,
                normalized_edits.len(),
                occurrences,
            ));
        }

        matched_edits.push(MatchedEdit {
            edit_index: index,
            match_index: adjusted_start,
            match_length: match_len,
            new_text: normalized_edits[index].new_text.clone(),
        });
    }

    matched_edits.sort_by_key(|edit| edit.match_index);
    for pair in matched_edits.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index, path
            ));
        }
    }

    let mut new_content = base_content.clone();
    for edit in matched_edits.iter().rev() {
        new_content = format!(
            "{}{}{}",
            &new_content[..edit.match_index],
            edit.new_text,
            &new_content[edit.match_index + edit.match_length..]
        );
    }

    if new_content == base_content {
        return Err(get_no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

fn generate_diff_string(path: &str, base_content: &str, new_content: &str) -> String {
    let diff = TextDiff::from_lines(base_content, new_content);
    diff.unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

fn first_changed_line(base_content: &str, new_content: &str) -> Option<usize> {
    let base_lines = base_content.split('\n').collect::<Vec<_>>();
    let new_lines = new_content.split('\n').collect::<Vec<_>>();
    let max_len = base_lines.len().max(new_lines.len());
    for index in 0..max_len {
        if base_lines.get(index) != new_lines.get(index) {
            return Some(index + 1);
        }
    }
    None
}

fn detect_supported_image_mime_type(bytes: &[u8], path: &Path) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }

    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

/// Reads text files and images from the workspace, with line/byte truncation.
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
            description: "Read the contents of a file. Supports text files and images (jpg, png, gif, webp). For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.".to_string(),
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

        if let Some(mime_type) = detect_supported_image_mime_type(&bytes, &resolved) {
            const MAX_IMAGE_SIZE: usize = 10 * 1024 * 1024;
            if bytes.len() > MAX_IMAGE_SIZE {
                return ToolResult::error(format!(
                    "Image file too large ({}). Maximum size is {}.",
                    format_size(bytes.len()),
                    format_size(MAX_IMAGE_SIZE)
                ));
            }
            let preview = format!("Read image file [{mime_type}]");
            let data_url = format!(
                "data:{mime_type};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            );
            return ToolResult::success_with_llm_content(
                preview.clone(),
                MessageContent::parts(vec![
                    MessageContentPart::InputText { text: preview },
                    MessageContentPart::InputImage {
                        image_url: data_url,
                        detail: Some("auto".to_string()),
                    },
                ]),
            );
        }

        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                return ToolResult::error(
                    "Failed to read file: file is not valid UTF-8 text or a supported image."
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
            let first_line_size = format_size(
                all_lines
                    .get(start_line)
                    .map(|line| line.len())
                    .unwrap_or(0),
            );
            format!(
                "[Line {start_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_display}p' {} | head -c {}]",
                format_size(DEFAULT_MAX_BYTES),
                path,
                DEFAULT_MAX_BYTES
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
            return ToolResult::success_with_details(
                output,
                json!({
                    "truncation": truncation_json(&truncation)
                }),
            );
        }

        if let Some(limit) = user_limit
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

/// Writes content to a file, creating parent directories as needed.
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
            Ok(()) => ToolResult::success(format!(
                "Successfully wrote {} bytes to {}",
                content.len(),
                path
            )),
            Err(error) => ToolResult::error(format!("Failed to write file: {error}")),
        }
    }
}

/// Applies exact-match text replacements with fuzzy Unicode normalization and overlap detection.
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
            description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".to_string(),
            parameters: schema_object(
                json!({
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative or absolute)"
                    },
                    "edits": {
                        "type": "array",
                        "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "oldText": {
                                    "type": "string",
                                    "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
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

        let (bom, content_without_bom) = strip_bom(&raw_content);
        let original_ending = detect_line_ending(content_without_bom);
        let normalized_original = normalize_newlines(content_without_bom);
        let applied = match apply_edits_to_normalized_content(&normalized_original, &edits, path) {
            Ok(applied) => applied,
            Err(error) => return ToolResult::error(error),
        };

        let final_content = format!(
            "{bom}{}",
            restore_line_endings(&applied.new_content, original_ending)
        );
        match tokio::fs::write(&resolved, final_content).await {
            Ok(()) => {
                let diff = generate_diff_string(path, &applied.base_content, &applied.new_content);
                let first_changed_line =
                    first_changed_line(&applied.base_content, &applied.new_content);
                ToolResult::success_with_details(
                    format!(
                        "Successfully edited {} with {} replacement(s).",
                        path,
                        edits.len()
                    ),
                    json!({
                        "diff": diff,
                        "firstChangedLine": first_changed_line
                    }),
                )
            }
            Err(error) => ToolResult::error(format!("Failed to write file: {error}")),
        }
    }
}

/// Executes bash commands in the workspace with configurable timeout and output capture.
struct BashTool {
    workspace_dir: PathBuf,
}

impl BashTool {
    fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }

    fn temp_dir(&self) -> PathBuf {
        self.workspace_dir.join(".tmp").join("bash")
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
            description: "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.".to_string(),
            parameters: schema_object(
                json!({
                    "command": {
                        "type": "string",
                        "description": "Bash command to execute"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (optional, default: 30)"
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
        let timeout_secs = input
            .get("timeout")
            .and_then(|value| value.as_u64())
            .unwrap_or(DEFAULT_BASH_TIMEOUT_SECS);
        let temp_dir = self.temp_dir();
        if let Err(error) = tokio::fs::create_dir_all(&temp_dir).await {
            return ToolResult::error(format!(
                "Failed to prepare bash temp directory {}: {error}",
                temp_dir.display()
            ));
        }
        let temp_path = temp_dir.join(format!("egopulse-bash-{}.log", uuid::Uuid::new_v4()));
        let quoted_temp = shell_quote(&temp_path.to_string_lossy());
        let wrapped_command = format!("({command}) > {quoted_temp} 2>&1");

        let mut child = match Command::new("bash")
            .arg("-lc")
            .arg(&wrapped_command)
            .current_dir(&self.workspace_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                return ToolResult::error(format!("Failed to execute bash command: {error}"));
            }
        };

        let status = match timeout(Duration::from_secs(timeout_secs), child.wait()).await {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(error)) => Err(format!("Failed to execute bash command: {error}")),
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                let output = read_temp_output(&temp_path);
                return bash_error_result(output, &temp_path, Some(timeout_secs), None);
            }
        };

        let status = match status {
            Ok(status) => status,
            Err(error) => return ToolResult::error(error),
        };

        let output = read_temp_output(&temp_path);
        let truncation = truncate_tail(&output, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
        let details = if truncation.truncated {
            Some(json!({
                "truncation": truncation_json(&truncation),
                "fullOutputPath": temp_path.to_string_lossy()
            }))
        } else {
            None
        };
        let mut text = if truncation.content.is_empty() {
            "(no output)".to_string()
        } else {
            truncation.content.clone()
        };

        if truncation.truncated {
            let start_line = truncation
                .total_lines
                .saturating_sub(truncation.output_lines)
                + 1;
            let end_line = truncation.total_lines;
            if truncation.last_line_partial {
                let last_line_size =
                    format_size(output.split('\n').next_back().unwrap_or_default().len());
                text.push_str(&format!(
                    "\n\n[Showing last {} of line {end_line} (line is {last_line_size}). Full output: {}]",
                    format_size(truncation.output_bytes),
                    temp_path.to_string_lossy()
                ));
            } else if truncation.truncated_by == Some("lines") {
                text.push_str(&format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {}]",
                    truncation.total_lines,
                    temp_path.to_string_lossy()
                ));
            } else {
                text.push_str(&format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {}]",
                    truncation.total_lines,
                    format_size(DEFAULT_MAX_BYTES),
                    temp_path.to_string_lossy()
                ));
            }
        } else {
            let _ = fs::remove_file(&temp_path);
        }

        if !status.success() {
            if let Some(code) = status.code() {
                text.push_str(&format!("\n\nCommand exited with code {code}"));
            }
            if let Some(details) = details {
                return ToolResult::error_with_details(text, details);
            }
            return ToolResult::error(text);
        }

        if let Some(details) = details {
            ToolResult::success_with_details(text, details)
        } else {
            ToolResult::success(text)
        }
    }
}

fn bash_error_result(
    output: String,
    temp_path: &Path,
    timeout_secs: Option<u64>,
    aborted: Option<bool>,
) -> ToolResult {
    let truncation = truncate_tail(&output, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
    let details = json!({
        "truncation": if truncation.truncated { Some(truncation_json(&truncation)) } else { None::<serde_json::Value> },
        "fullOutputPath": if truncation.truncated { Some(temp_path.to_string_lossy().to_string()) } else { None::<String> }
    });
    let mut text = if truncation.content.is_empty() {
        "(no output)".to_string()
    } else {
        truncation.content.clone()
    };
    if let Some(true) = aborted {
        text.push_str("\n\nCommand aborted");
    } else if let Some(timeout_secs) = timeout_secs {
        text.push_str(&format!(
            "\n\nCommand timed out after {timeout_secs} seconds"
        ));
    }
    if truncation.truncated {
        ToolResult::error_with_details(text, details)
    } else {
        let _ = fs::remove_file(temp_path);
        ToolResult::error(text)
    }
}

fn read_temp_output(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
        .unwrap_or_default()
}

/// Searches file contents via ripgrep with regex/literal and context line support.
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
            description: "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.".to_string(),
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
            .map(|value| value.max(1) as usize)
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
                    "ripgrep (rg) is not available and could not be downloaded".to_string(),
                );
            }
            Err(error) => return ToolResult::error(format!("Failed to run ripgrep: {error}")),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
        if stdout.trim().is_empty() && output.status.code() == Some(1) {
            return ToolResult::success("No matches found".to_string());
        }
        if !output.status.success() && output.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return ToolResult::error(if stderr.is_empty() {
                format!(
                    "ripgrep exited with code {}",
                    output.status.code().unwrap_or(-1)
                )
            } else {
                stderr
            });
        }

        let mut lines_truncated = false;
        let lines = stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (truncated, was_truncated) = truncate_grep_line(line);
                lines_truncated |= was_truncated;
                truncated
            })
            .collect::<Vec<_>>();
        let result_limit_reached = lines.len() > limit;
        let limited = if result_limit_reached {
            lines[..limit].to_vec()
        } else {
            lines
        };
        let raw_output = limited.join("\n");
        let truncation = truncate_head(&raw_output, usize::MAX, DEFAULT_MAX_BYTES);
        let mut text = if truncation.content.is_empty() {
            "No matches found".to_string()
        } else {
            truncation.content.clone()
        };

        let mut notices = Vec::new();
        if result_limit_reached {
            notices.push(format!(
                "{limit} matches limit reached. Use limit={} for more, or refine pattern",
                limit * 2
            ));
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        }
        if lines_truncated {
            notices.push(format!(
                "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"
            ));
        }
        if !notices.is_empty() {
            text.push_str(&format!("\n\n[{}]", notices.join(". ")));
            return ToolResult::success_with_details(
                text,
                json!({
                    "truncation": if truncation.truncated { Some(truncation_json(&truncation)) } else { None::<serde_json::Value> },
                    "matchLimitReached": if result_limit_reached { Some(limit) } else { None::<usize> },
                    "linesTruncated": lines_truncated
                }),
            );
        }

        ToolResult::success(text)
    }
}

/// Finds files by glob pattern via fd, respecting .gitignore.
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
            description: "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).".to_string(),
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
            .arg(limit.to_string());
        let ignore_files = tokio::task::spawn_blocking({
            let search_dir = search_dir.clone();
            move || collect_gitignore_files(&search_dir)
        })
        .await
        .unwrap_or_default();
        for ignore_file in ignore_files {
            command.arg("--ignore-file").arg(ignore_file);
        }
        command
            .arg(pattern)
            .arg(search_dir.to_string_lossy().to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = match command.output().await {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(
                    "fd is not available and could not be downloaded".to_string(),
                );
            }
            Err(error) => return ToolResult::error(format!("Failed to run fd: {error}")),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
        if !output.status.success() && stdout.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return ToolResult::error(if stderr.is_empty() {
                format!("fd exited with code {}", output.status.code().unwrap_or(-1))
            } else {
                stderr
            });
        }

        let results = stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| {
                let path = Path::new(line);
                let relative = path
                    .strip_prefix(&search_dir)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if line.ends_with('/') || line.ends_with('\\') {
                    format!("{relative}/")
                } else {
                    relative
                }
            })
            .collect::<Vec<_>>();
        if results.is_empty() {
            return ToolResult::success("No files found matching pattern".to_string());
        }

        let result_limit_reached = results.len() >= limit;
        let raw_output = results.join("\n");
        let truncation = truncate_head(&raw_output, usize::MAX, DEFAULT_MAX_BYTES);
        let mut text = truncation.content.clone();
        let mut notices = Vec::new();
        if result_limit_reached {
            notices.push(format!(
                "{limit} results limit reached. Use limit={} for more, or refine pattern",
                limit * 2
            ));
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        }
        if !notices.is_empty() {
            text.push_str(&format!("\n\n[{}]", notices.join(". ")));
            return ToolResult::success_with_details(
                text,
                json!({
                    "truncation": if truncation.truncated { Some(truncation_json(&truncation)) } else { None::<serde_json::Value> },
                    "resultLimitReached": if result_limit_reached { Some(limit) } else { None::<usize> }
                }),
            );
        }

        ToolResult::success(text)
    }
}

fn collect_gitignore_files(root: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default();
                if name == ".git" || name == "node_modules" {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file()
                && path.file_name().and_then(|value| value.to_str()) == Some(".gitignore")
            {
                results.push(path);
            }
        }
    }
    results.sort();
    results
}

/// Lists directory entries alphabetically with `/` suffix for subdirectories.
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
            description: "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).".to_string(),
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

        let mut entries = match tokio::fs::read_dir(&resolved).await {
            Ok(mut dir) => {
                let mut names = Vec::new();
                while let Some(entry) = dir.next_entry().await.ok().flatten() {
                    let mut name = entry.file_name().to_string_lossy().to_string();
                    if let Ok(file_type) = entry.file_type().await {
                        if file_type.is_dir() {
                            name.push('/');
                        }
                    }
                    names.push(name);
                }
                names
            }
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
        let mut text = truncation.content.clone();
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
            return ToolResult::success_with_details(
                text,
                json!({
                    "truncation": if truncation.truncated { Some(truncation_json(&truncation)) } else { None::<serde_json::Value> },
                    "entryLimitReached": if entry_limit_reached { Some(limit) } else { None::<usize> }
                }),
            );
        }
        ToolResult::success(text)
    }
}

/// Loads a skill's full instructions on demand by name.
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
    let mut parsed = Vec::new();
    if let Some(edits) = input.get("edits").and_then(|value| value.as_array()) {
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
    }

    if let (Some(old_text), Some(new_text)) = (
        input.get("oldText").and_then(|value| value.as_str()),
        input.get("newText").and_then(|value| value.as_str()),
    ) {
        parsed.push(EditSpec {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        });
    }

    if parsed.is_empty() {
        return Err(
            "Edit tool input is invalid. edits must contain at least one replacement.".to_string(),
        );
    }
    Ok(parsed)
}

fn truncate_grep_line(line: &str) -> (String, bool) {
    if line.chars().count() <= GREP_MAX_LINE_LENGTH {
        return (line.to_string(), false);
    }
    (
        format!(
            "{}...",
            line.chars().take(GREP_MAX_LINE_LENGTH).collect::<String>()
        ),
        true,
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

fn resolve_workspace_path(workspace_dir: &Path, requested_path: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(requested_path);
    let candidate = if requested.is_absolute() {
        requested
    } else {
        workspace_dir.join(requested)
    };

    let canonical_workspace = std::fs::canonicalize(workspace_dir)
        .map_err(|e| format!("Failed to resolve workspace path: {e}"))?;

    let canonical_candidate = match std::fs::canonicalize(&candidate) {
        Ok(path) => path,
        Err(_) => {
            let parent = candidate
                .parent()
                .ok_or_else(|| format!("Invalid path (no parent): {}", candidate.display()))?;
            let canonical_parent = std::fs::canonicalize(parent)
                .map_err(|_| format!("Parent directory does not exist: {}", parent.display()))?;
            if !canonical_parent.starts_with(&canonical_workspace) {
                return Err(format!(
                    "Refusing to access path outside workspace: {}",
                    candidate.display()
                ));
            }
            let file_name = candidate
                .file_name()
                .ok_or_else(|| format!("Invalid path (no file name): {}", candidate.display()))?;
            return Ok(canonical_parent.join(file_name));
        }
    };

    if !canonical_candidate.starts_with(&canonical_workspace) {
        return Err(format!(
            "Refusing to access path outside workspace: {}",
            canonical_candidate.display()
        ));
    }

    Ok(canonical_candidate)
}

fn truncation_json(truncation: &TruncationResult) -> serde_json::Value {
    json!({
        "truncated": truncation.truncated,
        "truncatedBy": truncation.truncated_by,
        "totalLines": truncation.total_lines,
        "totalBytes": truncation.total_bytes,
        "outputLines": truncation.output_lines,
        "outputBytes": truncation.output_bytes,
        "lastLinePartial": truncation.last_line_partial,
        "firstLineExceedsLimit": truncation.first_line_exceeds_limit,
        "maxLines": truncation.max_lines,
        "maxBytes": truncation.max_bytes
    })
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
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            max_session_messages: 40,
            compact_keep_recent: 20,
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
    }

    #[tokio::test]
    #[serial]
    async fn read_reports_supported_images() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir();
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(
            workspace.join("pixel.png"),
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        )
        .expect("png");
        let registry = test_registry(&config);

        let result = registry
            .execute("read", json!({"path": "pixel.png"}), &test_context())
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("Read image file [image/png]"));
        match result.llm_content {
            crate::llm::MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(
                    &parts[1],
                    crate::llm::MessageContentPart::InputImage { .. }
                ));
            }
            other => panic!("expected multimodal llm_content, got {other:?}"),
        }
    }

    #[tokio::test]
    #[serial]
    async fn write_creates_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let registry = test_registry(&config);

        std::fs::create_dir_all(config.workspace_dir().join("src")).expect("create src dir");

        let result = registry
            .execute(
                "write",
                json!({"path": "src/demo.txt", "content": "hello world"}),
                &test_context(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("Successfully wrote 11 bytes"));
        assert_eq!(
            std::fs::read_to_string(config.workspace_dir().join("src/demo.txt")).expect("read"),
            "hello world"
        );
    }

    #[tokio::test]
    #[serial]
    async fn edit_replaces_exact_match_and_returns_diff() {
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
        assert!(!result.is_error, "{}", result.content);
        let content = std::fs::read_to_string(workspace.join("notes.txt")).expect("read");
        assert!(content.contains("delta"));
        assert_eq!(
            result
                .details
                .as_ref()
                .and_then(|details| details.get("firstChangedLine"))
                .and_then(|value| value.as_u64()),
            Some(2)
        );
        assert!(
            result
                .details
                .as_ref()
                .and_then(|details| details.get("diff"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .contains("-beta")
        );
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
        let bash_temp_dir = workspace.join(".tmp").join("bash");
        assert!(bash_temp_dir.is_dir());
        assert_eq!(
            std::fs::read_dir(&bash_temp_dir)
                .expect("bash temp dir entries")
                .count(),
            0
        );
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

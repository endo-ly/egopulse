//! ファイル操作ツール群 — read / write / edit。

use std::cmp::min;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine;
use serde_json::json;

use crate::llm::{MessageContent, MessageContentPart, ToolDefinition};

use super::path_guard;
use super::search::resolve_workspace_path;
use super::text::{
    EditSpec, apply_edits_to_normalized_content, detect_line_ending, first_changed_line,
    format_size, generate_diff_string, normalize_newlines, restore_line_endings, strip_bom,
    truncate_head,
};
use super::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, Tool, ToolExecutionContext, ToolResult, schema_object,
};

// ---------------------------------------------------------------------------
// ユーティリティ
// ---------------------------------------------------------------------------

pub(crate) fn detect_supported_image_mime_type(bytes: &[u8], path: &Path) -> Option<&'static str> {
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
pub(crate) struct ReadTool {
    pub(super) workspace_dir: PathBuf,
}

impl ReadTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
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
        if let Err(reason) = path_guard::check_path(path) {
            return ToolResult::error(reason);
        }

        let bytes = match tokio::fs::read(&resolved).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("File not found: {path}"));
            }
            Err(error) => return ToolResult::error(format!("Failed to read file: {error}")),
        };

        if let Some(mime_type) = detect_supported_image_mime_type(&bytes, &resolved) {
            return image_read_result(&bytes, mime_type);
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

        let normalized = normalize_newlines(&content);
        let all_lines = normalized.split('\n').collect::<Vec<_>>();
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

        build_text_read_result(path, &all_lines, start_line, user_limit, &selected_content)
    }
}

/// Writes content to a file, creating parent directories as needed.
pub(crate) struct WriteTool {
    pub(super) workspace_dir: PathBuf,
}

impl WriteTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
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
        if let Err(reason) = path_guard::check_path(path) {
            return ToolResult::error(reason);
        }
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
pub(crate) struct EditTool {
    pub(super) workspace_dir: PathBuf,
}

impl EditTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
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
        if let Err(reason) = path_guard::check_path(path) {
            return ToolResult::error(reason);
        }

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

pub(crate) fn parse_edits(input: &serde_json::Value) -> Result<Vec<EditSpec>, String> {
    let has_edits_array = input
        .get("edits")
        .and_then(|value| value.as_array())
        .is_some();
    let has_legacy_fields = input
        .get("oldText")
        .and_then(|value| value.as_str())
        .is_some()
        || input
            .get("newText")
            .and_then(|value| value.as_str())
            .is_some();

    if has_edits_array && has_legacy_fields {
        return Err(
            "Edit tool input is ambiguous: provide either 'edits' array or 'oldText'/'newText' pair, not both.".to_string(),
        );
    }

    let mut parsed = Vec::new();
    if let Some(edits) = input.get("edits").and_then(|value| value.as_array()) {
        for edit in edits {
            parsed.push(parse_edit_spec(edit)?);
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

fn image_read_result(bytes: &[u8], mime_type: &str) -> ToolResult {
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
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    ToolResult::success_with_llm_content(
        preview.clone(),
        MessageContent::parts(vec![
            MessageContentPart::InputText { text: preview },
            MessageContentPart::InputImage {
                image_url: data_url,
                detail: Some("auto".to_string()),
            },
        ]),
    )
}

fn build_text_read_result(
    path: &str,
    all_lines: &[&str],
    start_line: usize,
    user_limit: Option<usize>,
    selected_content: &str,
) -> ToolResult {
    let truncation = truncate_head(selected_content, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
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
            super::text::shell_quote(path),
            DEFAULT_MAX_BYTES
        )
    } else {
        truncation.content.clone()
    };

    if truncation.truncated && !truncation.first_line_exceeds_limit {
        append_truncation_notice(&mut output, &truncation, start_display, total_lines);
        return ToolResult::success_with_details(
            output,
            json!({
                "truncation": super::truncation_json(&truncation)
            }),
        );
    }

    if let Some(limit) = user_limit.filter(|limit| start_line + limit < all_lines.len()) {
        let remaining = all_lines.len() - (start_line + limit);
        let next_offset = start_line + limit + 1;
        output.push_str(&format!(
            "\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]"
        ));
    }

    ToolResult::success(output)
}

fn append_truncation_notice(
    output: &mut String,
    truncation: &super::text::TruncationResult,
    start_display: usize,
    total_lines: usize,
) {
    let end_display = start_display + truncation.output_lines.saturating_sub(1);
    let next_offset = end_display + 1;
    if truncation.truncated_by == Some("lines") {
        output.push_str(&format!(
            "\n\n[Showing lines {start_display}-{end_display} of {total_lines}. Use offset={next_offset} to continue.]"
        ));
        return;
    }

    output.push_str(&format!(
        "\n\n[Showing lines {start_display}-{end_display} of {total_lines} ({} limit). Use offset={next_offset} to continue.]",
        format_size(DEFAULT_MAX_BYTES)
    ));
}

fn parse_edit_spec(edit: &serde_json::Value) -> Result<EditSpec, String> {
    let Some(old_text) = edit.get("oldText").and_then(|value| value.as_str()) else {
        return Err("Each edit must include oldText".to_string());
    };
    let Some(new_text) = edit.get("newText").and_then(|value| value.as_str()) else {
        return Err("Each edit must include newText".to_string());
    };

    Ok(EditSpec {
        old_text: old_text.to_string(),
        new_text: new_text.to_string(),
    })
}

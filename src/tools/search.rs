//! 検索ツール群 — grep / find / ls。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::llm::ToolDefinition;

use super::text::{format_size, truncate_head};
use super::{
    DEFAULT_FIND_LIMIT, DEFAULT_GREP_LIMIT, DEFAULT_GREP_TIMEOUT_SECS, DEFAULT_LS_LIMIT,
    DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH, Tool, ToolExecutionContext, ToolResult, schema_object,
};

pub(crate) fn resolve_workspace_path(
    workspace_dir: &Path,
    requested_path: &str,
) -> Result<PathBuf, String> {
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

/// grep パターンの長さおよび括弧のネスト深さを検証する。
///
/// ReDoS（Regular Expression Denial of Service）攻撃を防止するため、
/// パターン長は 1024 文字まで、開き括弧の連続ネスト深さは 10 までに制限する。
fn validate_grep_pattern(pattern: &str, literal: bool) -> Result<(), String> {
    const MAX_PATTERN_LEN: usize = 1024;
    const MAX_NESTING_DEPTH: usize = 10;

    let char_count = pattern.chars().count();
    if char_count > MAX_PATTERN_LEN {
        return Err(format!(
            "Pattern too long: {char_count} chars (max {MAX_PATTERN_LEN}). Use a shorter pattern or enable literal mode."
        ));
    }

    if !literal {
        let mut depth = 0usize;
        let mut in_char_class = false;
        let mut escaped = false;
        for ch in pattern.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '[' if !in_char_class => in_char_class = true,
                ']' if in_char_class => in_char_class = false,
                '(' if !in_char_class => {
                    depth += 1;
                    if depth > MAX_NESTING_DEPTH {
                        return Err(format!(
                            "Pattern nesting too deep: exceeds {MAX_NESTING_DEPTH} levels. Simplify the pattern or enable literal mode."
                        ));
                    }
                }
                ')' if !in_char_class => {
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// タイムアウト時にプロセスグループへ SIGKILL を送信し、終了を待機する。
async fn kill_on_timeout(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let ret = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        if ret != 0 {
            let _ = child.start_kill();
        }
    } else {
        let _ = child.start_kill();
    }
    let _ = child.wait().await;
}

/// Searches file contents via ripgrep with regex/literal and context line support.
pub(crate) struct GrepTool {
    pub(super) workspace_dir: PathBuf,
}

impl GrepTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
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
        let literal = input
            .get("literal")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if let Err(error) = validate_grep_pattern(pattern, literal) {
            return ToolResult::error(error);
        }
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
        let context_lines = input
            .get("context")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(0);
        let file_glob = input.get("glob").and_then(|value| value.as_str());

        let (cwd, target) = command_scope_for_path(&resolved);
        let mut command = Command::new("rg");
        command.process_group(0).kill_on_drop(true);
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
            .arg("-e")
            .arg(pattern)
            .arg("--")
            .arg(target)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(
                    "ripgrep (rg) is not available and could not be downloaded".to_string(),
                );
            }
            Err(error) => return ToolResult::error(format!("Failed to run ripgrep: {error}")),
        };

        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();

        let read_limit = limit;
        let stdout_fut = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut line_count = 0usize;
            let mut exceeded = false;
            if let Some(ref mut stdout) = stdout_pipe {
                let mut tmp = [0u8; 8192];
                loop {
                    match AsyncReadExt::read(stdout, &mut tmp).await {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            line_count += tmp[..n].iter().filter(|&&b| b == b'\n').count();
                            if line_count > read_limit + 50 || buf.len() > DEFAULT_MAX_BYTES * 2 {
                                exceeded = true;
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
            (buf, exceeded)
        });
        let stderr_fut = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(ref mut stderr) = stderr_pipe {
                let _ = AsyncReadExt::read_to_end(stderr, &mut buf).await;
            }
            buf
        });

        let wait_result =
            timeout(Duration::from_secs(DEFAULT_GREP_TIMEOUT_SECS), child.wait()).await;

        match wait_result {
            Ok(Ok(status)) => {
                let (stdout_bytes, stdout_exceeded) = stdout_fut.await.unwrap_or_default();
                let stderr_bytes = stderr_fut.await.unwrap_or_default();
                if stdout_exceeded {
                    let _ = child.start_kill();
                }
                let stdout = String::from_utf8_lossy(&stdout_bytes).replace("\r\n", "\n");
                let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_string();

                if stdout.trim().is_empty() && status.code() == Some(1) {
                    return ToolResult::success("No matches found".to_string());
                }
                if !status.success() && status.code() != Some(1) {
                    return ToolResult::error(if stderr.is_empty() {
                        format!("ripgrep exited with code {}", status.code().unwrap_or(-1))
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
                            "truncation": if truncation.truncated { Some(super::truncation_json(&truncation)) } else { None::<serde_json::Value> },
                            "matchLimitReached": if result_limit_reached { Some(limit) } else { None::<usize> },
                            "linesTruncated": lines_truncated
                        }),
                    );
                }

                ToolResult::success(text)
            }
            Ok(Err(error)) => {
                let _ = stdout_fut.await;
                let _ = stderr_fut.await;
                ToolResult::error(format!("Failed to run ripgrep: {error}"))
            }
            Err(_) => {
                kill_on_timeout(&mut child).await;
                let _ = stdout_fut.await;
                let _ = stderr_fut.await;
                ToolResult::error(format!(
                    "Grep timed out after {DEFAULT_GREP_TIMEOUT_SECS}s. Try a simpler pattern or narrow the search path."
                ))
            }
        }
    }
}

/// Finds files by glob pattern via fd, respecting .gitignore.
pub(crate) struct FindTool {
    pub(super) workspace_dir: PathBuf,
}

impl FindTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
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
            .map(|value| value.max(1) as usize)
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
            .arg("--type")
            .arg("f")
            .arg("--max-results")
            .arg((limit + 1).to_string());
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
            .arg("--")
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

        let mut results = stdout
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

        let result_limit_reached = results.len() > limit;
        if result_limit_reached {
            results.truncate(limit);
        }
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
                    "truncation": if truncation.truncated { Some(super::truncation_json(&truncation)) } else { None::<serde_json::Value> },
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
                if should_skip_gitignore_dir(&path) {
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

fn should_skip_gitignore_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some(".git") | Some("node_modules")
    )
}

/// Lists directory entries alphabetically with `/` suffix for subdirectories.
pub(crate) struct LsTool {
    pub(super) workspace_dir: PathBuf,
}

impl LsTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
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
            .map(|value| value.max(1) as usize)
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
        let output_truncated = truncate_head(&raw_output, usize::MAX, DEFAULT_MAX_BYTES).truncated;
        let mut notices = Vec::new();
        if entry_limit_reached {
            notices.push(format!(
                "{limit} entries limit reached. Use limit={} for more",
                limit * 2
            ));
        }
        if output_truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        }
        finalize_listing_output(
            raw_output,
            notices,
            "entryLimitReached",
            entry_limit_reached.then_some(limit),
        )
    }
}

fn finalize_listing_output(
    raw_output: String,
    notices: Vec<String>,
    limit_key: &str,
    limit_value: Option<usize>,
) -> ToolResult {
    let truncation = truncate_head(&raw_output, usize::MAX, DEFAULT_MAX_BYTES);
    let mut text = truncation.content.clone();

    if notices.is_empty() {
        return ToolResult::success(text);
    }

    text.push_str(&format!("\n\n[{}]", notices.join(". ")));
    ToolResult::success_with_details(
        text,
        json!({
            "truncation": truncation.truncated.then(|| super::truncation_json(&truncation)),
            limit_key: limit_value,
        }),
    )
}

pub(crate) fn truncate_grep_line(line: &str) -> (String, bool) {
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

pub(crate) fn command_scope_for_path(path: &Path) -> (&Path, &str) {
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

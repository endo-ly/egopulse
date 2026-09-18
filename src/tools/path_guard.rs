//! ファイルパスのセキュリティガード。
//!
//! 機密情報を含むパス（.ssh, .aws, .env など）へのアクセスをブロックする。

use std::path::{Component, Path, PathBuf};

const BLOCKED_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg", ".kube"];

const BLOCKED_SUBPATHS: &[&[&str]] = &[&[".config", "gcloud"]];

const BLOCKED_FILE_NAMES: &[&str] = &[
    "auth.json",
    "credentials",
    "credentials.json",
    "token.json",
    "secrets.yaml",
    "secrets.json",
    "id_rsa",
    "id_rsa.pub",
    "id_ed25519",
    "id_ed25519.pub",
    "id_ecdsa",
    "id_ecdsa.pub",
    "id_dsa",
    "id_dsa.pub",
    ".netrc",
    ".npmrc",
];

const BLOCKED_ABSOLUTE: &[&str] = &[
    "/etc/shadow",
    "/etc/gshadow",
    "/etc/sudoers",
    "/proc/self/environ",
    "/proc/self/mem",
    "/proc/self/maps",
    "/proc/self/cmdline",
    "/proc/self/status",
    "/proc/self/mountinfo",
];

pub(crate) fn blocked_ripgrep_exclude_globs() -> Vec<String> {
    let mut globs = Vec::with_capacity(
        BLOCKED_DIRS.len() * 2 + BLOCKED_FILE_NAMES.len() * 2 + BLOCKED_SUBPATHS.len() * 2 + 2,
    );

    for dir in BLOCKED_DIRS {
        globs.push(format!("!{dir}/**"));
        globs.push(format!("!**/{dir}/**"));
    }
    globs.push("!.env*".to_string());
    globs.push("!**/.env*".to_string());

    for file in BLOCKED_FILE_NAMES {
        globs.push(format!("!{file}"));
        globs.push(format!("!**/{file}"));
    }
    for subpath in BLOCKED_SUBPATHS {
        let joined = subpath.join("/");
        globs.push(format!("!{joined}/**"));
        globs.push(format!("!**/{joined}/**"));
    }

    globs
}

pub(crate) fn blocked_fd_exclude_patterns() -> Vec<String> {
    let mut patterns = Vec::with_capacity(
        BLOCKED_DIRS.len() * 4 + BLOCKED_FILE_NAMES.len() * 2 + BLOCKED_SUBPATHS.len() * 4 + 2,
    );

    for dir in BLOCKED_DIRS {
        patterns.push((*dir).to_string());
        patterns.push(format!("**/{dir}"));
        patterns.push(format!("{dir}/**"));
        patterns.push(format!("**/{dir}/**"));
    }
    patterns.push(".env*".to_string());
    patterns.push("**/.env*".to_string());

    for file in BLOCKED_FILE_NAMES {
        patterns.push((*file).to_string());
        patterns.push(format!("**/{file}"));
    }
    for subpath in BLOCKED_SUBPATHS {
        let joined = subpath.join("/");
        patterns.push(joined.clone());
        patterns.push(format!("**/{joined}"));
        patterns.push(format!("{joined}/**"));
        patterns.push(format!("**/{joined}/**"));
    }

    patterns
}

/// パスがセキュリティポリシーでブロックされるか検査する。
pub(crate) fn check_path(path: &str) -> Result<(), String> {
    let candidate = Path::new(path);
    validate_symlink_safety(candidate)?;
    if is_blocked(candidate) {
        return Err(format!(
            "Access denied: '{path}' is a sensitive path and cannot be accessed."
        ));
    }
    Ok(())
}

/// コマンド文字列内に含まれるパス参照がブロック対象か検査する。
/// `cat /home/user/.ssh/id_rsa` や `cat .env` などを検知する。
pub(crate) fn check_command_paths(command: &str) -> Result<(), String> {
    let normalized_command = normalize_command_path_like(command);
    for blocked in BLOCKED_ABSOLUTE {
        if contains_path_reference(&normalized_command, blocked) {
            return Err(format!(
                "Access denied: command references blocked path '{blocked}'."
            ));
        }
    }
    check_proc_access(&normalized_command)?;
    check_blocked_files_in_command(&normalized_command)?;
    check_blocked_dirs_in_command(&normalized_command)?;
    check_blocked_subpaths_in_command(&normalized_command)?;
    Ok(())
}

fn check_blocked_files_in_command(command: &str) -> Result<(), String> {
    for token in command_word_tokens(command) {
        for component in token.split('/') {
            if component.is_empty() {
                continue;
            }
            if is_blocked_filename(component) {
                return Err(format!(
                    "Access denied: command references blocked file '{component}'. \
                     Sensitive files cannot be accessed through shell commands."
                ));
            }
        }
    }
    Ok(())
}

fn check_blocked_dirs_in_command(lower: &str) -> Result<(), String> {
    for blocked in BLOCKED_DIRS {
        if contains_path_component(lower, blocked) {
            return Err(format!(
                "Access denied: command references blocked directory '{blocked}'."
            ));
        }
    }
    Ok(())
}

fn check_blocked_subpaths_in_command(lower: &str) -> Result<(), String> {
    for blocked in BLOCKED_SUBPATHS {
        let subpath = blocked.join("/");
        if contains_path_reference(lower, &subpath) {
            return Err(format!(
                "Access denied: command references blocked path segment '{subpath}'."
            ));
        }
    }
    Ok(())
}

fn contains_path_component(lower: &str, component: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut start = 0usize;
    while let Some(offset) = lower[start..].find(component) {
        let abs = start + offset;
        let end = abs + component.len();
        let preceded = if abs == 0 {
            true
        } else {
            is_path_prefix_boundary(bytes[abs - 1])
        };
        let followed = end >= bytes.len() || is_path_suffix_boundary(bytes[end]);
        if preceded && followed {
            return true;
        }
        start = abs + 1;
        if start >= lower.len() {
            break;
        }
    }
    false
}

fn contains_path_reference(lower: &str, needle: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut start = 0usize;
    while let Some(offset) = lower[start..].find(needle) {
        let abs = start + offset;
        let end = abs + needle.len();
        let preceded = abs == 0 || is_path_prefix_boundary(bytes[abs - 1]);
        let followed = end >= bytes.len() || is_path_suffix_boundary(bytes[end]);
        if preceded && followed {
            return true;
        }
        start = abs + 1;
        if start >= lower.len() {
            break;
        }
    }
    false
}

fn is_path_prefix_boundary(byte: u8) -> bool {
    matches!(
        byte,
        b'/' | b'\\'
            | b' '
            | b'\t'
            | b'\n'
            | b'\''
            | b'"'
            | b'`'
            | b';'
            | b'|'
            | b'&'
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'='
            | b':'
            | b','
    )
}

fn is_path_suffix_boundary(byte: u8) -> bool {
    if is_path_prefix_boundary(byte) {
        return true;
    }
    !matches!(
        byte,
        b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'
    )
}

/// `/proc/self/*` `/proc/<pid>/*` へのアクセスを包括的にブロックする。
fn check_proc_access(lower: &str) -> Result<(), String> {
    let mut start = 0usize;
    while let Some(offset) = lower[start..].find("/proc/") {
        let abs = start + offset;
        let after = &lower[abs + "/proc/".len()..];
        let segment = after.split('/').next().unwrap_or("");
        if segment == "self" || (!segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()))
        {
            return Err(
                "Access denied: command references /proc/*/..., which exposes process internals."
                    .to_string(),
            );
        }
        start = abs + 1;
        if start >= lower.len() {
            break;
        }
    }
    Ok(())
}

fn normalize_command_path_like(command: &str) -> String {
    let mut normalized = String::with_capacity(command.len());
    let mut prev_slash = false;

    for ch in command.chars() {
        let mapped = match ch {
            '\\' => '/',
            _ => ch.to_ascii_lowercase(),
        };

        if mapped == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }

        normalized.push(mapped);
    }

    normalized
}

fn command_word_tokens(command: &str) -> Vec<&str> {
    command
        .split(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    ';' | '|' | '&' | '(' | ')' | '\'' | '"' | '`' | ',' | '=' | ':'
                )
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn is_blocked_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with(".env") {
        return true;
    }
    BLOCKED_FILE_NAMES.contains(&lower.as_str())
}

pub(crate) fn is_blocked(path: &Path) -> bool {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| {
        let abs = if path.is_relative() {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        } else {
            path.to_path_buf()
        };
        normalize_path(&abs)
    });

    let original_str = path.to_string_lossy();
    let resolved_str = resolved.to_string_lossy();
    for blocked in BLOCKED_ABSOLUTE {
        if original_str == *blocked || resolved_str == *blocked {
            return true;
        }
    }

    if is_proc_path(&original_str) || is_proc_path(&resolved_str) {
        return true;
    }

    let components: Vec<String> = resolved
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();

    for component in &components {
        if BLOCKED_DIRS.contains(&component.as_str()) {
            return true;
        }
        if is_blocked_filename(component) {
            return true;
        }
    }

    for subpath in BLOCKED_SUBPATHS {
        if subpath.len() <= components.len() {
            for window in components.windows(subpath.len()) {
                let matches = window
                    .iter()
                    .zip(subpath.iter())
                    .all(|(a, b)| a.as_str() == *b);
                if matches {
                    return true;
                }
            }
        }
    }

    false
}

fn is_proc_path(path: &str) -> bool {
    if let Some(after) = path.strip_prefix("/proc/") {
        let segment = after.split('/').next().unwrap_or("");
        segment == "self" || (!segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()))
    } else {
        false
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else if matches!(parts.last(), Some(Component::RootDir)) {
                } else {
                    parts.push(component);
                }
            }
            _ => parts.push(component),
        }
    }
    parts.iter().collect()
}

fn validate_symlink_safety(path: &Path) -> Result<(), String> {
    let mut cur = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => {
                cur.push(Path::new("/"));
            }
            Component::Prefix(prefix) => {
                cur.push(prefix.as_os_str());
            }
            Component::Normal(part) => {
                cur.push(part);
                if !cur.exists() {
                    continue;
                }
                let meta = std::fs::symlink_metadata(&cur).map_err(|e| {
                    format!("failed to inspect path component '{}': {e}", cur.display())
                })?;
                if meta.file_type().is_symlink() {
                    if cur == Path::new("/tmp") || cur == Path::new("/var") {
                        continue;
                    }
                    return Err(format!("symlink component detected at '{}'", cur.display()));
                }
            }
            Component::CurDir | Component::ParentDir => {}
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn filter_paths(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|p| !is_blocked(Path::new(p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_path_matrix_covers_sensitive_locations_and_traversal() {
        let blocked = [
            "/home/user/.ssh/id_rsa",
            "/home/user/.ssh/config",
            "/home/user/.aws/credentials",
            "/home/user/.gnupg/private-keys-v1.d",
            "/home/user/.kube/config",
            "/home/user/.config/gcloud/credentials.db",
            "/project/.env",
            "/project/.env.local",
            "/project/.env.production",
            "/project/.env.development",
            "/project/.env.test",
            "/project/.env.staging",
            "/project/.envrc",
            "/project/credentials.json",
            "/project/token.json",
            "/project/secrets.yaml",
            "/project/secrets.json",
            "/home/user/.codex/auth.json",
            "/root/.codex/auth.json",
            "/project/auth.json",
            "/home/user/id_rsa",
            "/home/user/id_ed25519",
            "/proc/self/environ",
            "/proc/self/mem",
            "/proc/self/maps",
            "/proc/self/cmdline",
            "/proc/self/fd/3",
            "/proc/1/environ",
            "/proc/123/mem",
            "/tmp/../etc/shadow",
            "/home/user/project/../../.ssh/id_rsa",
        ];

        for path in blocked {
            assert!(is_blocked(Path::new(path)), "should block: {path}");
        }
    }

    #[test]
    fn allowed_path_matrix_avoids_proc_and_file_false_positives() {
        let allowed = [
            "/proc/cpuinfo",
            "/proc/meminfo",
            "/proc/",
            "/home/user/project/main.rs",
            "/tmp/test.txt",
            "src/config.rs",
        ];

        for path in allowed {
            assert!(!is_blocked(Path::new(path)), "should allow: {path}");
        }
    }

    #[test]
    fn check_path_returns_error_for_blocked() {
        let result = check_path("/home/user/.ssh/id_rsa");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Access denied"));
    }

    #[test]
    fn filter_paths_removes_blocked() {
        let paths = vec![
            "src/main.rs".to_string(),
            "/home/user/.ssh/id_rsa".to_string(),
            "README.md".to_string(),
            "/project/.env".to_string(),
        ];
        let filtered = filter_paths(paths);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0], "src/main.rs");
        assert_eq!(filtered[1], "README.md");
    }

    #[test]
    fn command_path_guard_preserves_blocked_and_allowed_boundaries() {
        let cases = [
            ("find ~/.ssh -type f -exec cat {} +", false),
            ("tar czf - /home/user/.aws", false),
            ("tar czf - ~/.config/gcloud", false),
            ("cat /home/user/.config/gcloud/credentials.db", false),
            ("cat .env.test", false),
            ("cat ./config/.env.staging", false),
            ("cat .envrc", false),
            ("cat /etc//shadow", false),
            ("cat /proc//self/environ", false),
            ("cat /proc/", true),
            ("cat /etc/shadow.bak", true),
            ("echo credentials-json", true),
        ];

        for (command, allowed) in cases {
            assert_eq!(
                check_command_paths(command).is_ok(),
                allowed,
                "command: {command}"
            );
        }
    }

    #[test]
    fn search_exclude_patterns_include_sensitive_targets() {
        let globs = blocked_ripgrep_exclude_globs();
        assert!(globs.iter().any(|g| g == "!**/.ssh/**"));
        assert!(globs.iter().any(|g| g == "!**/.env*"));
        assert!(globs.iter().any(|g| g == "!**/.config/gcloud/**"));

        let patterns = blocked_fd_exclude_patterns();
        assert!(patterns.iter().any(|p| p == ".ssh"));
        assert!(patterns.iter().any(|p| p == "**/.env*"));
        assert!(patterns.iter().any(|p| p == "**/.config/gcloud/**"));
    }
}

//! ファイルパスのセキュリティガード。
//!
//! 機密情報を含むパス（.ssh, .aws, .env など）へのアクセスをブロックする。
//! MicroClaw の path_guard.rs をベースに EgoPulse 向けに調整。

use std::path::{Component, Path, PathBuf};

const BLOCKED_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg", ".kube"];

const BLOCKED_SUBPATHS: &[&[&str]] = &[&[".config", "gcloud"]];

const BLOCKED_FILES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    ".env.development",
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
    let lower = command.to_ascii_lowercase();
    for blocked in BLOCKED_ABSOLUTE {
        if lower.contains(blocked) {
            return Err(format!(
                "Access denied: command references blocked path '{blocked}'."
            ));
        }
    }
    check_proc_access(&lower)?;
    for blocked in BLOCKED_FILES {
        if lower.contains(blocked) {
            return Err(format!(
                "Access denied: command references blocked file '{blocked}'. \
                 Sensitive files cannot be accessed through shell commands."
            ));
        }
    }
    Ok(())
}

/// `/proc/self/*` `/proc/<pid>/*` へのアクセスを包括的にブロックする。
fn check_proc_access(lower: &str) -> Result<(), String> {
    let mut start = 0usize;
    while let Some(offset) = lower[start..].find("/proc/") {
        let abs = start + offset;
        let after = &lower[abs + "/proc/".len()..];
        let segment = after.split('/').next().unwrap_or("");
        if segment == "self" || segment.chars().all(|c| c.is_ascii_digit()) {
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
        if BLOCKED_FILES.contains(&component.as_str()) {
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
        segment == "self" || segment.chars().all(|c| c.is_ascii_digit())
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
    fn blocks_ssh_directory() {
        assert!(is_blocked(Path::new("/home/user/.ssh/id_rsa")));
        assert!(is_blocked(Path::new("/home/user/.ssh/config")));
    }

    #[test]
    fn blocks_aws_directory() {
        assert!(is_blocked(Path::new("/home/user/.aws/credentials")));
    }

    #[test]
    fn blocks_gnupg_directory() {
        assert!(is_blocked(Path::new("/home/user/.gnupg/private-keys-v1.d")));
    }

    #[test]
    fn blocks_kube_directory() {
        assert!(is_blocked(Path::new("/home/user/.kube/config")));
    }

    #[test]
    fn blocks_gcloud_config() {
        assert!(is_blocked(Path::new(
            "/home/user/.config/gcloud/credentials.db"
        )));
    }

    #[test]
    fn blocks_env_files() {
        assert!(is_blocked(Path::new("/project/.env")));
        assert!(is_blocked(Path::new("/project/.env.local")));
        assert!(is_blocked(Path::new("/project/.env.production")));
        assert!(is_blocked(Path::new("/project/.env.development")));
    }

    #[test]
    fn blocks_credential_files() {
        assert!(is_blocked(Path::new("/project/credentials.json")));
        assert!(is_blocked(Path::new("/project/token.json")));
        assert!(is_blocked(Path::new("/project/secrets.yaml")));
        assert!(is_blocked(Path::new("/project/secrets.json")));
    }

    #[test]
    fn blocks_ssh_keys() {
        assert!(is_blocked(Path::new("/home/user/id_rsa")));
        assert!(is_blocked(Path::new("/home/user/id_ed25519")));
    }

    #[test]
    fn blocks_proc_self_environ() {
        assert!(is_blocked(Path::new("/proc/self/environ")));
    }

    #[test]
    fn blocks_proc_self_mem() {
        assert!(is_blocked(Path::new("/proc/self/mem")));
        assert!(is_blocked(Path::new("/proc/self/maps")));
        assert!(is_blocked(Path::new("/proc/self/cmdline")));
        assert!(is_blocked(Path::new("/proc/self/fd/3")));
    }

    #[test]
    fn blocks_proc_pid_paths() {
        assert!(is_blocked(Path::new("/proc/1/environ")));
        assert!(is_blocked(Path::new("/proc/123/mem")));
    }

    #[test]
    fn allows_proc_non_numeric() {
        // /proc/cpuinfo 等の数値以外はプロセス情報ではないため許可
        assert!(!is_blocked(Path::new("/proc/cpuinfo")));
        assert!(!is_blocked(Path::new("/proc/meminfo")));
    }

    #[test]
    fn allows_normal_files() {
        assert!(!is_blocked(Path::new("/home/user/project/main.rs")));
        assert!(!is_blocked(Path::new("/tmp/test.txt")));
        assert!(!is_blocked(Path::new("src/config.rs")));
    }

    #[test]
    fn blocks_traversal_via_parent_dir() {
        assert!(is_blocked(Path::new("/tmp/../etc/shadow")));
        assert!(is_blocked(Path::new(
            "/home/user/project/../../.ssh/id_rsa"
        )));
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
}

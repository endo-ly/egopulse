//! スキルの発見・読み込み・カタログ生成。
//!
//! ワークスペース配下の `SKILL.md` を再帰的に走査し、frontmatter から
//! メタデータを抽出して利用可能スキルとして登録する。プロンプト予算に応じた
//! コンパクトモードでのカタログ出力も行う。

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Parsed skill metadata extracted from a SKILL.md frontmatter block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub dir_path: PathBuf,
    pub platforms: Vec<String>,
    pub deps: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    deps: Vec<String>,
}

/// A fully loaded skill with both metadata and the instruction body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSkill {
    pub metadata: SkillMetadata,
    pub instructions: String,
}

/// Discovers, validates, and loads skills from the workspace skill directories.
#[derive(Debug, Clone)]
pub struct SkillManager {
    user_skills_dir: PathBuf,
    builtin_skills_dir: PathBuf,
}

const MAX_SKILLS_CATALOG_ITEMS: usize = 40;
const MAX_SKILL_DESCRIPTION_CHARS: usize = 120;
const COMPACT_SKILLS_MODE_THRESHOLD: usize = 20;

impl SkillManager {
    /// Create a manager scanning both user and built-in skill directories.
    pub fn from_dirs(
        user_skills_dir: impl Into<PathBuf>,
        builtin_skills_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            user_skills_dir: user_skills_dir.into(),
            builtin_skills_dir: builtin_skills_dir.into(),
        }
    }

    /// Backward-compatible constructor using a single skills directory.
    pub fn from_skills_dir(skills_dir: impl Into<PathBuf>) -> Self {
        let dir = skills_dir.into();
        let builtin = dir
            .parent()
            .and_then(|p| p.parent())
            .map(|root| root.join("skills"))
            .unwrap_or_else(|| dir.clone());
        Self {
            user_skills_dir: dir.clone(),
            builtin_skills_dir: builtin,
        }
    }

    pub fn skills_dir(&self) -> &Path {
        &self.user_skills_dir
    }

    /// Scan the workspace for available skills, filtering by platform and dependency availability.
    pub fn discover_skills(&self) -> Vec<SkillMetadata> {
        let mut skills_by_name = BTreeMap::new();
        for candidate in self.discover_skill_dirs() {
            let skill_md = candidate.join("SKILL.md");
            let Ok(content) = std::fs::read_to_string(&skill_md) else {
                continue;
            };
            let Some((meta, _body)) = parse_skill_md(&content, &candidate) else {
                continue;
            };
            if self.skill_is_available(&meta) {
                skills_by_name.entry(meta.name.clone()).or_insert(meta);
            }
        }

        skills_by_name.into_values().collect()
    }

    /// Load a skill by name. Returns an error listing available skills if not found.
    pub fn load_skill_checked(&self, name: &str) -> Result<LoadedSkill, String> {
        let mut available_names = Vec::new();

        for meta in self.discover_skills() {
            available_names.push(meta.name.clone());
            if meta.name != name {
                continue;
            }
            let skill_md = meta.dir_path.join("SKILL.md");
            let content = std::fs::read_to_string(&skill_md)
                .map_err(|error| format!("failed to read skill '{name}': {error}"))?;
            let Some((_meta, body)) = parse_skill_md(&content, &meta.dir_path) else {
                return Err(format!("skill '{name}' exists but could not be parsed"));
            };
            return Ok(LoadedSkill {
                metadata: meta,
                instructions: body,
            });
        }

        if available_names.is_empty() {
            Err(format!(
                "Skill '{name}' not found. No skills are currently available."
            ))
        } else {
            Err(format!(
                "Skill '{name}' not found. Available skills: {}",
                available_names.join(", ")
            ))
        }
    }

    /// チャット表示用にプレーンテキストでスキル一覧を返す。
    pub fn list_skills_formatted(&self) -> String {
        let skills = self.discover_skills();
        if skills.is_empty() {
            return "No skills loaded.".to_string();
        }

        let mut out = String::from("Available skills:\n");
        for skill in &skills {
            out.push_str(&format!("- {} ({})\n", skill.name, skill.description));
        }
        out.pop();
        out
    }

    /// Build an XML-formatted skills catalog for injection into the system prompt.
    /// Switches to compact mode (name-only) when skill count exceeds threshold.
    pub fn build_skills_catalog(&self) -> String {
        let mut skills = self.discover_skills();
        if skills.is_empty() {
            return String::new();
        }

        skills.sort_by_key(|skill| skill.name.to_ascii_lowercase());
        let total = skills.len();
        let omitted = total.saturating_sub(MAX_SKILLS_CATALOG_ITEMS);
        let visible = skills
            .into_iter()
            .take(MAX_SKILLS_CATALOG_ITEMS)
            .collect::<Vec<_>>();
        let compact_mode = total > COMPACT_SKILLS_MODE_THRESHOLD || omitted > 0;

        let mut out = String::from("<available_skills>\n");
        for skill in &visible {
            if compact_mode {
                out.push_str(&format!("- {}\n", skill.name));
            } else {
                out.push_str(&format!(
                    "- {}: {}\n",
                    skill.name,
                    truncate_chars(&skill.description, MAX_SKILL_DESCRIPTION_CHARS)
                ));
            }
        }
        if compact_mode {
            out.push_str("- (compact mode: use activate_skill to load full instructions)\n");
        }
        if omitted > 0 {
            out.push_str(&format!(
                "- ... ({} additional skills omitted for prompt budget)\n",
                omitted
            ));
        }
        out.push_str("</available_skills>");
        out
    }

    fn skill_is_available(&self, skill: &SkillMetadata) -> bool {
        platform_allowed(&skill.platforms) && missing_deps(&skill.deps).is_empty()
    }

    fn discover_skill_dirs(&self) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        // Highest priority: user skills (workspace/skills/*)
        collect_skill_dirs_direct_children(&self.user_skills_dir, &mut candidates);

        // Then recursively scan workspace for SKILL.md files.
        let Some(workspace_root) = self.user_skills_dir.parent() else {
            return candidates;
        };
        let Ok(user_skills_dir_canonical) = std::fs::canonicalize(&self.user_skills_dir) else {
            collect_skill_dirs_recursive(workspace_root, &self.user_skills_dir, &mut candidates);
            return candidates;
        };

        collect_skill_dirs_recursive(workspace_root, &user_skills_dir_canonical, &mut candidates);

        // Built-in skills: state_root/skills/*
        if self.builtin_skills_dir != self.user_skills_dir {
            collect_skill_dirs_direct_children(&self.builtin_skills_dir, &mut candidates);
        }

        candidates
    }
}

fn collect_skill_dirs_direct_children(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join("SKILL.md").is_file() {
            out.push(path);
        }
    }
}

fn collect_skill_dirs_recursive(
    root: &Path,
    prioritized_skills_dir: &Path,
    out: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if should_skip_directory(&path, prioritized_skills_dir) {
            continue;
        }
        if path.join("SKILL.md").is_file() {
            out.push(path.clone());
        }
        collect_skill_dirs_recursive(&path, prioritized_skills_dir, out);
    }
}

fn should_skip_directory(path: &Path, prioritized_skills_dir: &Path) -> bool {
    if path == prioritized_skills_dir {
        return true;
    }

    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };

    matches!(
        name,
        ".git" | ".gradle" | "node_modules" | "target" | "build" | "dist"
    )
}

fn parse_skill_md(content: &str, dir_path: &Path) -> Option<(SkillMetadata, String)> {
    let content = content.replace("\r\n", "\n");
    let trimmed = content.trim_start();
    let rest = trimmed.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n')?;
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    let body = rest[end + 4..].trim().to_string();
    let parsed: SkillFrontmatter = serde_yml::from_str(frontmatter).ok()?;
    let name = parsed.name?.trim().to_string();
    if name.is_empty() {
        return None;
    }

    Some((
        SkillMetadata {
            name,
            description: parsed.description.trim().to_string(),
            dir_path: dir_path.to_path_buf(),
            platforms: parsed.platforms,
            deps: parsed.deps,
        },
        body,
    ))
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let truncated = value.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

fn normalize_platform(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "macos" | "osx" => "darwin".to_string(),
        other => other.to_string(),
    }
}

fn platform_allowed(platforms: &[String]) -> bool {
    if platforms.is_empty() {
        return true;
    }

    let current = current_platform();
    platforms.iter().any(|platform| {
        let normalized = normalize_platform(platform);
        normalized == "*" || normalized == "all" || normalized == current
    })
}

fn missing_deps(deps: &[String]) -> Vec<String> {
    deps.iter()
        .filter(|dep| !command_exists(dep))
        .cloned()
        .collect()
}

#[cfg(target_os = "windows")]
fn command_exists(command: &str) -> bool {
    if command.trim().is_empty() {
        return true;
    }

    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };

    let pathext_default = ".COM;.EXE;.BAT;.CMD";
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| pathext_default.to_string());
    let executable_extensions: Vec<&str> = pathext.split(';').collect();

    let command_candidates: Vec<std::path::PathBuf> = if command.contains('.') {
        vec![command.into()]
    } else {
        let mut candidates = vec![command.into()];
        candidates.extend(
            executable_extensions
                .iter()
                .map(|ext| format!("{command}{ext}").into()),
        );
        candidates
    };

    std::env::split_paths(&path_var).any(|base| {
        command_candidates.iter().any(|candidate| {
            let full_path = base.join(candidate);
            full_path.is_file() && is_executable(&full_path)
        })
    })
}

#[cfg(not(target_os = "windows"))]
fn command_exists(command: &str) -> bool {
    if command.trim().is_empty() {
        return true;
    }

    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };

    std::env::split_paths(&path_var).any(|base| {
        let candidate = base.join(command);
        candidate.is_file() && is_executable(&candidate)
    })
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &std::path::Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::SkillManager;

    fn create_skill(root: &std::path::Path, name: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("create skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Description for {name}\n---\n{body}\n"),
        )
        .expect("write skill");
    }

    fn create_skill_at(dir: &std::path::Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).expect("create skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Description for {name}\n---\n{body}\n"),
        )
        .expect("write skill");
    }

    #[test]
    fn discovers_and_loads_skill() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skills_dir = dir.path().join("workspace").join("skills");
        create_skill(&skills_dir, "pdf", "Use the PDF workflow.");

        let manager = SkillManager::from_skills_dir(skills_dir);
        let skills = manager.discover_skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "pdf");

        let loaded = manager.load_skill_checked("pdf").expect("load skill");
        assert_eq!(loaded.metadata.name, "pdf");
        assert!(loaded.instructions.contains("PDF workflow"));
    }

    #[test]
    fn builds_catalog_for_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skills_dir = dir.path().join("workspace").join("skills");
        create_skill(&skills_dir, "pdf", "Use the PDF workflow.");
        let manager = SkillManager::from_skills_dir(skills_dir);

        let catalog = manager.build_skills_catalog();
        assert!(catalog.contains("<available_skills>"));
        assert!(catalog.contains("pdf: Description for pdf"));
    }

    #[test]
    fn discovers_skills_recursively_under_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        let skills_dir = workspace.join("skills");
        create_skill(&skills_dir, "pdf", "Use the PDF workflow.");
        create_skill_at(
            &workspace.join("features").join("images").join("resize"),
            "resize",
            "Use the resize workflow.",
        );

        let manager = SkillManager::from_skills_dir(skills_dir);
        let skills = manager.discover_skills();

        assert_eq!(skills.len(), 2);
        assert!(skills.iter().any(|skill| skill.name == "pdf"));
        assert!(skills.iter().any(|skill| skill.name == "resize"));
    }

    #[test]
    fn prefers_workspace_skills_dir_on_duplicate_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        let skills_dir = workspace.join("skills");
        create_skill(&skills_dir, "pdf", "Preferred instructions.");
        create_skill_at(
            &workspace.join("notes").join("pdf-copy"),
            "pdf",
            "Fallback instructions.",
        );

        let manager = SkillManager::from_skills_dir(skills_dir.clone());
        let loaded = manager.load_skill_checked("pdf").expect("load skill");

        assert_eq!(loaded.metadata.dir_path, skills_dir.join("pdf"));
        assert!(loaded.instructions.contains("Preferred instructions."));
    }

    #[test]
    fn ignores_skills_inside_excluded_build_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        let skills_dir = workspace.join("skills");
        create_skill(&skills_dir, "pdf", "Use the PDF workflow.");
        create_skill_at(
            &workspace
                .join("node_modules")
                .join("some-package")
                .join("skill"),
            "ignored",
            "Should not be discovered.",
        );

        let manager = SkillManager::from_skills_dir(skills_dir);
        let skills = manager.discover_skills();

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "pdf");
    }

    #[test]
    fn list_skills_formatted_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skills_dir = dir.path().join("workspace").join("skills");

        let manager = SkillManager::from_skills_dir(skills_dir);
        let formatted = manager.list_skills_formatted();

        assert_eq!(formatted, "No skills loaded.");
    }

    #[test]
    fn list_skills_formatted_multiple() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skills_dir = dir.path().join("workspace").join("skills");
        create_skill(&skills_dir, "pdf", "Use the PDF workflow.");
        create_skill(&skills_dir, "docx", "Use the DOCX workflow.");

        let manager = SkillManager::from_skills_dir(skills_dir);
        let formatted = manager.list_skills_formatted();

        assert!(formatted.starts_with("Available skills:\n"));
        assert!(formatted.contains("- pdf (Description for pdf)"));
        assert!(formatted.contains("- docx (Description for docx)"));
    }
}

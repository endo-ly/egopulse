use serde::Deserialize;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSkill {
    pub metadata: SkillMetadata,
    pub instructions: String,
}

#[derive(Debug, Clone)]
pub struct SkillManager {
    skills_dir: PathBuf,
}

const MAX_SKILLS_CATALOG_ITEMS: usize = 40;
const MAX_SKILL_DESCRIPTION_CHARS: usize = 120;
const COMPACT_SKILLS_MODE_THRESHOLD: usize = 20;

impl SkillManager {
    pub fn from_skills_dir(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            skills_dir: skills_dir.into(),
        }
    }

    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    pub fn discover_skills(&self) -> Vec<SkillMetadata> {
        let mut skills = Vec::new();
        let entries = match std::fs::read_dir(&self.skills_dir) {
            Ok(entries) => entries,
            Err(_) => return skills,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let skill_md = path.join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&skill_md) else {
                continue;
            };
            let Some((meta, _body)) = parse_skill_md(&content, &path) else {
                continue;
            };
            if self.skill_is_available(&meta) {
                skills.push(meta);
            }
        }

        skills.sort_by(|left, right| left.name.cmp(&right.name));
        skills
    }

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
}

fn parse_skill_md(content: &str, dir_path: &Path) -> Option<(SkillMetadata, String)> {
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

fn command_exists(command: &str) -> bool {
    if command.trim().is_empty() {
        return true;
    }

    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };

    std::env::split_paths(&path_var).any(|base| {
        let candidate = base.join(command);
        candidate.is_file()
    })
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

    #[test]
    fn discovers_and_loads_skill() {
        let dir = tempfile::tempdir().expect("tempdir");
        create_skill(dir.path(), "pdf", "Use the PDF workflow.");

        let manager = SkillManager::from_skills_dir(dir.path());
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
        create_skill(dir.path(), "pdf", "Use the PDF workflow.");
        let manager = SkillManager::from_skills_dir(dir.path());

        let catalog = manager.build_skills_catalog();
        assert!(catalog.contains("<available_skills>"));
        assert!(catalog.contains("pdf: Description for pdf"));
    }
}

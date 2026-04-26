use std::io;
use std::path::{Path, PathBuf};

const DEFAULT_SOUL_MD: &str = include_str!("default_soul.md");

pub struct SoulAgentsLoader {
    state_root: PathBuf,
    soul_path: PathBuf,
    agents_path: PathBuf,
    agents_dir: PathBuf,
    souls_dir: PathBuf,
}

impl SoulAgentsLoader {
    pub fn new(config: &crate::config::Config) -> Self {
        Self {
            state_root: PathBuf::from(&config.state_root),
            soul_path: config.soul_path(),
            agents_path: config.agents_path(),
            agents_dir: PathBuf::from(&config.state_root).join("agents"),
            souls_dir: config.souls_dir(),
        }
    }

    /// Agent SOUL → channel soul_path → global SOUL.md
    pub fn load_soul(
        &self,
        _channel: &str,
        _thread: &str,
        channel_soul_path: Option<&str>,
        agent_id: Option<&str>,
    ) -> Option<String> {
        self.load_base_soul(channel_soul_path, agent_id)
    }

    fn load_base_soul(
        &self,
        channel_soul_path: Option<&str>,
        agent_id: Option<&str>,
    ) -> Option<String> {
        if let Some(id) = agent_id {
            if safe_agent_id(id) {
                let path = self.agents_dir.join(id).join("SOUL.md");
                if let Some(content) = read_trimmed(&path) {
                    return Some(content);
                }
            }
        }

        if let Some(soul_path) = channel_soul_path {
            let candidates = self.resolve_soul_path(soul_path);
            for candidate in candidates {
                if let Some(content) = read_trimmed(&candidate) {
                    return Some(content);
                }
            }
        }

        read_trimmed(&self.soul_path)
    }

    /// souls/ ディレクトリから名前指定で読み込み。
    /// "work" → souls/work.md, "work.md" → souls/work.md
    pub fn load_soul_by_name(&self, name: &str) -> Option<String> {
        let stripped = name.strip_suffix(".md").unwrap_or(name);
        let path = self.souls_dir.join(format!("{stripped}.md"));
        read_trimmed(&path)
    }

    /// 相対パスを解決する。
    /// - まず souls/ から探す
    /// - 次に state_root から探す
    fn resolve_soul_path(&self, path: &str) -> Vec<PathBuf> {
        let p = Path::new(path);
        if p.is_absolute() {
            return vec![p.to_path_buf()];
        }

        vec![
            self.souls_dir.join(format!("{path}.md")),
            self.souls_dir.join(path),
            self.state_root.join(format!("{path}.md")),
            self.state_root.join(path),
        ]
    }

    /// グローバル AGENTS.md を読み込む
    pub fn load_global_agents(&self) -> Option<String> {
        read_trimmed(&self.agents_path)
    }

    pub fn build_soul_section(&self, content: &str, channel: &str) -> String {
        format!("<soul>\n{content}\n</soul>\n\nYour name is EgoPulse. Current channel: {channel}.")
    }

    pub fn build_agents_section(
        &self,
        _channel: &str,
        _thread: &str,
        agent_id: Option<&str>,
    ) -> Option<String> {
        let global = self.load_global_agents();
        let agent_agents = agent_id.and_then(|id| {
            if safe_agent_id(id) {
                read_trimmed(&self.agents_dir.join(id).join("AGENTS.md"))
            } else {
                None
            }
        });

        if global.is_none() && agent_agents.is_none() {
            return None;
        }

        let mut section = String::from("# Memories\n");
        if let Some(content) = global {
            section.push_str(&format!("\n<agents>\n{content}\n</agents>\n"));
        }
        if let Some(content) = agent_agents {
            section.push_str(&format!("\n<agents>\n{content}\n</agents>\n"));
        }
        Some(section)
    }

    pub fn provision_default_soul(&self) -> io::Result<bool> {
        if self.soul_path.exists() {
            return Ok(false);
        }
        if let Some(parent) = self.soul_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.soul_path, DEFAULT_SOUL_MD)?;
        Ok(true)
    }
}

/// `Path::components()` がすべて `Normal` であることを検証し、
/// `../` や `./` を含むパストラバーサルを弾く。
fn safe_agent_id(id: &str) -> bool {
    !id.is_empty()
        && Path::new(id)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn read_trimmed(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_loader(dir: &Path) -> SoulAgentsLoader {
        SoulAgentsLoader {
            state_root: dir.to_path_buf(),
            soul_path: dir.join("SOUL.md"),
            agents_path: dir.join("AGENTS.md"),
            agents_dir: dir.join("agents"),
            souls_dir: dir.join("souls"),
        }
    }

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    // --- load_soul tests ---

    #[test]
    fn load_soul_reads_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("SOUL.md"), "I am a helpful assistant.");

        let result = loader.load_soul("web", "t1", None, None);
        assert_eq!(result, Some("I am a helpful assistant.".to_string()));
    }

    #[test]
    fn load_soul_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load_soul("web", "t1", None, None);
        assert_eq!(result, None);
    }

    #[test]
    fn load_soul_returns_none_for_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("SOUL.md"), "   \n\n  ");

        let result = loader.load_soul("web", "t1", None, None);
        assert_eq!(result, None);
    }

    // --- load_global_agents tests ---

    #[test]
    fn load_agents_reads_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("AGENTS.md"), "Use python for code tasks.");

        let result = loader.load_global_agents();
        assert_eq!(result, Some("Use python for code tasks.".to_string()));
    }

    #[test]
    fn load_agents_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load_global_agents();
        assert_eq!(result, None);
    }

    // --- load_chat_agents tests (removed: chat-specific loading removed) ---

    #[test]
    fn load_chat_agents_removed_no_longer_reads_chat_file() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("SOUL.md"), "Global soul");
        let chat_soul = dir.path().join("runtime/groups/web/thread1/SOUL.md");
        write_file(&chat_soul, "Chat-specific soul");

        let result = loader.load_soul("web", "thread1", None, None);
        assert_eq!(result, Some("Global soul".to_string()));
    }

    #[test]
    fn load_chat_agents_removed_returns_none_without_global_soul() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load_soul("web", "thread1", None, None);
        assert_eq!(result, None);
    }

    // --- load_soul_by_name tests ---

    #[test]
    fn load_soul_from_souls_dir_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("souls/work.md"), "Work soul content");

        let result = loader.load_soul_by_name("work");
        assert_eq!(result, Some("Work soul content".to_string()));
    }

    #[test]
    fn load_soul_from_souls_dir_with_md_extension() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("souls/work.md"), "Work soul content");

        let result = loader.load_soul_by_name("work.md");
        assert_eq!(result, Some("Work soul content".to_string()));
    }

    #[test]
    fn load_soul_from_souls_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load_soul_by_name("nonexistent");
        assert_eq!(result, None);
    }

    // --- resolve_soul_path tests ---

    #[test]
    fn resolve_soul_path_absolute_uses_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.resolve_soul_path("/absolute/path");
        assert_eq!(result, vec![PathBuf::from("/absolute/path")]);
    }

    #[test]
    fn resolve_soul_path_relative_resolves_from_souls_dir() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.resolve_soul_path("friendly");
        assert_eq!(result[0], dir.path().join("souls/friendly.md"));
    }

    #[test]
    fn resolve_soul_path_relative_resolves_from_state_root() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.resolve_soul_path("friendly");
        assert_eq!(result[2], dir.path().join("friendly.md"));
    }

    // --- channel_soul_path fallback tests ---

    #[test]
    fn load_soul_prefers_channel_soul_over_default() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("SOUL.md"), "Default soul");
        write_file(&dir.path().join("souls/custom.md"), "Custom channel soul");

        let result = loader.load_soul("web", "t1", Some("custom"), None);
        assert_eq!(result, Some("Custom channel soul".to_string()));
    }

    #[test]
    fn load_soul_falls_back_to_default_when_channel_soul_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("SOUL.md"), "Default soul");

        let result = loader.load_soul("web", "t1", Some("nonexistent"), None);
        assert_eq!(result, Some("Default soul".to_string()));
    }

    // --- agent_id tests ---

    #[test]
    fn load_soul_agent_id_falls_through_when_no_agent_soul() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("SOUL.md"), "Default soul");

        let result = loader.load_soul("web", "t1", None, Some("user1"));
        assert_eq!(result, Some("Default soul".to_string()));
    }

    #[test]
    fn load_soul_agent_id_falls_through_to_channel() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("souls/custom.md"), "Custom soul");

        let result = loader.load_soul("web", "t1", Some("custom"), Some("user1"));
        assert_eq!(result, Some("Custom soul".to_string()));
    }

    // --- build_soul_section tests ---

    #[test]
    fn build_soul_section_wraps_in_xml_tags() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.build_soul_section("I am helpful", "web");
        assert!(result.starts_with("<soul>\n"));
        assert!(result.contains("</soul>"));
    }

    #[test]
    fn build_soul_section_includes_identity_line() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.build_soul_section("I am helpful", "web");
        assert!(result.contains("Your name is EgoPulse. Current channel: web."));
    }

    // --- build_agents_section tests ---

    #[test]
    fn build_agents_section_formats_memories_header() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("AGENTS.md"), "Global agents content");

        let result = loader.build_agents_section("web", "thread1", None);
        let section = result.expect("should return Some");
        assert!(section.contains("# Memories"));
        assert!(section.contains("<agents>"));
        assert!(section.contains("Global agents content"));
        assert!(section.contains("</agents>"));
    }

    // --- provision_default_soul tests ---

    #[test]
    fn default_soul_content_is_non_empty_and_contains_key_phrases() {
        assert!(!DEFAULT_SOUL_MD.trim().is_empty());
        assert!(DEFAULT_SOUL_MD.contains("action-oriented"));
        assert!(DEFAULT_SOUL_MD.contains("Reliability over impressiveness"));
    }

    #[test]
    fn provision_default_soul_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let created = loader.provision_default_soul().unwrap();
        assert!(created);

        let content = std::fs::read_to_string(dir.path().join("SOUL.md")).unwrap();
        assert_eq!(content, DEFAULT_SOUL_MD);
    }

    #[test]
    fn provision_default_soul_does_not_overwrite_existing() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        write_file(&dir.path().join("SOUL.md"), "Existing personality");

        let created = loader.provision_default_soul().unwrap();
        assert!(!created);

        let content = std::fs::read_to_string(dir.path().join("SOUL.md")).unwrap();
        assert_eq!(content, "Existing personality");
    }

    // --- path traversal guards ---

    #[test]
    fn load_soul_agent_id_rejects_parent_dir_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load_soul("web", "t1", None, Some("../etc"));
        assert_eq!(result, None);
    }

    #[test]
    fn load_soul_rejects_parent_dir_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        assert!(
            loader
                .load_soul("../../../etc", "thread", None, None)
                .is_none()
        );
    }

    // --- agent-specific SOUL/AGENTS tests ---

    #[test]
    fn load_soul_prefers_agent_soul() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("agents/alice/SOUL.md"), "Alice soul");
        write_file(&dir.path().join("SOUL.md"), "Global soul");
        write_file(&dir.path().join("souls/custom.md"), "Custom soul");

        let result = loader.load_soul("web", "t1", Some("custom"), Some("alice"));
        assert_eq!(result, Some("Alice soul".to_string()));
    }

    #[test]
    fn load_soul_falls_back_to_channel_soul_path() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("souls/custom.md"), "Custom soul");
        write_file(&dir.path().join("SOUL.md"), "Global soul");

        let result = loader.load_soul("web", "t1", Some("custom"), Some("alice"));
        assert_eq!(result, Some("Custom soul".to_string()));
    }

    #[test]
    fn load_soul_falls_back_to_global_soul() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("SOUL.md"), "Global soul");

        let result = loader.load_soul("web", "t1", None, Some("alice"));
        assert_eq!(result, Some("Global soul".to_string()));
    }

    #[test]
    fn build_agents_combines_global_and_agent_agents() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("AGENTS.md"), "Global agents content");
        write_file(
            &dir.path().join("agents/alice/AGENTS.md"),
            "Alice agents content",
        );

        let result = loader.build_agents_section("web", "t1", Some("alice"));
        let section = result.expect("should return Some");
        assert!(section.contains("Global agents content"));
        assert!(section.contains("Alice agents content"));
    }

    #[test]
    fn chat_specific_md_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        write_file(&dir.path().join("SOUL.md"), "Global soul");
        write_file(
            &dir.path().join("runtime/groups/web/thread1/SOUL.md"),
            "Chat soul",
        );
        write_file(
            &dir.path().join("runtime/groups/web/thread1/AGENTS.md"),
            "Chat agents",
        );

        let soul = loader.load_soul("web", "thread1", None, None);
        assert_eq!(soul, Some("Global soul".to_string()));

        let agents = loader.build_agents_section("web", "thread1", None);
        assert!(agents.is_none());
    }

    #[test]
    fn agent_id_path_traversal_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        assert!(
            loader
                .load_soul("web", "t1", None, Some("../etc"))
                .is_none()
        );
        assert!(
            loader
                .load_soul("web", "t1", None, Some("../../soul_agents"))
                .is_none()
        );
        assert!(loader.load_soul("web", "t1", None, Some("")).is_none());
    }
}

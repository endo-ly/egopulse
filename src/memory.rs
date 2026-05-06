use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// Agent long-term memory content loaded from markdown files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub(crate) struct MemoryContent {
    pub episodic: Option<String>,
    pub semantic: Option<String>,
    pub prospective: Option<String>,
}

struct CachedContent {
    path: PathBuf,
    content: String,
    mtime: SystemTime,
}

/// Loads agent long-term memory files from `{agents_dir}/{agent_id}/memory/`.
///
/// Follows the same caching pattern as `SoulAgentsLoader` — tracks file mtime
/// and re-reads only when changed.
#[allow(dead_code)]
pub(crate) struct MemoryLoader {
    agents_dir: PathBuf,
    episodic_cache: Mutex<Option<CachedContent>>,
    semantic_cache: Mutex<Option<CachedContent>>,
    prospective_cache: Mutex<Option<CachedContent>>,
}

#[allow(dead_code)]
impl MemoryLoader {
    pub(crate) fn new(agents_dir: PathBuf) -> Self {
        Self {
            agents_dir,
            episodic_cache: Mutex::new(None),
            semantic_cache: Mutex::new(None),
            prospective_cache: Mutex::new(None),
        }
    }

    /// Loads memory files for the given agent.
    ///
    /// Reads `agents/{agent_id}/memory/{episodic,semantic,prospective}.md`.
    /// Returns `None` if all files are missing or empty.
    pub(crate) fn load(&self, agent_id: &str) -> Option<MemoryContent> {
        if !safe_agent_id(agent_id) {
            return None;
        }

        let memory_dir = self.agents_dir.join(agent_id).join("memory");

        let episodic =
            self.cached_read_trimmed(&memory_dir.join("episodic.md"), &self.episodic_cache);
        let semantic =
            self.cached_read_trimmed(&memory_dir.join("semantic.md"), &self.semantic_cache);
        let prospective =
            self.cached_read_trimmed(&memory_dir.join("prospective.md"), &self.prospective_cache);

        if episodic.is_none() && semantic.is_none() && prospective.is_none() {
            return None;
        }

        Some(MemoryContent {
            episodic,
            semantic,
            prospective,
        })
    }

    fn cached_read_trimmed(
        &self,
        path: &Path,
        cache: &Mutex<Option<CachedContent>>,
    ) -> Option<String> {
        let current_mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
        let mut guard = cache.lock().expect("memory cache lock");
        if let (Some(cached), Some(mtime)) = (&*guard, current_mtime) {
            if cached.path == path && cached.mtime == mtime {
                return Some(cached.content.clone());
            }
        }
        let content = read_trimmed(path)?;
        if let Some(mtime) = current_mtime {
            *guard = Some(CachedContent {
                path: path.to_path_buf(),
                content: content.clone(),
                mtime,
            });
        }
        Some(content)
    }
}

#[allow(dead_code)]
fn safe_agent_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && !id.contains("..")
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains(':')
}

#[allow(dead_code)]
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

    fn make_loader(dir: &Path) -> MemoryLoader {
        MemoryLoader::new(dir.join("agents"))
    }

    fn write_memory_file(dir: &Path, agent_id: &str, file_name: &str, content: &str) {
        let path = dir
            .join("agents")
            .join(agent_id)
            .join("memory")
            .join(file_name);
        fs::create_dir_all(path.parent().expect("memory dir has parent"))
            .expect("create memory dir");
        fs::write(path, content).expect("write memory file");
    }

    fn write_raw_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("file has parent")).expect("create dirs");
        fs::write(path, content).expect("write file");
    }

    // --- Test 1: load all three files ---

    #[test]
    fn load_memory_reads_all_three_files() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "episodic.md", "episodic content");
        write_memory_file(dir.path(), "testagent", "semantic.md", "semantic content");
        write_memory_file(
            dir.path(),
            "testagent",
            "prospective.md",
            "prospective content",
        );

        let loader = make_loader(dir.path());
        let result = loader.load("testagent");

        let mem = result.expect("should return Some");
        assert_eq!(mem.episodic, Some("episodic content".to_string()));
        assert_eq!(mem.semantic, Some("semantic content".to_string()));
        assert_eq!(mem.prospective, Some("prospective content".to_string()));
    }

    // --- Test 2: no memory dir at all ---

    #[test]
    fn load_memory_returns_none_when_no_memory_dir() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load("testagent");
        assert_eq!(result, None);
    }

    // --- Test 3: memory dir exists but no files ---

    #[test]
    fn load_memory_returns_none_when_all_files_missing() {
        let dir = tempfile::tempdir().unwrap();
        let memory_dir = dir.path().join("agents").join("testagent").join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let loader = make_loader(dir.path());
        let result = loader.load("testagent");
        assert_eq!(result, None);
    }

    // --- Test 4: skips empty files ---

    #[test]
    fn load_memory_skips_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "episodic.md", "episodic content");
        write_memory_file(dir.path(), "testagent", "semantic.md", "   \n\n  ");
        write_memory_file(
            dir.path(),
            "testagent",
            "prospective.md",
            "prospective content",
        );

        let loader = make_loader(dir.path());
        let result = loader.load("testagent");

        let mem = result.expect("should return Some");
        assert_eq!(mem.episodic, Some("episodic content".to_string()));
        assert_eq!(mem.semantic, None);
        assert_eq!(mem.prospective, Some("prospective content".to_string()));
    }

    // --- Test 5: only episodic exists ---

    #[test]
    fn load_memory_individual_episodic() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "episodic.md", "episodic only");

        let loader = make_loader(dir.path());
        let mem = loader.load("testagent").expect("should return Some");

        assert_eq!(mem.episodic, Some("episodic only".to_string()));
        assert_eq!(mem.semantic, None);
        assert_eq!(mem.prospective, None);
    }

    // --- Test 6: only semantic exists ---

    #[test]
    fn load_memory_individual_semantic() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "semantic.md", "semantic only");

        let loader = make_loader(dir.path());
        let mem = loader.load("testagent").expect("should return Some");

        assert_eq!(mem.episodic, None);
        assert_eq!(mem.semantic, Some("semantic only".to_string()));
        assert_eq!(mem.prospective, None);
    }

    // --- Test 7: only prospective exists ---

    #[test]
    fn load_memory_individual_prospective() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(
            dir.path(),
            "testagent",
            "prospective.md",
            "prospective only",
        );

        let loader = make_loader(dir.path());
        let mem = loader.load("testagent").expect("should return Some");

        assert_eq!(mem.episodic, None);
        assert_eq!(mem.semantic, None);
        assert_eq!(mem.prospective, Some("prospective only".to_string()));
    }

    // --- Test 8: path traversal rejection ---

    #[test]
    fn load_memory_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load("../etc");
        assert_eq!(result, None);
    }

    // --- Test 9: empty agent_id rejection ---

    #[test]
    fn load_memory_rejects_empty_agent_id() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load("");
        assert_eq!(result, None);
    }

    // --- Test 10: cache unchanged file ---

    #[test]
    fn load_memory_caches_unchanged_file() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "episodic.md", "cached content");

        let loader = make_loader(dir.path());
        let first = loader.load("testagent");
        let second = loader.load("testagent");

        assert_eq!(first, second);
        assert_eq!(first.unwrap().episodic, Some("cached content".to_string()));
    }

    // --- Test 11: cache invalidation on mtime change ---

    #[test]
    fn load_memory_invalidates_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir
            .path()
            .join("agents")
            .join("testagent")
            .join("memory")
            .join("episodic.md");
        write_raw_file(&file_path, "original content");

        let loader = make_loader(dir.path());
        let first = loader.load("testagent");
        assert_eq!(
            first.unwrap().episodic,
            Some("original content".to_string())
        );

        // Ensure mtime differs — modify and force flush
        write_raw_file(&file_path, "updated content");
        // Filesystems have 1s mtime resolution; wait to guarantee a different mtime
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&file_path, "updated content").unwrap();

        let second = loader.load("testagent");
        assert_eq!(
            second.unwrap().episodic,
            Some("updated content".to_string())
        );
    }
}

//! Agent long-term memory: bundle loading, atomic publication, and crash recovery.
//!
//! The three memory files (`episodic.md`, `semantic.md`, `prospective.md`) are
//! treated as a single [`MemoryBundle`]. Reads ([`MemoryLoader::load_bundle`])
//! take a per-agent **read lock** so they never observe a half-published
//! bundle. Sleep publication ([`MemoryLoader::publish_bundle`]) takes the
//! per-agent **write lock**, verifies the on-disk files still match the
//! run-start baseline, then replaces all three files via temp-file + rename +
//! fsync so a crash leaves either the old or the new bundle — never a mix.
//!
//! Crash recovery ([`MemoryLoader::recover_publication`]) re-drives the
//! rename sequence from the persisted `memory_snapshots` so a run interrupted
//! mid-publication converges to the same bundle on the next startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use tracing::warn;

use crate::runtime::metrics;

/// The three long-term memory files published atomically as one bundle.
///
/// Each field holds the raw file content (empty string when the file is absent
/// or empty). Callers that want to omit empty sections (e.g. the Turn prompt
/// builder) filter on `is_empty()` themselves.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct MemoryBundle {
    pub episodic: String,
    pub semantic: String,
    pub prospective: String,
}

impl MemoryBundle {
    /// Returns the content for the given memory file kind.
    fn file(&self, file: MemoryFile) -> &str {
        match file {
            MemoryFile::Episodic => &self.episodic,
            MemoryFile::Semantic => &self.semantic,
            MemoryFile::Prospective => &self.prospective,
        }
    }

    /// Whether all three files are empty (no memory published yet).
    pub(crate) fn all_empty(&self) -> bool {
        self.episodic.is_empty() && self.semantic.is_empty() && self.prospective.is_empty()
    }
}

/// The three memory file kinds, mirroring [`crate::storage::MemoryFile`] without
/// pulling the storage dependency into this module's public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryFile {
    Episodic,
    Semantic,
    Prospective,
}

impl MemoryFile {
    const ALL: [Self; 3] = [Self::Episodic, Self::Semantic, Self::Prospective];

    const fn file_name(self) -> &'static str {
        match self {
            Self::Episodic => "episodic.md",
            Self::Semantic => "semantic.md",
            Self::Prospective => "prospective.md",
        }
    }
}

/// Memory loading, publication, and recovery errors.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MemoryError {
    #[error("memory_io_failed: {0}")]
    Io(String),
    #[error("memory_unsafe_agent_id: {0}")]
    UnsafeAgentId(String),
    /// Publication precondition failed: the on-disk file changed between run
    /// start and publication (manual edit or concurrent writer). The current
    /// files are left untouched and the run must be marked failed.
    #[error("memory_publication_conflict: agent={agent_id} file={file}")]
    Conflict { agent_id: String, file: String },
    /// Startup recovery could not classify the on-disk content as either the
    /// pre- or post-publication state. Startup must halt to avoid silent loss.
    #[error("memory_recovery_validation_failed: agent={agent_id} run={run_id} file={file}")]
    RecoveryValidation {
        agent_id: String,
        run_id: String,
        file: String,
    },
}

impl From<std::io::Error> for MemoryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

struct CachedBundle {
    bundle: Arc<MemoryBundle>,
    /// mtime per file, indexed parallel to [`MemoryFile::ALL`].
    mtimes: [Option<SystemTime>; 3],
}

/// Loads agent long-term memory bundles from `{agents_dir}/{agent_id}/memory/`
/// and publishes new bundles atomically.
///
/// A per-agent [`RwLock`] serializes readers against the single writer
/// (publication). The write lock is held only across file I/O, never across
/// LLM generation, so a Turn can read the published bundle while a Sleep run
/// is still generating its candidate.
pub(crate) struct MemoryLoader {
    agents_dir: PathBuf,
    locks: Mutex<HashMap<String, Arc<RwLock<()>>>>,
    cache: Mutex<HashMap<String, CachedBundle>>,
}

impl MemoryLoader {
    pub(crate) fn new(agents_dir: PathBuf) -> Self {
        Self {
            agents_dir,
            locks: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn lock_for(&self, agent_id: &str) -> Arc<RwLock<()>> {
        let mut locks = self.locks.lock().expect("memory locks map lock");
        locks
            .entry(agent_id.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }

    /// Loads the current published memory bundle for `agent_id`.
    ///
    /// Takes the per-agent read lock, re-reads files only when their mtimes
    /// changed since the last load, and returns the cached bundle otherwise.
    /// Missing files contribute empty strings. Returns an empty bundle when no
    /// memory directory exists.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Io`] on filesystem errors other than
    /// "file not found", and [`MemoryError::UnsafeAgentId`] for path-traversal
    /// agent ids.
    pub(crate) fn load_bundle(&self, agent_id: &str) -> Result<Arc<MemoryBundle>, MemoryError> {
        if !safe_agent_id(agent_id) {
            return Err(MemoryError::UnsafeAgentId(agent_id.to_string()));
        }
        let lock = self.lock_for(agent_id);
        let _guard = lock.read().expect("memory read lock");

        let memory_dir = self.memory_dir(agent_id);
        let mtimes = file_mtimes(&memory_dir);

        {
            let cache = self.cache.lock().expect("memory cache lock");
            if let Some(cached) = cache.get(agent_id) {
                if cached.mtimes == mtimes {
                    return Ok(Arc::clone(&cached.bundle));
                }
            }
        }

        let bundle = Arc::new(read_bundle(&memory_dir));
        let mut cache = self.cache.lock().expect("memory cache lock");
        cache.insert(
            agent_id.to_string(),
            CachedBundle {
                bundle: Arc::clone(&bundle),
                mtimes,
            },
        );
        Ok(bundle)
    }

    /// Publishes `candidate` as the new memory bundle for `agent_id`.
    ///
    /// Verifies the on-disk files still equal `base` (the run-start bundle)
    /// before replacing them, so a manual edit during the run aborts
    /// publication with [`MemoryError::Conflict`] and leaves the current files
    /// untouched. The write lock is held across the temp-file + rename +
    /// fsync sequence so concurrent readers never observe a partial bundle.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Conflict`] on precondition mismatch,
    /// [`MemoryError::UnsafeAgentId`] for path-traversal agent ids, and
    /// [`MemoryError::Io`] on filesystem failure.
    pub(crate) fn publish_bundle(
        &self,
        agent_id: &str,
        run_id: &str,
        base: &MemoryBundle,
        candidate: &MemoryBundle,
    ) -> Result<(), MemoryError> {
        if !safe_agent_id(agent_id) {
            return Err(MemoryError::UnsafeAgentId(agent_id.to_string()));
        }
        let lock = self.lock_for(agent_id);
        let _guard = lock.write().expect("memory write lock");
        metrics::inc_memory_publication("started");

        let memory_dir = self.memory_dir(agent_id);
        let current = read_bundle(&memory_dir);

        for file in MemoryFile::ALL {
            if current.file(file) != base.file(file) {
                metrics::inc_memory_publication("conflict");
                return Err(MemoryError::Conflict {
                    agent_id: agent_id.to_string(),
                    file: file.file_name().to_string(),
                });
            }
        }

        write_bundle_atomically(&memory_dir, run_id, candidate)?;
        self.refresh_cache(agent_id, candidate);

        metrics::inc_memory_publication("success");
        Ok(())
    }

    /// Re-drives publication for a run interrupted mid-rename.
    ///
    /// Each on-disk file must equal either its `before` (pre-publication) or
    /// `after` (post-publication) content; otherwise the run is in an
    /// unclassifiable state and recovery halts with
    /// [`MemoryError::RecoveryValidation`]. The `after` bundle is then
    /// (re)written atomically so all three files converge to the intended
    /// post-publication state.
    ///
    /// # Errors
    ///
    /// See [`MemoryError`] variants.
    pub(crate) fn recover_publication(
        &self,
        agent_id: &str,
        run_id: &str,
        before: &MemoryBundle,
        after: &MemoryBundle,
    ) -> Result<(), MemoryError> {
        if !safe_agent_id(agent_id) {
            return Err(MemoryError::UnsafeAgentId(agent_id.to_string()));
        }
        let lock = self.lock_for(agent_id);
        let _guard = lock.write().expect("memory write lock");

        let memory_dir = self.memory_dir(agent_id);
        let current = read_bundle(&memory_dir);

        for file in MemoryFile::ALL {
            let cur = current.file(file);
            if cur != before.file(file) && cur != after.file(file) {
                metrics::inc_memory_recovery_validation_error();
                return Err(MemoryError::RecoveryValidation {
                    agent_id: agent_id.to_string(),
                    run_id: run_id.to_string(),
                    file: file.file_name().to_string(),
                });
            }
        }

        write_bundle_atomically(&memory_dir, run_id, after)?;
        self.refresh_cache(agent_id, after);

        metrics::inc_memory_publication("recovery");
        Ok(())
    }

    /// Updates the in-memory cache to the just-published bundle, recording the
    /// new mtimes so subsequent reads skip re-reading.
    fn refresh_cache(&self, agent_id: &str, bundle: &MemoryBundle) {
        let memory_dir = self.memory_dir(agent_id);
        let mtimes = file_mtimes(&memory_dir);
        let mut cache = self.cache.lock().expect("memory cache lock");
        cache.insert(
            agent_id.to_string(),
            CachedBundle {
                bundle: Arc::new(bundle.clone()),
                mtimes,
            },
        );
    }

    fn memory_dir(&self, agent_id: &str) -> PathBuf {
        self.agents_dir.join(agent_id).join("memory")
    }
}

/// Writes `bundle` to the memory dir via per-file temp files, fsync, and
/// rename, then fsyncs the directory. Temp files are named
/// `<file>.md.<run_id>.tmp` so concurrent runs do not collide.
fn write_bundle_atomically(
    memory_dir: &Path,
    run_id: &str,
    bundle: &MemoryBundle,
) -> Result<(), MemoryError> {
    std::fs::create_dir_all(memory_dir)?;

    // Best-effort sweep of stale temp files left by a crashed publication.
    // Same-run retries reuse the same temp names (File::create truncates), so
    // this only clears orphans from prior distinct runs.
    cleanup_stale_temp_files(memory_dir);

    for file in MemoryFile::ALL {
        let tmp_path = memory_dir.join(format!("{}.{}.tmp", file.file_name(), run_id));
        let content = bundle.file(file);
        let mut file_handle = std::fs::File::create(&tmp_path)?;
        std::io::Write::write_all(&mut file_handle, content.as_bytes())?;
        file_handle.sync_all()?;
    }

    // Rename after all temp files are written and synced so a crash before the
    // first rename leaves the old bundle intact.
    for file in MemoryFile::ALL {
        let tmp_path = memory_dir.join(format!("{}.{}.tmp", file.file_name(), run_id));
        let dest = memory_dir.join(file.file_name());
        std::fs::rename(&tmp_path, &dest)?;
    }

    sync_directory(memory_dir)?;
    Ok(())
}

/// fsyncs the directory so the rename operations survive a crash.
fn sync_directory(dir: &Path) -> Result<(), MemoryError> {
    let handle = std::fs::File::open(dir)?;
    // sync_all is preferred; if the platform rejects directory fsync, fall back
    // to sync_data rather than failing the whole publication.
    if let Err(error) = handle.sync_all() {
        warn!(error = %error, dir = %dir.display(), "directory sync_all failed; trying sync_data");
        handle.sync_data()?;
    }
    Ok(())
}

/// Removes orphaned `*.tmp` files from the memory dir. Best-effort: errors are
/// logged and never fail a publication, since a leftover temp file does not
/// affect correctness (only the three canonical files are ever read).
fn cleanup_stale_temp_files(memory_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(memory_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".tmp") {
            if let Err(error) = std::fs::remove_file(entry.path()) {
                warn!(
                    error = %error,
                    path = %entry.path().display(),
                    "could not remove stale memory temp file"
                );
            }
        }
    }
}

fn read_bundle(memory_dir: &Path) -> MemoryBundle {
    MemoryBundle {
        episodic: read_file_or_empty(&memory_dir.join(MemoryFile::Episodic.file_name())),
        semantic: read_file_or_empty(&memory_dir.join(MemoryFile::Semantic.file_name())),
        prospective: read_file_or_empty(&memory_dir.join(MemoryFile::Prospective.file_name())),
    }
}

fn read_file_or_empty(path: &Path) -> String {
    // Read raw content so it round-trips exactly through publish -> read,
    // which the publication precondition and recovery validation rely on.
    // Whitespace-only files collapse to empty so the prompt builder treats
    // them as "no memory" (preserving the old Option-based semantics).
    match std::fs::read_to_string(path) {
        Ok(content) if content.trim().is_empty() => String::new(),
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            warn!(error = %error, path = %path.display(), "memory file read failed; treating as empty");
            String::new()
        }
    }
}

/// Returns the mtime of each memory file, indexed parallel to
/// [`MemoryFile::ALL`]. `None` when the file is absent.
fn file_mtimes(memory_dir: &Path) -> [Option<SystemTime>; 3] {
    let mut mtimes = [None; 3];
    for (idx, file) in MemoryFile::ALL.iter().enumerate() {
        mtimes[idx] = std::fs::metadata(memory_dir.join(file.file_name()))
            .and_then(|m| m.modified())
            .ok();
    }
    mtimes
}

fn safe_agent_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && !id.contains("..")
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains(':')
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

    fn bundle_of(ep: &str, sem: &str, pro: &str) -> MemoryBundle {
        MemoryBundle {
            episodic: ep.to_string(),
            semantic: sem.to_string(),
            prospective: pro.to_string(),
        }
    }

    // --- load_bundle ---

    #[test]
    fn load_bundle_reads_all_three_files() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "a", "episodic.md", "ep");
        write_memory_file(dir.path(), "a", "semantic.md", "sem");
        write_memory_file(dir.path(), "a", "prospective.md", "pro");

        let loader = make_loader(dir.path());
        let bundle = loader.load_bundle("a").expect("bundle");

        assert_eq!(bundle.episodic, "ep");
        assert_eq!(bundle.semantic, "sem");
        assert_eq!(bundle.prospective, "pro");
    }

    #[test]
    fn load_bundle_returns_empty_when_no_memory_dir() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        let bundle = loader.load_bundle("a").expect("bundle");
        assert!(bundle.all_empty());
    }

    #[test]
    fn load_bundle_treats_missing_files_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "a", "episodic.md", "ep");

        let loader = make_loader(dir.path());
        let bundle = loader.load_bundle("a").expect("bundle");
        assert_eq!(bundle.episodic, "ep");
        assert!(bundle.semantic.is_empty());
        assert!(bundle.prospective.is_empty());
    }

    #[test]
    fn load_bundle_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        let err = loader.load_bundle("../etc").expect_err("should reject");
        assert!(matches!(err, MemoryError::UnsafeAgentId(_)));
    }

    #[test]
    fn load_bundle_caches_unchanged_files() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "a", "episodic.md", "cached");

        let loader = make_loader(dir.path());
        let first = loader.load_bundle("a").expect("first");
        let second = loader.load_bundle("a").expect("second");
        assert!(
            Arc::ptr_eq(&first, &second),
            "cached bundle should be shared"
        );
    }

    #[test]
    fn load_bundle_invalidates_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("agents")
            .join("a")
            .join("memory")
            .join("episodic.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "original").unwrap();

        let loader = make_loader(dir.path());
        let first = loader.load_bundle("a").expect("first");
        assert_eq!(first.episodic, "original");

        // Filesystems have ~1s mtime resolution; wait so the change is visible.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&path, "updated").unwrap();

        let second = loader.load_bundle("a").expect("second");
        assert_eq!(second.episodic, "updated");
        assert!(!Arc::ptr_eq(&first, &second));
    }

    // --- publish_bundle ---

    #[test]
    fn publish_bundle_writes_all_three_files_and_updates_cache() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let base = MemoryBundle::default();
        let candidate = bundle_of("new ep", "new sem", "new pro");

        loader
            .publish_bundle("a", "run-1", &base, &candidate)
            .expect("publish");

        let memory_dir = dir.path().join("agents").join("a").join("memory");
        assert_eq!(
            fs::read_to_string(memory_dir.join("episodic.md")).unwrap(),
            "new ep"
        );
        assert_eq!(
            fs::read_to_string(memory_dir.join("semantic.md")).unwrap(),
            "new sem"
        );
        assert_eq!(
            fs::read_to_string(memory_dir.join("prospective.md")).unwrap(),
            "new pro"
        );

        let loaded = loader.load_bundle("a").expect("load");
        assert_eq!(*loaded, candidate);
    }

    #[test]
    fn publish_bundle_detects_manual_edit_conflict() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "a", "episodic.md", "base ep");

        let loader = make_loader(dir.path());
        let base = bundle_of("base ep", "", "");

        // Simulate a manual edit between run start and publication.
        write_memory_file(dir.path(), "a", "episodic.md", "manually edited");

        let err = loader
            .publish_bundle("a", "run-1", &base, &bundle_of("new ep", "", ""))
            .expect_err("should conflict");
        assert!(matches!(err, MemoryError::Conflict { .. }));

        // Current files are left untouched (the manual edit remains).
        let memory_dir = dir.path().join("agents").join("a").join("memory");
        assert_eq!(
            fs::read_to_string(memory_dir.join("episodic.md")).unwrap(),
            "manually edited"
        );
    }

    #[test]
    fn publish_bundle_leaves_no_temp_files_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        loader
            .publish_bundle(
                "a",
                "run-1",
                &MemoryBundle::default(),
                &bundle_of("e", "s", "p"),
            )
            .expect("publish");

        let memory_dir = dir.path().join("agents").join("a").join("memory");
        let temps: Vec<_> = fs::read_dir(&memory_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(temps.is_empty(), "no temp files should remain");
    }

    #[test]
    fn publish_bundle_rejects_unsafe_agent_id() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());
        let err = loader
            .publish_bundle(
                "../etc",
                "run-1",
                &MemoryBundle::default(),
                &MemoryBundle::default(),
            )
            .expect_err("should reject");
        assert!(matches!(err, MemoryError::UnsafeAgentId(_)));
    }

    // --- recover_publication ---

    #[test]
    fn recover_publication_converges_to_after_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let before = MemoryBundle::default();
        let after = bundle_of("after ep", "after sem", "after pro");

        // No files exist yet → current == before (all empty).
        loader
            .recover_publication("a", "run-1", &before, &after)
            .expect("recover");

        let loaded = loader.load_bundle("a").expect("load");
        assert_eq!(*loaded, after);
    }

    #[test]
    fn recover_publication_accepts_partially_renamed_state() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let before = bundle_of("old ep", "old sem", "old pro");
        let after = bundle_of("new ep", "new sem", "new pro");

        // Simulate a crash after episodic was renamed to `after` but the others
        // are still `before`.
        write_memory_file(dir.path(), "a", "episodic.md", "new ep");
        write_memory_file(dir.path(), "a", "semantic.md", "old sem");
        write_memory_file(dir.path(), "a", "prospective.md", "old pro");

        loader
            .recover_publication("a", "run-1", &before, &after)
            .expect("recover");

        let loaded = loader.load_bundle("a").expect("load");
        assert_eq!(*loaded, after);
    }

    #[test]
    fn recover_publication_rejects_unknown_third_state() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let before = bundle_of("old ep", "", "");
        let after = bundle_of("new ep", "", "");

        // A content that matches neither before nor after.
        write_memory_file(dir.path(), "a", "episodic.md", "mystery");

        let err = loader
            .recover_publication("a", "run-1", &before, &after)
            .expect_err("should reject");
        assert!(matches!(err, MemoryError::RecoveryValidation { .. }));
    }

    #[test]
    fn recover_publication_accepts_content_with_trailing_whitespace() {
        // Regression guard: the on-disk file and the snapshot `after` must use
        // the same representation. Markdown naturally ends with a newline, so
        // recovery must not treat "content\n" (after) as different from the
        // trimmed read it would get if read_bundle trimmed.
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let before = MemoryBundle {
            episodic: String::new(),
            semantic: String::new(),
            prospective: String::new(),
        };
        let after = MemoryBundle {
            episodic: "# Episodic\n\n- entry\n".to_string(),
            semantic: "# Semantic\n".to_string(),
            prospective: String::new(),
        };

        // Simulate a crash after all three files were renamed to `after`.
        write_memory_file(dir.path(), "a", "episodic.md", "# Episodic\n\n- entry\n");
        write_memory_file(dir.path(), "a", "semantic.md", "# Semantic\n");

        loader
            .recover_publication("a", "run-1", &before, &after)
            .expect("recovery must succeed for whitespace-bearing content");

        let loaded = loader.load_bundle("a").expect("load");
        assert_eq!(loaded.episodic, "# Episodic\n\n- entry\n");
        assert_eq!(loaded.semantic, "# Semantic\n");
    }

    // --- concurrency: readers never observe a half-published bundle ---

    /// A torn read would mix generations (e.g. episodic="gen1-ep" with
    /// semantic="gen2-sem"). The per-agent RwLock serializes readers against
    /// the single writer, so every load must return a self-consistent bundle.
    #[test]
    fn concurrent_reads_during_publish_never_observe_mixed_generations() {
        use std::sync::Arc;
        use std::thread;

        let dir = Arc::new(tempfile::tempdir().unwrap());
        let loader = Arc::new(make_loader(dir.path()));

        // Seed generation 1.
        loader
            .publish_bundle(
                "a",
                "seed",
                &MemoryBundle::default(),
                &bundle_of("gen1-ep", "gen1-sem", "gen1-pro"),
            )
            .expect("seed publish");

        let writer = Arc::clone(&loader);
        let publish = thread::spawn(move || {
            // Publish generation 2.
            writer
                .publish_bundle(
                    "a",
                    "gen2",
                    &bundle_of("gen1-ep", "gen1-sem", "gen1-pro"),
                    &bundle_of("gen2-ep", "gen2-sem", "gen2-pro"),
                )
                .expect("gen2 publish");
        });

        // Hammer reads concurrently with the publish.
        let mut readers = Vec::new();
        for _ in 0..8 {
            let reader = Arc::clone(&loader);
            readers.push(thread::spawn(move || {
                for _ in 0..64 {
                    let bundle = reader.load_bundle("a").expect("load");
                    let generation = if bundle.episodic == "gen1-ep" {
                        1
                    } else if bundle.episodic == "gen2-ep" {
                        2
                    } else {
                        0
                    };
                    let semantic_gen = if bundle.semantic == "gen1-sem" {
                        1
                    } else if bundle.semantic == "gen2-sem" {
                        2
                    } else {
                        0
                    };
                    let prospective_gen = if bundle.prospective == "gen1-pro" {
                        1
                    } else if bundle.prospective == "gen2-pro" {
                        2
                    } else {
                        0
                    };
                    assert!(
                        generation == semantic_gen && generation == prospective_gen,
                        "torn read detected: {:?}",
                        bundle
                    );
                }
            }));
        }

        publish.join().expect("publish thread");
        for reader in readers {
            reader.join().expect("reader thread");
        }

        // Final state is generation 2.
        let final_bundle = loader.load_bundle("a").expect("final load");
        assert_eq!(*final_bundle, bundle_of("gen2-ep", "gen2-sem", "gen2-pro"));
    }
}

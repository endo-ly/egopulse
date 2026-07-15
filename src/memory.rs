//! Agent long-term memory: bundle loading, atomic publication, and crash recovery.
//!
//! The three memory files (`episodic.md`, `semantic.md`, `prospective.md`) are
//! treated as a single [`MemoryBundle`]. Reads ([`MemoryLoader::load_bundle`])
//! take a per-agent **read lock** and serve the published bundle from an
//! in-memory cache (disk is only touched on the first load, before the cache
//! is warm). Sleep publication ([`MemoryLoader::publish_bundle`]) takes the
//! per-agent **write lock** and refreshes both the on-disk files and the
//! in-memory cache, so a reader and a publisher can never run concurrently:
//! readers observe either the fully old or the fully new bundle, never an
//! in-flight mix.
//!
//! Publication replaces all three files via per-file temp-file + fsync +
//! rename. Because the renames are sequential, a crash between them can leave a
//! **mixed bundle on disk**; that on-disk state is never exposed to live
//! readers (the write lock), and [`MemoryLoader::recover_publication`] re-drives
//! the rename sequence from the persisted `memory_snapshots` on the next
//! startup so the run converges to the intended post-publication bundle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

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
    /// Filesystem failure while reading or writing a memory file. The path
    /// and the underlying [`std::io::Error`] are preserved so the error kind and
    /// source chain are not lost.
    #[error("memory_io_failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
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

/// Per-agent lock pair for memory access.
///
/// `io_lock` serializes readers against the single writer (publication) so a
/// reader never observes a half-published bundle. `cache` holds the most
/// recently published bundle as an `Arc` so the Turn hot path can serve it with
/// a cheap clone instead of disk I/O. Both are refreshed under `io_lock` write
/// during publication, and the cache is warmed lazily on the first read after
/// startup.
struct MemoryLock {
    io_lock: RwLock<()>,
    cache: RwLock<Option<Arc<MemoryBundle>>>,
}

/// Loads agent long-term memory bundles from `{agents_dir}/{agent_id}/memory/`
/// and publishes new bundles atomically.
///
/// A per-agent [`MemoryLock`] serializes readers against the single writer
/// (publication). The write lock is held only across file I/O and the cache
/// refresh, never across LLM generation, so a Turn can read the published
/// bundle while a Sleep run is still generating its candidate.
pub(crate) struct MemoryLoader {
    agents_dir: PathBuf,
    locks: Mutex<HashMap<String, Arc<MemoryLock>>>,
}

impl MemoryLoader {
    pub(crate) fn new(agents_dir: PathBuf) -> Self {
        Self {
            agents_dir,
            locks: Mutex::new(HashMap::new()),
        }
    }

    fn lock_for(&self, agent_id: &str) -> Arc<MemoryLock> {
        let mut locks = self.locks.lock().expect("memory locks map lock");
        locks
            .entry(agent_id.to_string())
            .or_insert_with(|| {
                Arc::new(MemoryLock {
                    io_lock: RwLock::new(()),
                    cache: RwLock::new(None),
                })
            })
            .clone()
    }

    /// Loads the current published memory bundle for `agent_id`.
    ///
    /// Takes the per-agent read lock and returns the published bundle from the
    /// in-memory cache. Disk is read only once, on the first load after startup
    /// (cache miss); once warmed, every subsequent load on the Turn hot path is
    /// an `Arc` clone with no disk I/O. The cache is refreshed under the write
    /// lock by [`MemoryLoader::publish_bundle`] /
    /// [`MemoryLoader::recover_publication`], so a reader always sees a
    /// self-consistent, fully-published bundle.
    ///
    /// Because the runtime holds the exclusive instance lock and is the sole
    /// writer, the cache is authoritative for the process lifetime. An external
    /// edit to the on-disk files is not reflected in turns until the next sleep
    /// publication (which would detect and refuse it via the precondition) or a
    /// restart.
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
        let _guard = lock.io_lock.read().expect("memory read lock");
        // Fast path: serve the cached published bundle without touching disk.
        if let Some(bundle) = lock.cache.read().expect("memory cache read lock").as_ref() {
            return Ok(Arc::clone(bundle));
        }
        // Cache miss (first load after startup): read once from disk and warm
        // the cache so subsequent loads stay off the Turn hot path.
        let bundle = Arc::new(read_bundle(&self.memory_dir(agent_id))?);
        *lock.cache.write().expect("memory cache write lock") = Some(Arc::clone(&bundle));
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
        let _guard = lock.io_lock.write().expect("memory write lock");
        metrics::inc_memory_publication("started");

        let memory_dir = self.memory_dir(agent_id);
        let current = read_bundle(&memory_dir)?;

        for file in MemoryFile::ALL {
            if current.file(file) != base.file(file) {
                metrics::inc_memory_publication("conflict");
                return Err(MemoryError::Conflict {
                    agent_id: agent_id.to_string(),
                    file: file.file_name().to_string(),
                });
            }
        }

        write_bundle_atomically(&memory_dir, &self.agents_dir, run_id, candidate)?;

        // Refresh the in-memory cache so subsequent loads observe the new
        // bundle without touching disk.
        *lock.cache.write().expect("memory cache write lock") = Some(Arc::new(candidate.clone()));
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
        let _guard = lock.io_lock.write().expect("memory write lock");

        let memory_dir = self.memory_dir(agent_id);
        let current = read_bundle(&memory_dir)?;

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

        write_bundle_atomically(&memory_dir, &self.agents_dir, run_id, after)?;

        // Refresh the cache to the recovered post-publication bundle.
        *lock.cache.write().expect("memory cache write lock") = Some(Arc::new(after.clone()));
        metrics::inc_memory_publication("recovery");
        Ok(())
    }

    fn memory_dir(&self, agent_id: &str) -> PathBuf {
        self.agents_dir.join(agent_id).join("memory")
    }
}

/// Writes `bundle` to the memory dir via per-file temp files, fsync, and
/// rename, then fsyncs the directory. Temp files are named
/// `<file>.md.<run_id>.tmp` so concurrent runs do not collide.
///
/// `agents_dir` is the loader root; when `memory_dir` (or any ancestor below
/// it) is newly created, those directory entries are fsynced too so a crash
/// immediately after a successful publication cannot lose the whole bundle.
fn write_bundle_atomically(
    memory_dir: &Path,
    agents_dir: &Path,
    run_id: &str,
    bundle: &MemoryBundle,
) -> Result<(), MemoryError> {
    let newly_created = !memory_dir.exists();
    std::fs::create_dir_all(memory_dir).map_err(io_err(memory_dir))?;
    if newly_created {
        sync_created_ancestors(memory_dir, agents_dir)?;
    }

    // Best-effort sweep of stale temp files left by a crashed publication.
    // Same-run retries reuse the same temp names (File::create truncates), so
    // this only clears orphans from prior distinct runs.
    cleanup_stale_temp_files(memory_dir);

    for file in MemoryFile::ALL {
        let tmp_path = memory_dir.join(format!("{}.{}.tmp", file.file_name(), run_id));
        let content = bundle.file(file);
        let mut file_handle = std::fs::File::create(&tmp_path).map_err(io_err(&tmp_path))?;
        std::io::Write::write_all(&mut file_handle, content.as_bytes())
            .map_err(io_err(&tmp_path))?;
        file_handle.sync_all().map_err(io_err(&tmp_path))?;
    }

    // Rename after all temp files are written and synced so a crash before the
    // first rename leaves the old bundle intact.
    for file in MemoryFile::ALL {
        let tmp_path = memory_dir.join(format!("{}.{}.tmp", file.file_name(), run_id));
        let dest = memory_dir.join(file.file_name());
        std::fs::rename(&tmp_path, &dest).map_err(io_err(&dest))?;
    }

    sync_directory(memory_dir)?;
    Ok(())
}

/// fsyncs the directory so the rename operations survive a crash.
fn sync_directory(dir: &Path) -> Result<(), MemoryError> {
    let handle = std::fs::File::open(dir).map_err(io_err(dir))?;
    // sync_all is preferred; if the platform rejects directory fsync, fall back
    // to sync_data rather than failing the whole publication.
    if let Err(error) = handle.sync_all() {
        warn!(error = %error, dir = %dir.display(), "directory sync_all failed; trying sync_data");
        handle.sync_data().map_err(io_err(dir))?;
    }
    Ok(())
}

/// fsyncs `memory_dir` and each ancestor up to and including `agents_dir` so
/// newly created directory entries survive a crash. Called only when
/// `memory_dir` did not exist before [`std::fs::create_dir_all`].
fn sync_created_ancestors(memory_dir: &Path, agents_dir: &Path) -> Result<(), MemoryError> {
    let mut current = Some(memory_dir);
    while let Some(dir) = current {
        sync_directory(dir)?;
        if dir == agents_dir {
            break;
        }
        current = dir.parent();
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

/// Builds a one-shot converter that attaches the offending `path` to an
/// [`std::io::Error`], so each blocking filesystem call site reports a
/// structured [`MemoryError::Io`] instead of collapsing the error into a
/// string (which would discard the error kind and source chain).
fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> MemoryError + '_ {
    move |source| MemoryError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn read_bundle(memory_dir: &Path) -> Result<MemoryBundle, MemoryError> {
    Ok(MemoryBundle {
        episodic: read_file_or_empty(&memory_dir.join(MemoryFile::Episodic.file_name()))?,
        semantic: read_file_or_empty(&memory_dir.join(MemoryFile::Semantic.file_name()))?,
        prospective: read_file_or_empty(&memory_dir.join(MemoryFile::Prospective.file_name()))?,
    })
}

/// Reads a memory file. `NotFound` (and whitespace-only content) collapse to
/// an empty string so a missing memory directory reads as "no memory". Any
/// other read failure (permissions, transient I/O) is propagated as
/// [`MemoryError::Io`] — swallowing it would let the publication precondition
/// compare against an empty bundle and overwrite unreadable existing content.
fn read_file_or_empty(path: &Path) -> Result<String, MemoryError> {
    match std::fs::read_to_string(path) {
        Ok(content) if content.trim().is_empty() => Ok(String::new()),
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(MemoryError::Io {
            path: path.to_path_buf(),
            source: error,
        }),
    }
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

    // --- publish_bundle ---

    #[test]
    fn publish_bundle_writes_all_three_files() {
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

//! Turn execution runtime: narrows [`AppState`] to the fields a [`TurnExecutor`]
//! actually needs.
//!
//! All Turn execution paths (Agent loop, Prompt builder, Compaction, Tool
//! phase, Session persistence) receive `&TurnRuntime` instead of `&AppState`,
//! eliminating accidental dependency on scheduling / channel / observability
//! state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::agent_loop::ConversationScope;
use crate::config::{Config, ConfigManager, ResolvedLlmConfig};
use crate::error::EgoPulseError;
use crate::llm::LlmProvider;
use crate::memory::MemoryLoader;
use crate::runtime::ScopedStorage;
use crate::runtime::turn_scheduler::ActiveTurnTracker;
use crate::skills::SkillManager;
use crate::storage::Database;
use crate::tools::ToolRegistry;

/// Narrow dependency bundle for Turn execution.
///
/// Constructed once per Turn via [`crate::runtime::AppState::turn_runtime`].
/// Holds only the services that participate in model/tool loop execution,
/// leaving scheduling (`TurnScheduler`, `TurnTracker`, `ActiveTurns`),
/// channel dispatch (`ChannelRegistry`), and runtime observability
/// (`RuntimeStatus`) on the caller side.
pub(crate) struct TurnRuntime {
    pub(crate) db: Arc<Database>,
    pub(crate) secret_db: Option<Arc<Database>>,
    pub(crate) config_manager: Arc<ConfigManager>,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) llm_override: Option<Arc<dyn LlmProvider>>,
    pub(crate) llm_cache: Arc<Mutex<HashMap<u64, Arc<dyn LlmProvider>>>>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) skills: Arc<SkillManager>,
    pub(crate) soul_agents: Arc<crate::agent_loop::soul_agents::SoulAgentsLoader>,
    pub(crate) memory_loader: Arc<MemoryLoader>,
    pub(crate) assets: Arc<crate::assets::AssetStore>,
    pub(crate) usage_calibrator: Arc<crate::llm::calibration::UsageCalibrator>,
    pub(crate) turn_sender: tokio::sync::mpsc::Sender<crate::agent_loop::PendingAgentTurn>,
    pub(crate) active_turns: Arc<ActiveTurnTracker>,
}

impl TurnRuntime {
    /// Returns the appropriate `Database` reference based on `scope`.
    ///
    /// # Panics
    ///
    /// Panics if `scope` is `Secret` but `secret_db` was not initialized.
    pub(crate) fn db_for(&self, scope: ConversationScope) -> &Arc<Database> {
        match scope {
            ConversationScope::Normal => &self.db,
            ConversationScope::Secret => self
                .secret_db
                .as_ref()
                .expect("secret db required but not initialized"),
        }
    }

    /// Returns the current validated [`Config`] snapshot.
    pub(crate) fn current_config(&self) -> Arc<Config> {
        Arc::new(self.config_manager.current_blocking().config.clone())
    }

    /// Resolves storage endpoints (database + archive root) for a scope.
    ///
    /// # Panics
    ///
    /// Panics if `scope` is `Secret` but `secret_db` was not initialized.
    pub(crate) fn storage_for(&self, scope: ConversationScope) -> ScopedStorage<'_> {
        let snapshot = self.config_manager.current_blocking();
        let config = &snapshot.config;
        match scope {
            ConversationScope::Normal => ScopedStorage {
                db: &self.db,
                archive_root: config.groups_dir(),
            },
            ConversationScope::Secret => ScopedStorage {
                db: self
                    .secret_db
                    .as_ref()
                    .expect("secret db required but not initialized"),
                archive_root: config.runtime_dir().join("secret_groups"),
            },
        }
    }

    /// Returns the LLM provider using the provided immutable Config snapshot.
    ///
    /// Callers inside a Turn should use this so the entire Turn runs against a
    /// single fixed Config generation.
    pub(crate) fn llm_for_context_with_snapshot(
        &self,
        context: &crate::agent_loop::SurfaceContext,
        snapshot: &crate::config::manager::ConfigSnapshot,
    ) -> Result<Arc<dyn LlmProvider>, EgoPulseError> {
        if let Some(provider) = self.llm_override.clone() {
            return Ok(provider);
        }

        let agent_id = crate::config::AgentId::new(&context.agent_id);
        let resolved = snapshot
            .config
            .resolve_llm_for_agent_channel(&agent_id, &context.channel)?;
        self.cached_provider(&resolved, snapshot.revision)
    }

    pub(crate) fn cached_provider(
        &self,
        resolved: &ResolvedLlmConfig,
        config_revision: u64,
    ) -> Result<Arc<dyn LlmProvider>, EgoPulseError> {
        let key = resolved.cache_key_with_revision(config_revision);
        let mut cache = self.llm_cache.lock().expect("llm_cache lock");
        if let Some(provider) = cache.get(&key) {
            return Ok(Arc::clone(provider));
        }
        let provider: Arc<dyn LlmProvider> = Arc::from(crate::llm::create_provider(resolved)?);
        cache.insert(key, Arc::clone(&provider));
        Ok(provider)
    }
}

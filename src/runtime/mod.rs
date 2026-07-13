//! EgoPulse ランタイム全体の依存を組み立てるモジュール。
//!
//! `AppState` の構築、単発 LLM 実行、各チャネルの起動と監視を提供する。

pub(crate) mod backup_scheduler;
pub(crate) mod channel_input;
pub mod gateway;
pub mod logging;
pub(crate) mod metrics;
pub(crate) mod runtime_status;
pub(crate) mod status;
pub(crate) mod supervisor;
pub(crate) mod tool_progress;
pub(crate) mod turn_scheduler;

pub(crate) use channel_input::{
    ChannelLogKey, HumanChannelLogMessage, build_channel_context, channel_scope_from_secret,
    store_human_channel_log_message, submit_agent_turn,
};
pub(crate) use runtime_status::ChannelState;
pub(crate) use runtime_status::RuntimeStatus;
pub(crate) use supervisor::Criticality;
pub(crate) use supervisor::InstanceGuard;
pub(crate) use supervisor::RuntimeSupervisor;
pub(crate) use supervisor::TaskKind;
pub(crate) use supervisor::TaskSpec;
pub(crate) use turn_scheduler::ActiveTurnTracker;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agent_loop::ConversationScope;
use crate::agent_loop::deserialize_scheduled_turn;
use crate::agent_loop::soul_agents::SoulAgentsLoader;
use crate::assets::AssetStore;
use crate::channels;
use crate::channels::adapter::ChannelRegistry;
use crate::channels::voice::VoiceAdapter;
use crate::channels::web::WebAdapter;
use crate::config::{Config, ConfigManager};
use crate::error::{ChannelError, EgoPulseError};
use crate::llm::calibration::{CalibrationKey, CalibrationObservation, UsageCalibrator};
use crate::llm::{Message, create_provider};
use crate::memory::MemoryLoader;
use crate::skills::SkillManager;
use crate::storage::Database;
use crate::storage::call_blocking;
use crate::tools::ToolRegistry;

/// Holds the shared runtime dependencies used across all channels.
#[derive(Clone)]
pub struct AppState {
    pub(crate) db: Arc<Database>,
    /// Secret DB for isolated secret-mode storage. `None` when no secret channels are configured.
    pub(crate) secret_db: Option<Arc<Database>>,
    pub(crate) config: Config,
    pub(crate) config_manager: Arc<ConfigManager>,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) llm_override: Option<Arc<dyn crate::llm::LlmProvider>>,
    pub(crate) channels: Arc<ChannelRegistry>,
    pub(crate) skills: Arc<SkillManager>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) mcp_manager: Option<Arc<tokio::sync::RwLock<crate::tools::mcp::McpManager>>>,
    pub(crate) assets: Arc<AssetStore>,
    pub(crate) soul_agents: Arc<SoulAgentsLoader>,
    pub(crate) memory_loader: Arc<MemoryLoader>,
    pub(crate) llm_cache: Arc<Mutex<HashMap<u64, Arc<dyn crate::llm::LlmProvider>>>>,
    /// Tracks in-flight conversation turns per agent for scheduler active-agent detection.
    pub(crate) active_turns: Arc<ActiveTurnTracker>,
    /// Sender half of the pending-agent-turn channel for `agent_send` turn queuing.
    pub(crate) turn_sender: tokio::sync::mpsc::Sender<crate::agent_loop::PendingAgentTurn>,
    /// Per-session turn scheduler for concurrency control and ordered execution.
    pub(crate) turn_scheduler: Arc<turn_scheduler::TurnScheduler>,
    /// Per-origin turn counter for runaway prevention.
    pub(crate) turn_tracker: Arc<turn_scheduler::TurnTracker>,
    /// In-memory runtime health summary for observability.
    pub(crate) runtime_status: Arc<RuntimeStatus>,
    /// Owns long-lived tasks and in-flight turns; orchestrates shutdown.
    pub(crate) supervisor: Arc<RuntimeSupervisor>,
    /// Learns prompt-token estimate correction factors from observed LLM usage.
    pub(crate) usage_calibrator: Arc<UsageCalibrator>,
    _sealed: (),
}

pub(crate) struct AppStateParts {
    pub(crate) db: Arc<Database>,
    pub(crate) secret_db: Option<Arc<Database>>,
    pub(crate) config: Config,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) llm_override: Option<Arc<dyn crate::llm::LlmProvider>>,
    pub(crate) channels: Arc<ChannelRegistry>,
    pub(crate) skills: Arc<SkillManager>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) mcp_manager: Option<Arc<tokio::sync::RwLock<crate::tools::mcp::McpManager>>>,
    pub(crate) assets: Arc<AssetStore>,
    pub(crate) soul_agents: Arc<SoulAgentsLoader>,
    pub(crate) memory_loader: Arc<MemoryLoader>,
    pub(crate) turn_sender: tokio::sync::mpsc::Sender<crate::agent_loop::PendingAgentTurn>,
    pub(crate) runtime_status: Arc<RuntimeStatus>,
    pub(crate) instance_guard: Arc<InstanceGuard>,
}

struct AppStateDependencies {
    db: Arc<Database>,
    secret_db: Option<Arc<Database>>,
    assets: Arc<AssetStore>,
    skills: Arc<SkillManager>,
    soul_agents: Arc<SoulAgentsLoader>,
    memory_loader: Arc<MemoryLoader>,
}

/// Resolved storage endpoints for a conversation scope.
///
/// Groups the database handle and archive root path so callers do not
/// need to know scope-specific path conventions.
pub(crate) struct ScopedStorage<'a> {
    /// The database handle for this scope.
    pub db: &'a Arc<Database>,
    /// Root directory for archived conversations.
    pub archive_root: PathBuf,
}

impl AppState {
    pub(crate) fn from_parts(parts: AppStateParts) -> Self {
        Self {
            db: parts.db,
            secret_db: parts.secret_db,
            config: parts.config.clone(),
            config_manager: Arc::new(ConfigManager::new(
                parts.config,
                parts.config_path.as_deref(),
            )),
            config_path: parts.config_path,
            llm_override: parts.llm_override,
            channels: parts.channels,
            skills: parts.skills,
            tools: parts.tools,
            mcp_manager: parts.mcp_manager,
            assets: parts.assets,
            soul_agents: parts.soul_agents,
            memory_loader: parts.memory_loader,
            llm_cache: Arc::new(Mutex::new(HashMap::new())),
            active_turns: Arc::new(ActiveTurnTracker::new()),
            turn_sender: parts.turn_sender,
            turn_scheduler: Arc::new(turn_scheduler::TurnScheduler::new()),
            turn_tracker: Arc::new(turn_scheduler::TurnTracker::new()),
            runtime_status: parts.runtime_status.clone(),
            supervisor: Arc::new(RuntimeSupervisor::with_instance_guard(
                parts.runtime_status,
                Arc::clone(&parts.instance_guard),
            )),
            usage_calibrator: Arc::new(UsageCalibrator::new()),
            _sealed: (),
        }
    }

    /// Builds a [`crate::agent_loop::TurnRuntime`] from the subset of this
    /// `AppState` that Turn execution actually needs.
    ///
    /// Scheduling, channel dispatch, and runtime observability fields are
    /// intentionally omitted so that Turn logic cannot accidentally depend on
    /// them.
    pub(crate) fn turn_runtime(&self) -> crate::agent_loop::TurnRuntime {
        crate::agent_loop::TurnRuntime {
            db: Arc::clone(&self.db),
            secret_db: self.secret_db.clone(),
            config_manager: Arc::clone(&self.config_manager),
            config_path: self.config_path.clone(),
            llm_override: self.llm_override.clone(),
            llm_cache: Arc::clone(&self.llm_cache),
            tools: Arc::clone(&self.tools),
            skills: Arc::clone(&self.skills),
            soul_agents: Arc::clone(&self.soul_agents),
            memory_loader: Arc::clone(&self.memory_loader),
            assets: Arc::clone(&self.assets),
            usage_calibrator: Arc::clone(&self.usage_calibrator),
            turn_sender: self.turn_sender.clone(),
            active_turns: Arc::clone(&self.active_turns),
        }
    }

    /// Returns the appropriate `Database` reference based on `scope`.
    ///
    /// # Panics
    ///
    /// Panics if `scope` is `Secret` but `secret_db` was not initialized
    /// (i.e., no secret channels in config).
    pub(crate) fn db_for(&self, scope: ConversationScope) -> &Arc<Database> {
        match scope {
            ConversationScope::Normal => &self.db,
            ConversationScope::Secret => self
                .secret_db
                .as_ref()
                .expect("secret db required but not initialized"),
        }
    }

    /// Rebuilds calibration factors from persisted usage observations.
    ///
    /// Loads recent observations from both normal and secret databases and
    /// replays them through the calibrator so learned factors survive restarts.
    /// Observations are merged in chronological order so shared
    /// [`CalibrationKey`](crate::llm::calibration::CalibrationKey)s replay their
    /// true history. Load failures fall back to whatever was loaded (possibly
    /// empty), leaving unmeasured keys at `DEFAULT_FACTOR`.
    pub(crate) async fn warm_up_calibrator(&self) {
        const REPLAY_LIMIT_PER_KEY: usize = 30;
        let mut observations = match crate::storage::call_blocking(Arc::clone(&self.db), |db| {
            db.load_calibration_observations(REPLAY_LIMIT_PER_KEY)
        })
        .await
        {
            Ok(o) => o,
            Err(e) => {
                warn!(error = %e, "calibration load failed (normal db); using defaults");
                Vec::new()
            }
        };
        if let Some(secret_db) = &self.secret_db {
            match crate::storage::call_blocking(Arc::clone(secret_db), |db| {
                db.load_calibration_observations(REPLAY_LIMIT_PER_KEY)
            })
            .await
            {
                Ok(o) => observations.extend(o),
                Err(e) => warn!(error = %e, "calibration load failed (secret db); using defaults"),
            }
        }
        // Each database already applied the per-key cap; re-cap after merging
        // so a key present in both databases still replays at most N.
        Self::cap_observations_per_key(&mut observations, REPLAY_LIMIT_PER_KEY);
        self.usage_calibrator.replay(&observations).await;
    }

    /// Keeps at most `limit_per_key` observations per [`CalibrationKey`], then
    /// restores oldest-first order for chronological EMA replay. Applied after
    /// merging normal and secret observations so a key present in both still
    /// replays at most N entries.
    fn cap_observations_per_key(
        observations: &mut Vec<CalibrationObservation>,
        limit_per_key: usize,
    ) {
        use std::collections::HashMap;
        observations.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let mut counts: HashMap<CalibrationKey, usize> = HashMap::new();
        observations.retain(|o| {
            let key = CalibrationKey {
                provider: o.provider.clone(),
                model: o.model.clone(),
                request_kind: o.request_kind.clone(),
                has_tools: o.has_tools,
            };
            let count = counts.entry(key).or_insert(0);
            if *count < limit_per_key {
                *count += 1;
                true
            } else {
                false
            }
        });
        observations.reverse();
    }

    /// 現在の設定スナップショットを返す。
    pub fn current_config(&self) -> Arc<Config> {
        Arc::new(self.config.clone())
    }

    /// 設定ファイルパスがある場合はディスクから再読込した最新設定を返す。
    pub fn try_current_config(&self) -> Result<Arc<Config>, EgoPulseError> {
        match self.config_path.as_deref() {
            Some(path) => Ok(Arc::new(Config::load_allow_missing_api_key(Some(path))?)),
            None => Ok(self.current_config()),
        }
    }

    /// Returns the LLM provider resolved for the agent and channel in the given context.
    pub(crate) fn llm_for_context(
        &self,
        context: &crate::agent_loop::SurfaceContext,
    ) -> Result<Arc<dyn crate::llm::LlmProvider>, EgoPulseError> {
        if let Some(provider) = self.llm_override.clone() {
            return Ok(provider);
        }

        let snapshot = self.config_manager.current_blocking();
        let config = &snapshot.config;
        let agent_id = crate::config::AgentId::new(&context.agent_id);
        let resolved = config.resolve_llm_for_agent_channel(&agent_id, &context.channel)?;
        self.cached_provider(&resolved, snapshot.revision)
    }

    pub(crate) fn cached_provider(
        &self,
        resolved: &crate::config::ResolvedLlmConfig,
        config_revision: u64,
    ) -> Result<Arc<dyn crate::llm::LlmProvider>, EgoPulseError> {
        let key = resolved.cache_key_with_revision(config_revision);
        let mut cache = self.llm_cache.lock().expect("llm_cache lock");
        if let Some(provider) = cache.get(&key) {
            return Ok(Arc::clone(provider));
        }
        let provider: Arc<dyn crate::llm::LlmProvider> = Arc::from(create_provider(resolved)?);
        cache.insert(key, Arc::clone(&provider));
        Ok(provider)
    }
}

/// Builds the application state without recording a config file path.
pub async fn build_app_state(config: Config) -> Result<AppState, EgoPulseError> {
    build_app_state_with_path(config, None).await
}

/// Builds the application state and keeps the config path for later saves.
pub async fn build_app_state_with_path(
    config: Config,
    config_path: Option<PathBuf>,
) -> Result<AppState, EgoPulseError> {
    crate::runtime::metrics::init_metrics();

    // Acquire the exclusive instance lock before the database is opened, so a
    // concurrent runtime or manual `sleep` command against this state root is
    // rejected up front.
    let instance_guard = InstanceGuard::acquire(Path::new(&config.state_root))?;
    metrics::set_instance_lock_held(true);

    let deps = build_app_state_dependencies(&config, ProvisionDefaultSoul::Yes)?;

    let mut channels = ChannelRegistry::new();
    channels.register(Arc::new(WebAdapter));
    if config.voice_enabled() {
        channels.register(Arc::new(VoiceAdapter));
    }

    #[cfg(feature = "channel-discord")]
    if !config.discord_bots().is_empty() {
        channels.register(Arc::new(
            crate::channels::discord::DiscordAdapter::new_for_bots(&config),
        ));
    }

    #[cfg(feature = "channel-telegram")]
    if !config.telegram_bots().is_empty() {
        let bot_tokens: std::collections::HashMap<String, String> = config
            .telegram_bots()
            .into_iter()
            .map(|b| (b.bot_id.to_string(), b.token.to_string()))
            .collect();
        let agent_bots: std::collections::HashMap<String, String> = config
            .agents
            .iter()
            .filter_map(|(agent_id, agent)| {
                let bot_id = agent.telegram_bot.as_ref()?;
                Some((agent_id.to_string(), bot_id.to_string()))
            })
            .collect();
        channels.register(Arc::new(
            crate::channels::telegram::TelegramAdapter::new_multi(bot_tokens, agent_bots),
        ));
    }

    let channels = Arc::new(channels);
    let mut tools = ToolRegistry::new(&config, Arc::clone(&deps.skills));

    let workspace_dir = config.workspace_dir()?;
    let mcp_manager = crate::tools::mcp::McpManager::new(&workspace_dir).await?;
    let mcp_arc = Arc::new(tokio::sync::RwLock::new(mcp_manager));
    tools.set_mcp_manager(Arc::clone(&mcp_arc));

    tools.register_tool(Box::new(crate::tools::SendMessageTool::new(
        workspace_dir.clone(),
        Arc::clone(&channels),
        Arc::clone(&deps.db),
        deps.secret_db.clone(),
    )));

    let (turn_sender, turn_receiver) =
        tokio::sync::mpsc::channel::<crate::agent_loop::PendingAgentTurn>(16);

    tools.register_tool(Box::new(crate::tools::AgentSendTool::new(
        config.agents.clone(),
        Arc::clone(&deps.db),
        deps.secret_db.clone(),
        Arc::clone(&channels),
    )));

    let tools = Arc::new(tools);

    let runtime_status = Arc::new(RuntimeStatus::new());

    let state = AppState::from_parts(AppStateParts {
        db: deps.db,
        secret_db: deps.secret_db,
        config,
        config_path,
        llm_override: None,
        channels,
        skills: deps.skills,
        tools,
        mcp_manager: Some(mcp_arc),
        assets: deps.assets,
        soul_agents: deps.soul_agents,
        memory_loader: deps.memory_loader,
        turn_sender,
        runtime_status: Arc::clone(&runtime_status),
        instance_guard: Arc::clone(&instance_guard),
    });
    state.warm_up_calibrator().await;

    // Recover durable Turn / Tool state left non-terminal by a prior crash.
    // Running Tools become uncertain, and interrupted turn_runs stop safely.
    recover_durable_state(&state).await;

    // Own long-lived background tasks through the supervisor so their lifetime
    // and shutdown are centrally managed.
    let mcp_arc = state
        .mcp_manager
        .as_ref()
        .expect("mcp manager initialized")
        .clone();
    state.supervisor.spawn_long_lived(
        TaskSpec::new(
            TaskKind::McpReconnect,
            "mcp-reconnect",
            Criticality::NonCritical,
        ),
        spawn_mcp_reconnect_loop(
            mcp_arc,
            workspace_dir.clone(),
            state.supervisor.shutdown_token(),
        ),
    );

    spawn_agent_turn_worker(
        state.clone(),
        turn_receiver,
        state.supervisor.shutdown_token(),
    );

    spawn_turn_dispatcher(state.clone(), state.supervisor.shutdown_token());

    Ok(state)
}

/// Recovers durable Turn / Tool state on startup.
///
/// Runs against both the primary and (when present) secret databases:
/// `running` tool_calls are transitioned by [`Database::recover_running_tools`],
/// and non-terminal `turn_runs` are stopped safely by [`Database::recover_interrupted_turns`].
/// Failures are logged but never abort startup so the runtime can still serve
/// new turns.
async fn recover_durable_state(state: &AppState) {
    recover_durable_state_for_db(&state.db, ConversationScope::Normal);
    if let Some(secret_db) = &state.secret_db {
        recover_durable_state_for_db(secret_db, ConversationScope::Secret);
    }
}

fn recover_durable_state_for_db(db: &Arc<Database>, scope: ConversationScope) {
    match db.as_ref().recover_running_tools() {
        Ok(recovered) if !recovered.is_empty() => {
            for tool in &recovered {
                tracing::info!(
                    scope = %scope,
                    turn_id = %tool.turn_id,
                    tool_call_id = %tool.tool_call_id,
                    tool_name = %tool.tool_name,
                    from = "running",
                    to = %tool.recovered_to,
                    "recovered tool_call on startup"
                );
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, scope = %scope, "tool_calls recovery failed"),
    }
    match db.as_ref().recover_interrupted_turns() {
        Ok(recovered) if !recovered.is_empty() => {
            for turn in &recovered {
                tracing::info!(
                    scope = %scope,
                    turn_id = %turn.turn_id,
                    chat_id = turn.chat_id,
                    from = %turn.from,
                    to = %turn.recovered_to,
                    "recovered turn_run on startup"
                );
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, scope = %scope, "turn_runs recovery failed"),
    }
}

/// Builds the minimal application state needed for manual sleep batch execution.
///
/// Sleep batch does not execute agent tools or channels, so this intentionally
/// avoids MCP initialization and the reconnect loop.
pub fn build_sleep_app_state_with_path(
    config: Config,
    config_path: Option<PathBuf>,
) -> Result<AppState, EgoPulseError> {
    // Acquire the exclusive instance lock before the database is opened, so a
    // concurrent runtime against this state root is rejected.
    let instance_guard = InstanceGuard::acquire(Path::new(&config.state_root))?;
    metrics::set_instance_lock_held(true);

    let deps = build_app_state_dependencies(&config, ProvisionDefaultSoul::No)?;
    let channels = Arc::new(ChannelRegistry::new());
    let tools = Arc::new(ToolRegistry::new(&config, Arc::clone(&deps.skills)));

    let runtime_status = Arc::new(RuntimeStatus::new());

    Ok(AppState::from_parts(AppStateParts {
        db: deps.db,
        secret_db: deps.secret_db,
        config,
        config_path,
        llm_override: None,
        channels,
        skills: deps.skills,
        tools,
        mcp_manager: None,
        assets: deps.assets,
        soul_agents: deps.soul_agents,
        memory_loader: deps.memory_loader,
        turn_sender: tokio::sync::mpsc::channel(16).0,
        runtime_status,
        instance_guard,
    }))
}

enum ProvisionDefaultSoul {
    Yes,
    No,
}

fn build_app_state_dependencies(
    config: &Config,
    provision_default_soul: ProvisionDefaultSoul,
) -> Result<AppStateDependencies, EgoPulseError> {
    let backup_settings = crate::storage::BackupSettings {
        enabled: config.db.backup.enabled,
        dest_dir: config.backup_dir(),
        max_generations: config.db.backup.max_generations,
        tz: config.timezone.clone(),
        now: chrono::Utc::now(),
    };
    let db = Arc::new(Database::new_with_backup(
        &config.db_path(),
        &backup_settings,
    )?);

    let secret_db = if config.needs_secret_db() {
        Some(Arc::new(Database::new_secret_with_backup(
            &config.secret_db_path(),
            &backup_settings,
        )?))
    } else {
        None
    };
    let assets = Arc::new(AssetStore::new(&config.assets_dir())?);

    if let Err(error) = crate::builtin_skills::expand_builtin_skills(Path::new(&config.state_root))
    {
        tracing::warn!("failed to expand built-in skills: {error}");
    }

    let skills = Arc::new(SkillManager::from_dirs(
        config.user_skills_dir()?,
        config.skills_dir()?,
    ));
    let soul_agents = Arc::new(SoulAgentsLoader::new(config));
    if matches!(provision_default_soul, ProvisionDefaultSoul::Yes) {
        if let Err(error) = soul_agents.provision_default_soul() {
            tracing::warn!("failed to provision default SOUL.md: {error}");
        }
    }
    let memory_loader = Arc::new(MemoryLoader::new(
        PathBuf::from(&config.state_root).join("agents"),
    ));

    Ok(AppStateDependencies {
        db,
        secret_db,
        assets,
        skills,
        soul_agents,
        memory_loader,
    })
}

fn spawn_agent_turn_worker(
    state: AppState,
    mut receiver: tokio::sync::mpsc::Receiver<crate::agent_loop::PendingAgentTurn>,
    shutdown: CancellationToken,
) {
    let supervisor = Arc::clone(&state.supervisor);
    supervisor.spawn_long_lived(
        TaskSpec::new(
            TaskKind::AgentTurnWorker,
            "agent-turn-worker",
            Criticality::Critical,
        ),
        async move {
            loop {
                let pending = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    msg = receiver.recv() => match msg {
                        Some(p) => p,
                        None => break,
                    },
                };
                let channel_log_chat_id = pending.context.channel_log_chat_id;
                let scope = pending.context.scope;
                let scheduled = crate::agent_loop::ScheduledTurn {
                    context: pending.context,
                    input: pending.input,
                    origin_id: pending.origin_id,
                };

                if let turn_scheduler::SubmitOutcome::Rejected(reason) =
                    channel_input::submit_scheduled_turn(&state, scheduled).await
                {
                    // Log + metric are already recorded centrally in the submit
                    // path; store a system event so the async rejection surfaces in
                    // the channel log instead of being silently dropped.
                    if let Some(chat_id) = channel_log_chat_id {
                        if let Err(error) = state.db_for(scope).store_system_event(chat_id, &reason)
                        {
                            tracing::warn!(
                                error = %error,
                                "failed to store system event for agent_send queue rejection"
                            );
                        }
                    }
                }
            }
            Ok(())
        },
    );
}

/// Spawns the turn dispatcher: a long-lived supervisor task that periodically
/// scans both databases for durably accepted turns (request persisted but never
/// started) and re-submits them so a crash before execution is fully recovered
fn spawn_turn_dispatcher(state: AppState, shutdown: CancellationToken) {
    let supervisor = Arc::clone(&state.supervisor);
    supervisor.spawn_long_lived(
        TaskSpec::new(
            TaskKind::TurnDispatcher,
            "turn-dispatcher",
            Criticality::Critical,
        ),
        async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
                if let Err(error) = dispatch_durable_turns(&state).await {
                    tracing::warn!(error = %error, "turn dispatcher scan failed");
                }
            }
            Ok(())
        },
    );
}

/// Re-enqueues any durably accepted turns found in the databases. Each is
/// rebuilt from its persisted request and re-submitted; the executor re-accepts
/// the existing row idempotently, so a turn already running is a no-op.
async fn dispatch_durable_turns(state: &AppState) -> Result<(), EgoPulseError> {
    let scopes: &[ConversationScope] = if state.secret_db.is_some() {
        &[ConversationScope::Normal, ConversationScope::Secret]
    } else {
        &[ConversationScope::Normal]
    };
    for &scope in scopes {
        let db = Arc::clone(state.db_for(scope));
        let ids = call_blocking(Arc::clone(&db), |db| db.scan_durable_accepted_turn_ids())
            .await
            .map_err(EgoPulseError::from)?;
        for turn_id in ids {
            let json = match call_blocking(Arc::clone(&db), {
                let turn_id = turn_id.clone();
                move |db| {
                    db.get_turn_run(&turn_id)
                        .map(|run| run.scheduled_request_json)
                }
            })
            .await
            {
                Ok(Some(json)) => json,
                _ => continue,
            };
            let turn = match deserialize_scheduled_turn(&json) {
                Ok(turn) => turn,
                Err(error) => {
                    tracing::warn!(error = %error, turn_id, "failed to deserialize durable turn");
                    continue;
                }
            };
            // Re-enqueue. Bypasses dedupe so the recovered `accepted` turn is
            // actually scheduled; the executor finds the existing row and
            // proceeds instead of creating a duplicate.
            let _ = channel_input::enqueue_durable_turn(state, turn);
        }
    }
    Ok(())
}

pub(crate) fn execute_scheduled_turn(
    state: &AppState,
    turn: crate::agent_loop::ScheduledTurn,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
    Box::pin(async move {
        let trace_id = uuid::Uuid::new_v4().to_string();
        let mut turn = turn;
        turn.context.trace_id = trace_id;

        let session_key = turn.session_key();
        let origin_id = if turn.origin_id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            turn.origin_id.clone()
        };

        state
            .runtime_status
            .touch_channel_activity(&turn.context.channel);

        // Shutdown may have begun between acceptance and execution start. Once
        // shutdown is in progress, no new turn is started: release the
        // reservation and let the in-flight task complete so the supervisor can
        // drain it. This closes the race between the intake gate and execution.
        if state.supervisor.is_shutting_down() {
            tracing::info!(
                agent_id = %turn.context.agent_id,
                origin_id = %origin_id,
                "shutdown in progress: not starting submitted turn"
            );
            state.turn_tracker.release(&origin_id);
            return;
        }

        // The origin was already reserved at acceptance (submit_scheduled_turn).
        // Re-check here for queued turns whose chain terminated while waiting in
        // the scheduler; release the reservation so capacity is not leaked.
        if let Some(reason) = state.turn_tracker.terminal_reason(&origin_id) {
            tracing::warn!(
                agent_id = %turn.context.agent_id,
                origin_id = %origin_id,
                reason = ?reason,
                "dropping turn: origin already has terminal stop reason"
            );
            state.turn_tracker.release(&origin_id);
            drain_next_queued_turn(state, &session_key).await;
            return;
        }

        let valid_ids: Vec<&str> = state.config.agents.keys().map(|id| id.as_str()).collect();
        let chain_depth = turn.context.chain_depth;
        let agent_id = &turn.context.agent_id;

        // Atomically gate the turn and commit it to execution in a single
        // tracker lock: the per-chain turn-count check and the executed-count
        // increment happen together, so two concurrent turns for the same origin
        // (different agent sessions share an origin) cannot both pass the limit.
        // try_begin_execution consumes the reservation on both paths, so it must
        // not be paired with a release() here. A turn whose chain already
        // terminated while queued is dropped quietly by the re-check above; a
        // new rejection records the terminal reason inside the call.
        match state
            .turn_tracker
            .try_begin_execution(&origin_id, chain_depth, agent_id, &valid_ids)
        {
            Ok(_executed_turn_count) => {}
            Err(reason) => {
                tracing::warn!(
                    agent_id = %agent_id,
                    chain_depth,
                    reason = ?reason,
                    "scheduled turn rejected by stop condition evaluator"
                );
                state.runtime_status.push_error(
                    &turn.context.trace_id,
                    "stop_condition",
                    agent_id,
                    &turn.context.channel,
                    &format!("{reason:?}"),
                );
                crate::runtime::metrics::inc_turn_errors_total("stop_condition", agent_id);
                if let Some(log_chat_id) = turn.context.channel_log_chat_id {
                    if let Err(error) = state
                        .db_for(turn.context.scope)
                        .store_system_event(log_chat_id, &reason)
                    {
                        tracing::warn!(error = %error, "failed to store system event for stop condition");
                    }
                }
                drain_next_queued_turn(state, &session_key).await;
                return;
            }
        }

        let adapter = state.channels.get(&turn.context.channel);
        let external_chat_id = turn.context.session_key();
        let _activity = match adapter {
            Some(adapter) => match adapter.begin_turn_activity(&external_chat_id).await {
                Ok(activity) => Some(activity),
                Err(error) => {
                    tracing::warn!(
                        agent_id = %turn.context.agent_id,
                        error = %error,
                        "scheduled turn: failed to begin channel activity"
                    );
                    None
                }
            },
            None => None,
        };

        let started_at = chrono::Utc::now().to_rfc3339();
        let started = std::time::Instant::now();

        let turn_result = execute_turn_with_progress(state, &turn.context, &turn.input).await;
        let duration = started.elapsed().as_secs_f64();

        match turn_result {
            Ok(response) => {
                state.runtime_status.push_turn(
                    &turn.context.trace_id,
                    &turn.context.agent_id,
                    &turn.context.channel,
                    &started_at,
                    duration,
                    true,
                );
                if let Some(adapter) = adapter {
                    if let Err(error) = adapter.send_text(&external_chat_id, &response).await {
                        tracing::warn!(
                            agent_id = %turn.context.agent_id,
                            error = %error,
                            "scheduled turn: failed to send response to channel"
                        );
                        state.runtime_status.push_error(
                            &origin_id,
                            "channel_send",
                            &turn.context.agent_id,
                            &turn.context.channel,
                            &error.to_string(),
                        );
                        crate::runtime::metrics::inc_turn_errors_total(
                            "channel_send",
                            &turn.context.agent_id,
                        );
                    }
                }
                if !response.is_empty() {
                    if let Some(log_chat_id) = turn.context.channel_log_chat_id {
                        let db = std::sync::Arc::clone(state.db_for(turn.context.scope));
                        let agent_id = turn.context.agent_id.clone();
                        let response_owned = response.clone();
                        if let Err(error) = crate::storage::call_blocking(db, move |db| {
                            db.store_channel_log_bot_response(
                                log_chat_id,
                                &agent_id,
                                &response_owned,
                            )
                        })
                        .await
                        {
                            tracing::warn!(error = %error, "failed to store bot response in Channel Log");
                        }
                    }
                }
            }
            Err(error) => {
                state.runtime_status.push_turn(
                    &turn.context.trace_id,
                    &turn.context.agent_id,
                    &turn.context.channel,
                    &started_at,
                    duration,
                    false,
                );
                tracing::warn!(
                    agent_id = %turn.context.agent_id,
                    error = %error,
                    "scheduled turn: process_turn failed"
                );
                state.runtime_status.push_error(
                    &origin_id,
                    "turn_failure",
                    &turn.context.agent_id,
                    &turn.context.channel,
                    &error.to_string(),
                );
                crate::runtime::metrics::inc_turn_errors_total(
                    "turn_failure",
                    &turn.context.agent_id,
                );
                state
                    .turn_tracker
                    .set_terminal_reason(&origin_id, turn_scheduler::StopReason::LlmFailure);
                if let Some(log_chat_id) = turn.context.channel_log_chat_id {
                    if let Err(db_err) = state
                        .db_for(turn.context.scope)
                        .store_system_event(log_chat_id, &turn_scheduler::StopReason::LlmFailure)
                    {
                        tracing::warn!(error = %db_err, "failed to store LLM failure system event");
                    }
                }
                send_turn_failure_to_channel(adapter, &external_chat_id, &error).await;
                drain_next_queued_turn(state, &session_key).await;
                return;
            }
        }

        drain_next_queued_turn(state, &session_key).await;
    })
}

/// Drains the next queued turn for a session after the current turn completes.
///
/// During shutdown (`accepting_inputs == false`) the next queued turn is **not**
/// started: its origin reservation is released and the chain stops, so the
/// in-flight turn task can complete and be reaped by the supervisor drain.
/// This is the single point that enforces "no new turn starts after shutdown
/// begins" for turns already buffered in the in-memory scheduler.
async fn drain_next_queued_turn(state: &AppState, session_key: &str) {
    if let Some(next) = state.turn_scheduler.on_turn_completed(session_key) {
        if state.supervisor.is_shutting_down() {
            state.turn_tracker.release(&next.origin_id);
            tracing::info!(
                origin_id = %next.origin_id,
                "shutdown in progress: not starting next queued turn"
            );
            return;
        }
        execute_scheduled_turn(state, next).await;
    }
}

/// Executes one agent turn while recording runtime activity and telemetry.
///
/// The crate-visible helper accepts the shared [`AppState`], a
/// [`crate::agent_loop::SurfaceContext`], and the user `input`, returning the
/// generated response as `Result<String, EgoPulseError>`. It touches channel
/// activity, records the completed turn, and records an error plus the
/// `turn_failure` metric when execution fails.
///
/// # Errors
///
/// Propagates any [`EgoPulseError`] returned by the single turn execution.
/// Such failures are also recorded through `runtime_status.push_error`.
pub(crate) async fn execute_observed_turn(
    state: &AppState,
    context: &crate::agent_loop::SurfaceContext,
    input: &str,
) -> Result<String, EgoPulseError> {
    state
        .runtime_status
        .touch_channel_activity(&context.channel);
    let started_at = chrono::Utc::now().to_rfc3339();
    let started = std::time::Instant::now();
    let runtime = state.turn_runtime();
    let result =
        crate::agent_loop::process_turn_with_events(&runtime, context, input, |_| {}).await;
    let duration = started.elapsed().as_secs_f64();
    state.runtime_status.push_turn(
        &context.trace_id,
        &context.agent_id,
        &context.channel,
        &started_at,
        duration,
        result.is_ok(),
    );
    if let Err(error) = &result {
        state.runtime_status.push_error(
            &context.trace_id,
            "turn_failure",
            &context.agent_id,
            &context.channel,
            &error.to_string(),
        );
        crate::runtime::metrics::inc_turn_errors_total("turn_failure", &context.agent_id);
    }
    result
}

async fn execute_turn_with_progress(
    state: &AppState,
    context: &crate::agent_loop::SurfaceContext,
    input: &str,
) -> Result<String, EgoPulseError> {
    let adapter = state.channels.get(&context.channel);
    let external_chat_id = context.session_key();
    let sink = adapter
        .and_then(|adapter| adapter.tool_progress_sink())
        .filter(|_| tool_progress_enabled(&state.config, context));

    let (evt_tx, evt_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::agent_loop::event::AgentEvent>();
    let coordinator = tool_progress::ToolProgressCoordinator::new(sink, external_chat_id.clone());
    let coordinator_handle = tokio::spawn(coordinator.run(evt_rx));
    // timeout 枝でタスクを確実に停止できるよう abort handle を保持する。
    let coordinator_abort = coordinator_handle.abort_handle();

    let event_sender = evt_tx.clone();
    let runtime = state.turn_runtime();
    let result =
        crate::agent_loop::process_turn_with_events(&runtime, context, input, move |event| {
            let _ = event_sender.send(event);
        })
        .await;

    if result
        .as_ref()
        .is_err_and(|error| error.is_codex_auth_error())
    {
        tracing::warn!("codex 401 detected, refreshing token for a later turn");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        crate::llm::codex_auth::force_refresh_codex_token(&http).await;
    }

    // `evt_tx` を全て drop してから await する（さもないと coordinator が EOF を検出できずハングする）。
    drop(evt_tx);
    match tokio::time::timeout(Duration::from_secs(2), coordinator_handle).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(
            error = %error,
            "tool progress coordinator task failed"
        ),
        Err(_) => {
            coordinator_abort.abort();
            tracing::warn!("tool progress coordinator did not finish within timeout; aborted");
        }
    }
    result
}

/// 当該チャネルで進捗表示が有効かを設定からルックアップする。
fn tool_progress_enabled(
    config: &crate::config::Config,
    context: &crate::agent_loop::SurfaceContext,
) -> bool {
    let channel_config = config.channels.get(context.channel.as_str());
    match context.channel.as_str() {
        "discord" => channel_config
            .and_then(|c| c.discord_channels.as_ref())
            .and_then(|channels| {
                context
                    .surface_thread
                    .parse::<u64>()
                    .ok()
                    .and_then(|id| channels.get(&id))
            })
            .is_some_and(|c| c.tool_progress),
        "telegram" => channel_config
            .and_then(|c| c.telegram_channels.as_ref())
            .and_then(|channels| {
                context
                    .surface_thread
                    .parse::<i64>()
                    .ok()
                    .and_then(|id| channels.get(&id))
            })
            .is_some_and(|c| c.tool_progress),
        _ => false,
    }
}

async fn send_turn_failure_to_channel(
    adapter: Option<&Arc<dyn crate::channels::adapter::ChannelAdapter>>,
    external_chat_id: &str,
    error: &EgoPulseError,
) {
    let Some(adapter) = adapter else { return };
    let message = format!("⚠️ {}", error.user_facing_summary());
    if let Err(send_err) = adapter.send_text(external_chat_id, &message).await {
        tracing::warn!(
            error = %send_err,
            "failed to send turn failure message to channel"
        );
    }
}

fn spawn_mcp_reconnect_loop(
    mcp_manager: Arc<tokio::sync::RwLock<crate::tools::mcp::McpManager>>,
    workspace_dir: PathBuf,
    shutdown: CancellationToken,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EgoPulseError>> + Send>> {
    Box::pin(async move {
        const INITIAL_RETRY_SECS: u64 = 5;
        const MAX_RETRY_SECS: u64 = 300;

        let mut retry_secs = INITIAL_RETRY_SECS;
        loop {
            let has_failed_servers = {
                let guard = mcp_manager.read().await;
                guard.has_failed_servers()
            };

            let sleep_secs = if has_failed_servers {
                retry_secs
            } else {
                MAX_RETRY_SECS
            };

            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {}
            }

            if !has_failed_servers {
                retry_secs = INITIAL_RETRY_SECS;
                continue;
            }

            let reconnected = {
                let mut guard = mcp_manager.write().await;
                guard.reconnect_failed_once(&workspace_dir).await
            };

            if reconnected > 0 {
                retry_secs = INITIAL_RETRY_SECS;
            } else {
                retry_secs = (retry_secs * 2).min(MAX_RETRY_SECS);
            }
        }
    })
}

/// Sends a single prompt to the configured LLM without session state.
pub async fn ask(config: Config, prompt: &str) -> Result<String, EgoPulseError> {
    let llm = create_provider(&config.resolve_global_llm())?;
    let messages = Arc::new(vec![Message::text("user", prompt)]);

    tokio::select! {
        response = llm.send_message("", messages, None) => Ok(response?.content),
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

/// Starts the local TUI channel with a fully built application state.
pub async fn run_tui(config: Config, config_path: Option<PathBuf>) -> Result<(), EgoPulseError> {
    let state = build_app_state_with_path(config, config_path).await?;
    channels::tui::run(state).await
}

/// Starts all enabled channels and supervises them until shutdown or failure.
///
/// Every long-lived task (channel listeners, schedulers) is owned by the
/// runtime supervisor. The run loop watches for critical task failures and
/// Ctrl-C; on either trigger it runs [`RuntimeSupervisor::shutdown`], which
/// stops accepting input, drains in-flight turns, then drains long-lived tasks
/// within bounded deadlines.
pub async fn start_channels(state: AppState) -> Result<(), EgoPulseError> {
    let mut has_active_channels = false;

    // Web サーバー起動
    if state.config.web_enabled() {
        has_active_channels = true;
        let rs = Arc::clone(&state.runtime_status);
        rs.update_channel("web", ChannelState::Starting);
        let web_state = state.clone();
        let host = state.config.web_host().to_owned();
        let port = state.config.web_port();
        let token = state.supervisor.shutdown_token();
        info!("Starting Web UI server on {host}:{port}");
        state.supervisor.spawn_long_lived(
            TaskSpec::new(TaskKind::Channel, "web", Criticality::Critical),
            async move { crate::channels::web::run_server(web_state, &host, port, token).await },
        );
    }

    // Discord bot 起動 — Bot ごとに 1 つ以上の Discord client を起動する。
    #[cfg(feature = "channel-discord")]
    {
        let shared_channels = state.config.discord_channels();
        let default_agent = state.config.default_agent.clone();
        let bot_configs: Vec<_> = state
            .config
            .discord_bots()
            .into_iter()
            .map(|b| (b.bot_id.clone(), b.token.to_string(), default_agent.clone()))
            .collect();

        if !bot_configs.is_empty() {
            has_active_channels = true;
            let rs = Arc::clone(&state.runtime_status);
            rs.update_channel("discord", ChannelState::Starting);
            let shared_chain_state = Arc::new(crate::channels::discord::BotChainState::new());
            for (bot_id, token, default_agent) in bot_configs {
                let discord_state = Arc::new(state.clone());
                info!("Starting Discord bot '{bot_id}' (agent {default_agent})...");
                let bid = bot_id.clone();
                let chain_state = Arc::clone(&shared_chain_state);
                let channels = shared_channels.clone();
                let handle_name = format!("discord[{bot_id}]");
                state.supervisor.spawn_long_lived(
                    TaskSpec::new(TaskKind::Channel, handle_name, Criticality::Critical),
                    async move {
                        crate::channels::discord::start_discord_bot_for_bot(
                            discord_state,
                            &token,
                            &bid,
                            &default_agent,
                            &channels,
                            chain_state,
                        )
                        .await
                        .map_err(|error| {
                            EgoPulseError::Channel(ChannelError::SendFailed(format!(
                                "discord bot ({bid}) failed: {error}",
                            )))
                        })
                    },
                );
            }
        } else {
            tracing::warn!(
                "Discord channel is enabled but no bots have a token configured. \
                 Set channels.discord.bots.<id>.token in egopulse.config.yaml."
            );
        }
    }

    // Telegram bot 起動
    #[cfg(feature = "channel-telegram")]
    {
        let shared_channels = state.config.telegram_channels();
        let default_agent = state.config.default_agent.clone();
        let bot_configs: Vec<_> = state
            .config
            .telegram_bots()
            .into_iter()
            .map(|b| (b.bot_id.clone(), b.token.to_string(), default_agent.clone()))
            .collect();

        if !bot_configs.is_empty() {
            has_active_channels = true;
            let rs = Arc::clone(&state.runtime_status);
            rs.update_channel("telegram", ChannelState::Starting);
            let shared_chain_state = Arc::new(crate::channels::telegram::BotChainState::new());
            for (bot_id, token, default_agent) in bot_configs {
                let telegram_state = Arc::new(state.clone());
                info!("Starting Telegram bot '{bot_id}' (agent {default_agent})...");
                let bid = bot_id.clone();
                let chain_state = Arc::clone(&shared_chain_state);
                let channels = shared_channels.clone();
                let handle_name = format!("telegram[{bot_id}]");
                state.supervisor.spawn_long_lived(
                    TaskSpec::new(TaskKind::Channel, handle_name, Criticality::Critical),
                    async move {
                        crate::channels::telegram::start_telegram_bot_for_bot(
                            telegram_state,
                            &token,
                            &bid,
                            &default_agent,
                            &channels,
                            chain_state,
                        )
                        .await
                        .map_err(|error| {
                            EgoPulseError::Channel(ChannelError::SendFailed(format!(
                                "telegram bot ({bid}) failed: {error}",
                            )))
                        })
                    },
                );
            }
        } else if state.config.channel_enabled("telegram") {
            tracing::warn!(
                "Telegram channel is enabled but no bots have a token configured. \
                 Set channels.telegram.bots.<id>.token in egopulse.config.yaml."
            );
        }
    }

    if !has_active_channels {
        return Err(EgoPulseError::Config(
            crate::error::ConfigError::NoActiveChannels,
        ));
    }

    if state.config.sleep_batch.scheduler_enabled() {
        let scheduler_state = state.clone();
        let token = state.supervisor.shutdown_token();
        info!("Starting sleep batch scheduler");
        state.supervisor.spawn_long_lived(
            TaskSpec::new(
                TaskKind::SleepScheduler,
                "sleep-scheduler",
                Criticality::NonCritical,
            ),
            async move {
                crate::sleep::scheduler::run_scheduler_loop(scheduler_state, token).await
            },
        );
    }

    if state.config.pulse().scheduler_enabled() {
        match crate::storage::call_blocking(std::sync::Arc::clone(&state.db), |db| {
            db.reap_orphaned_pulse_runs()
        })
        .await
        {
            Ok(n) if n > 0 => info!("reaped {n} orphaned pulse_runs on startup"),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to reap orphaned pulse_runs on startup"),
        }

        let pulse_state = state.clone();
        let token = state.supervisor.shutdown_token();
        info!("Starting pulse scheduler");
        state.supervisor.spawn_long_lived(
            TaskSpec::new(
                TaskKind::PulseScheduler,
                "pulse-scheduler",
                Criticality::NonCritical,
            ),
            async move {
                crate::pulse::scheduler::run_pulse_scheduler(pulse_state, token).await;
                Ok(())
            },
        );
    }

    if state.config.db.backup.scheduler_enabled() {
        let backup_state = state.clone();
        let token = state.supervisor.shutdown_token();
        info!("Starting backup scheduler");
        state.supervisor.spawn_long_lived(
            TaskSpec::new(
                TaskKind::BackupScheduler,
                "backup-scheduler",
                Criticality::NonCritical,
            ),
            async move { backup_scheduler::run_backup_scheduler_loop(backup_state, token).await },
        );
    }

    // Recovery is complete and channels are starting; the runtime may now serve
    // external input. Flipped back to false by `shutdown`.
    state.supervisor.start_accepting();
    state.runtime_status.set_accepting_inputs(true);

    info!("Runtime active; waiting for Ctrl-C or channel failure");

    loop {
        // Detect critical long-lived task failures (channel exit, worker panic).
        if let Some(outcome) = state.supervisor.poll_long_lived() {
            let summary = match outcome.result() {
                supervisor::TaskResult::Ok => {
                    format!("critical task '{}' exited unexpectedly", outcome.name())
                }
                supervisor::TaskResult::Err(msg) => {
                    format!("critical task '{}' failed: {}", outcome.name(), msg)
                }
                supervisor::TaskResult::Panic => {
                    format!("critical task '{}' panicked", outcome.name())
                }
            };
            state.runtime_status.record_critical_task_failure(&summary);
            tracing::warn!(
                task = %outcome.name(),
                result = ?outcome.result(),
                "critical task exited; initiating shutdown"
            );
            state.supervisor.shutdown().await;
            return Err(EgoPulseError::Channel(ChannelError::SendFailed(format!(
                "critical task '{}' exited unexpectedly",
                outcome.name()
            ))));
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                state.supervisor.shutdown().await;
                return Ok(());
            },
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::agent_loop::ConversationScope;
    use crate::agent_loop::soul_agents::SoulAgentsLoader;
    use crate::config::ResolvedLlmConfig;

    fn test_config_for_runtime(state_root: String) -> crate::config::Config {
        crate::test_util::test_config(&state_root)
    }

    #[test]
    fn tool_progress_enabled_reads_channel_config_flag() {
        use crate::agent_loop::SurfaceContext;
        use crate::config::{ChannelConfig, ChannelName, DiscordChannelConfig};
        use std::collections::HashMap;

        // Arrange: discord channel 123 has tool_progress on, 456 off
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let mut discord_channels = HashMap::new();
        discord_channels.insert(
            123u64,
            DiscordChannelConfig {
                tool_progress: true,
                ..Default::default()
            },
        );
        discord_channels.insert(
            456u64,
            DiscordChannelConfig {
                tool_progress: false,
                ..Default::default()
            },
        );
        config.channels.insert(
            ChannelName::new("discord"),
            ChannelConfig {
                discord_channels: Some(discord_channels),
                ..Default::default()
            },
        );

        let ctx = |thread: &str| {
            SurfaceContext::new(
                "discord".to_string(),
                "user".to_string(),
                thread.to_string(),
                "discord".to_string(),
                "lyre".to_string(),
            )
        };

        // Act + Assert
        assert!(
            tool_progress_enabled(&config, &ctx("123")),
            "channel 123 enabled"
        );
        assert!(
            !tool_progress_enabled(&config, &ctx("456")),
            "channel 456 disabled"
        );
        assert!(
            !tool_progress_enabled(&config, &ctx("999")),
            "unknown channel disabled"
        );
        let web_ctx = SurfaceContext::new(
            "web".to_string(),
            "user".to_string(),
            "session".to_string(),
            "web".to_string(),
            "lyre".to_string(),
        );
        assert!(
            !tool_progress_enabled(&config, &web_ctx),
            "web never enabled"
        );
    }

    // --- tool progress wiring (T16/T17/T19): coordinator must never hang the turn ---

    struct StubFinalProvider;

    #[async_trait::async_trait]
    impl crate::llm::LlmProvider for StubFinalProvider {
        fn provider_name(&self) -> &str {
            "stub"
        }
        fn model_name(&self) -> &str {
            "stub-model"
        }
        async fn send_message(
            &self,
            _: &str,
            _: std::sync::Arc<Vec<crate::llm::Message>>,
            _: Option<std::sync::Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            Ok(crate::llm::MessagesResponse {
                content: "ok".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: std::sync::Arc<Vec<crate::llm::Message>>,
            tools: Option<std::sync::Arc<Vec<crate::llm::ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    struct StubFailingProvider;

    #[async_trait::async_trait]
    impl crate::llm::LlmProvider for StubFailingProvider {
        fn provider_name(&self) -> &str {
            "stub"
        }
        fn model_name(&self) -> &str {
            "stub-model"
        }
        async fn send_message(
            &self,
            _: &str,
            _: std::sync::Arc<Vec<crate::llm::Message>>,
            _: Option<std::sync::Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            Err(crate::error::LlmError::InvalidResponse(
                "stub failure".to_string(),
            ))
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: std::sync::Arc<Vec<crate::llm::Message>>,
            tools: Option<std::sync::Arc<Vec<crate::llm::ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    struct RetryableFailingProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::llm::LlmProvider for RetryableFailingProvider {
        fn provider_name(&self) -> &str {
            "stub"
        }

        fn model_name(&self) -> &str {
            "stub-model"
        }

        async fn send_message(
            &self,
            _: &str,
            _: Arc<Vec<crate::llm::Message>>,
            _: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(crate::error::LlmError::ApiError {
                status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                body_preview: "test failure".to_string(),
                retry_after_secs: None,
            })
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<crate::llm::Message>>,
            tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    #[tokio::test]
    async fn execute_turn_with_progress_terminates_on_success() {
        // Arrange: a turn whose coordinator has no sink (web/cli channel).
        let dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(StubFinalProvider),
        );
        let context = crate::test_util::cli_context("progress-success");

        // Act: a bounded timeout proves the coordinator never hangs the turn.
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            execute_turn_with_progress(&state, &context, "hello"),
        )
        .await;

        // Assert
        assert!(result.is_ok(), "execute_turn_with_progress must not hang");
        assert_eq!(result.unwrap().expect("turn ok"), "ok");
    }

    #[tokio::test]
    async fn execute_turn_with_progress_terminates_on_failure() {
        // Arrange: the LLM always fails.
        let dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(StubFailingProvider),
        );
        let context = crate::test_util::cli_context("progress-failure");

        // Act: the failure path must also drop evt_tx and return bounded.
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            execute_turn_with_progress(&state, &context, "hello"),
        )
        .await;

        // Assert
        assert!(
            result.is_ok(),
            "execute_turn_with_progress must not hang on failure"
        );
        assert!(result.unwrap().is_err(), "turn should fail");
    }

    #[tokio::test]
    async fn retryable_llm_failure_executes_one_turn_and_persists_one_input() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let calls = Arc::new(AtomicUsize::new(0));
        let state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(RetryableFailingProvider {
                calls: Arc::clone(&calls),
            }),
        );
        let context = crate::test_util::cli_context("retryable-failure");

        // Act
        let result = execute_turn_with_progress(&state, &context, "hello").await;

        // Assert: the same model iteration is retried up to
        // `MAX_LLM_RETRIES` before surfacing the error, but still executes a
        // single Turn with a single persisted input.
        assert!(result.is_err(), "retryable failure must reach the caller");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            crate::agent_loop::turn::MAX_LLM_RETRIES,
            "LLM must be retried up to MAX_LLM_RETRIES within the same iteration"
        );
        let conn = state.db.get_conn().expect("connection");
        let user_messages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE sender_kind = 'user'",
                [],
                |row| row.get(0),
            )
            .expect("count user messages");
        assert_eq!(user_messages, 1, "input must be persisted once");
    }

    #[tokio::test]
    async fn build_app_state_contains_soul_agents_loader() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let state = build_app_state(config).await.expect("build state");
        let _ = &*state.soul_agents;
    }

    #[test]
    fn build_sleep_app_state_skips_mcp_initialization() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let config_path = dir.path().join("egopulse.config.yaml");

        let state = build_sleep_app_state_with_path(config, Some(config_path.clone()))
            .expect("build sleep state");

        assert!(
            state.mcp_manager.is_none(),
            "sleep state must not connect MCP servers"
        );
        assert_eq!(state.config_path.as_deref(), Some(config_path.as_path()));
        let _ = &*state.memory_loader;
    }

    #[tokio::test]
    async fn soul_agents_loader_loads_agents_from_config_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().to_str().expect("utf8").to_string();
        let config = test_config_for_runtime(state_root);
        let loader = SoulAgentsLoader::new(&config);

        assert!(loader.load_global_agents().is_none());

        std::fs::write(dir.path().join("AGENTS.md"), "test agents content").expect("write");
        assert_eq!(
            loader.load_global_agents(),
            Some("test agents content".to_string())
        );
    }

    fn resolved_config(provider: &str, model: &str, base_url: &str) -> ResolvedLlmConfig {
        ResolvedLlmConfig {
            provider: provider.to_string(),
            label: format!("{provider} label"),
            base_url: base_url.to_string(),
            api_key: Some(secrecy::SecretString::new(
                "sk-test".to_string().into_boxed_str(),
            )),
            model: model.to_string(),
        }
    }

    #[test]
    fn cache_key_differs_when_provider_differs() {
        let a = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        let b = resolved_config("anthropic", "gpt-4o", "https://api.openai.com/v1");
        assert_ne!(a.cache_key_with_revision(0), b.cache_key_with_revision(0));
    }

    #[test]
    fn cache_key_differs_when_model_differs() {
        let a = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        let b = resolved_config("openai", "gpt-4o-mini", "https://api.openai.com/v1");
        assert_ne!(a.cache_key_with_revision(0), b.cache_key_with_revision(0));
    }

    #[test]
    fn cache_key_differs_when_base_url_differs() {
        let a = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        let b = resolved_config("openai", "gpt-4o", "https://proxy.example.com/v1");
        assert_ne!(a.cache_key_with_revision(0), b.cache_key_with_revision(0));
    }

    #[test]
    fn cache_key_differs_when_api_key_differs() {
        let a = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        let mut b = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        b.api_key = Some(secrecy::SecretString::new(
            "sk-other".to_string().into_boxed_str(),
        ));
        assert_ne!(a.cache_key_with_revision(0), b.cache_key_with_revision(0));
    }

    #[test]
    fn cache_key_same_for_identical_configs() {
        let a = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        let b = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        assert_eq!(a.cache_key_with_revision(0), b.cache_key_with_revision(0));
    }

    #[tokio::test]
    async fn llm_for_context_reuses_cached_provider() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let state = build_app_state(config).await.expect("build state");
        let context = crate::test_util::cli_context("cache-test");

        let a = state.llm_for_context(&context).expect("llm");
        let b = state.llm_for_context(&context).expect("llm");

        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn cloned_app_state_shares_llm_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let state = build_app_state(config).await.expect("build state");
        let cloned = state.clone();
        let context = crate::test_util::cli_context("cache-clone-test");

        let a = state.llm_for_context(&context).expect("llm");
        let b = cloned.llm_for_context(&context).expect("llm");

        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn llm_override_bypasses_cache() {
        let dir = tempfile::tempdir().expect("tempdir");

        let expected_provider = "override";
        let expected_model = "model-x";

        let state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            crate::llm::create_provider(&resolved_config(
                expected_provider,
                expected_model,
                "https://example.com/v1",
            ))
            .expect("provider"),
        );
        let context = crate::test_util::cli_context("override-test");

        let result = state.llm_for_context(&context).expect("llm");
        assert_eq!(result.provider_name(), expected_provider);
        assert_eq!(result.model_name(), expected_model);

        let cache = state.llm_cache.lock().expect("lock");
        assert!(cache.is_empty());
    }

    #[tokio::test]
    async fn build_app_state_includes_runtime_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let state = build_app_state(config).await.expect("build state");
        let snap = state.runtime_status.snapshot();
        assert!(!snap.version.is_empty());
        assert!(snap.pid > 0);
        assert!(!snap.started_at.is_empty());
    }

    #[tokio::test]
    async fn cloned_app_state_shares_runtime_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let state = build_app_state(config).await.expect("build state");
        let cloned = state.clone();
        assert!(Arc::ptr_eq(&state.runtime_status, &cloned.runtime_status));
    }

    #[test]
    fn build_sleep_app_state_includes_runtime_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let state = build_sleep_app_state_with_path(config, None).expect("build sleep state");
        let snap = state.runtime_status.snapshot();
        assert!(!snap.version.is_empty());
    }

    #[test]
    fn cap_observations_per_key_keeps_newest_n_for_shared_keys() {
        let mk = |created_at: &str, input: i64| CalibrationObservation {
            provider: "p".into(),
            model: "m".into(),
            request_kind: "agent_loop".into(),
            has_tools: true,
            estimated_tokens: 100,
            input_tokens: input,
            created_at: created_at.into(),
        };
        // Simulate two databases each contributing observations for one key,
        // already individually capped but exceeding N once merged.
        let mut observations = vec![
            mk("2026-01-01T00:00:01Z", 1),
            mk("2026-01-01T00:00:02Z", 2),
            mk("2026-01-01T00:00:03Z", 3),
            mk("2026-01-01T00:00:04Z", 4),
            mk("2026-01-01T00:00:05Z", 5),
            mk("2026-01-01T00:00:06Z", 6),
        ];

        AppState::cap_observations_per_key(&mut observations, 3);

        // Assert: only the 3 newest (4, 5, 6), oldest-first
        assert_eq!(observations.len(), 3);
        assert_eq!(observations[0].input_tokens, 4);
        assert_eq!(observations[1].input_tokens, 5);
        assert_eq!(observations[2].input_tokens, 6);
    }

    fn build_sleep_state(dir: &tempfile::TempDir) -> AppState {
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        build_sleep_app_state_with_path(config, None).expect("build sleep state")
    }

    #[test]
    fn db_for_returns_normal_db_for_normal_scope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_sleep_state(&dir);
        let result = state.db_for(ConversationScope::Normal);
        assert!(Arc::ptr_eq(result, &state.db));
    }

    #[test]
    fn db_for_returns_secret_db_for_secret_scope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = build_sleep_state(&dir);
        let secret_path = dir.path().join("runtime").join("secret.db");
        let secret_db = Arc::new(Database::new_secret(&secret_path).expect("secret db"));
        state.secret_db = Some(Arc::clone(&secret_db));
        let result = state.db_for(ConversationScope::Secret);
        assert!(Arc::ptr_eq(result, &secret_db));
    }

    #[test]
    fn db_for_returns_database_for_conversation_scope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = build_sleep_state(&dir);
        let secret_path = dir.path().join("runtime").join("secret.db");
        let secret_db = Arc::new(Database::new_secret(&secret_path).expect("secret db"));
        state.secret_db = Some(Arc::clone(&secret_db));

        let normal_db = state.db_for(ConversationScope::Normal);
        let secret_result = state.db_for(ConversationScope::Secret);

        assert!(
            Arc::ptr_eq(normal_db, &state.db),
            "Normal scope must return the primary database"
        );
        assert!(
            Arc::ptr_eq(secret_result, &secret_db),
            "Secret scope must return the isolated secret database"
        );
        assert!(
            !Arc::ptr_eq(normal_db, secret_result),
            "Normal and Secret scopes must return different databases"
        );
    }

    #[test]
    #[should_panic(expected = "secret db required but not initialized")]
    fn db_for_panics_when_secret_db_uninitialized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_sleep_state(&dir);
        let _ = state.db_for(ConversationScope::Secret);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn scheduled_turn_logs_route_by_conversation_scope() {
        use crate::agent_loop::ScheduledTurn;
        use crate::agent_loop::turn::RecordingProvider;
        use crate::llm::MessagesResponse;
        use crate::storage::call_blocking;

        // Arrange: state with secret DB + recording provider
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "secret scheduled reply".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let mut state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(provider),
        );
        let secret_path = dir.path().join("runtime").join("secret.db");
        state.secret_db = Some(Arc::new(
            Database::new_secret(&secret_path).expect("secret db"),
        ));

        let log_chat_id: i64 = 9999;
        let mut context = crate::test_util::cli_context("scheduled-secret-routing");
        context.scope = ConversationScope::Secret;
        context.channel_log_chat_id = Some(log_chat_id);

        let turn = ScheduledTurn {
            context,
            input: "scheduled secret input".to_string(),
            origin_id: uuid::Uuid::new_v4().to_string(),
        };

        // Act: execute the scheduled turn
        execute_scheduled_turn(&state, turn).await;

        // Assert: secret DB has the bot response
        let secret_messages = call_blocking(
            Arc::clone(state.secret_db.as_ref().expect("secret db")),
            move |db| db.get_channel_log_messages(log_chat_id, 10),
        )
        .await
        .expect("read secret channel log");
        let secret_has_reply = secret_messages
            .iter()
            .any(|m| m.content.contains("secret scheduled reply"));
        assert!(
            secret_has_reply,
            "secret DB should contain the bot response"
        );

        // Assert: normal DB has no entries from this turn
        let normal_messages = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_channel_log_messages(log_chat_id, 10)
        })
        .await
        .expect("read normal channel log");
        let normal_has_reply = normal_messages
            .iter()
            .any(|m| m.content.contains("secret scheduled reply"));
        assert!(
            !normal_has_reply,
            "normal DB should not contain the secret bot response"
        );
    }
}

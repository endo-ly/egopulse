//! EgoPulse ランタイム全体の依存を組み立てるモジュール。
//!
//! `AppState` の構築、単発 LLM 実行、各チャネルの起動と監視を提供する。

pub(crate) mod backup_scheduler;
pub(crate) mod channel_input;
pub(crate) mod config_reload;
pub mod gateway;
pub mod logging;
pub(crate) mod metrics;
pub(crate) mod status;
pub(crate) mod supervisor;
pub(crate) mod turn;

pub(crate) use channel_input::{
    ChannelLogKey, HumanChannelLogMessage, TurnIntake, build_channel_context,
    channel_scope_from_secret, store_human_channel_log_message, submit_agent_turn,
};
pub(crate) use status::ChannelState;
pub(crate) use status::RuntimeStatus;
pub(crate) use supervisor::Criticality;
pub(crate) use supervisor::RuntimeSupervisor;
pub(crate) use supervisor::TaskKind;
pub(crate) use supervisor::TaskSpec;
pub(crate) use turn::{ActiveTurnTracker, execute_observed_turn, execute_scheduled_turn};

use turn::{recover_durable_state, rehydrate_origin_tracker, spawn_turn_dispatcher};

use fs2::FileExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tracing::{info, warn};

use crate::agent_loop::prompt::SoulAgentsLoader;
use crate::assets::AssetStore;
use crate::channels;
use crate::channels::adapter::ChannelRegistry;
use crate::channels::voice::VoiceAdapter;
use crate::channels::web::WebAdapter;
use crate::config::{Config, ConfigManager};
use crate::conversation::ConversationScope;
use crate::error::{ChannelError, EgoPulseError};
use crate::llm::calibration::{CalibrationKey, CalibrationObservation, UsageCalibrator};
use crate::llm::{Message, create_provider};
use crate::memory::MemoryLoader;
use crate::skills::SkillManager;
use crate::storage::Database;
use crate::storage::call_blocking;
use crate::tools::ToolRegistry;

// ---------------------------------------------------------------------------
// Shared state and dependency construction
// ---------------------------------------------------------------------------

const INSTANCE_LOCK_FILE_NAME: &str = "runtime-instance.lock";

/// Holds an exclusive advisory lock for a single state root.
///
/// The lock is bound to the underlying file descriptor, which stays open for
/// as long as this guard is alive. Dropping the guard — or the process
/// exiting, normally or abnormally — closes the descriptor and releases the
/// lock.
#[derive(Debug)]
pub(crate) struct InstanceGuard {
    _file: std::fs::File,
}

impl InstanceGuard {
    /// Acquires the exclusive instance lock for `state_root`.
    pub(crate) fn acquire(state_root: &Path) -> Result<Arc<Self>, EgoPulseError> {
        let lock_path = state_root.join(INSTANCE_LOCK_FILE_NAME);
        Self::open_and_lock(&lock_path)
    }

    fn open_and_lock(lock_path: &Path) -> Result<Arc<Self>, EgoPulseError> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|e| {
                EgoPulseError::Internal(format!(
                    "failed to open runtime instance lock {}: {e}",
                    lock_path.display()
                ))
            })?;
        file.try_lock_exclusive().map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                EgoPulseError::RuntimeAlreadyRunning(lock_path.display().to_string())
            } else {
                EgoPulseError::Internal(format!(
                    "failed to acquire runtime instance lock {}: {e}",
                    lock_path.display()
                ))
            }
        })?;
        Ok(Arc::new(Self { _file: file }))
    }
}

/// Holds the shared runtime dependencies used across all channels.
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
    /// Per-session turn scheduler for concurrency control and ordered execution.
    pub(crate) turn_scheduler: Arc<turn::TurnScheduler>,
    /// Per-origin turn counter for runaway prevention.
    pub(crate) turn_tracker: Arc<turn::TurnTracker>,
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

struct RuntimeTooling {
    tools: Arc<ToolRegistry>,
    mcp_manager: Arc<tokio::sync::RwLock<crate::tools::mcp::McpManager>>,
    agent_send_intake: Arc<channel_input::TurnIntake>,
    workspace_dir: PathBuf,
}

/// Resolved storage endpoints for a conversation scope.
///
/// Groups the database handle and archive root path so callers do not
/// need to know scope-specific path conventions.
pub(crate) struct ScopedStorage {
    /// The database handle for this scope.
    pub db: Arc<Database>,
    /// Root directory for archived conversations.
    pub archive_root: PathBuf,
}

impl AppState {
    pub(crate) fn from_parts(parts: AppStateParts) -> Self {
        Self {
            db: parts.db,
            secret_db: parts.secret_db,
            config: parts.config,
            config_manager: parts.config_manager,
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
            turn_scheduler: Arc::new(turn::TurnScheduler::new()),
            turn_tracker: Arc::new(turn::TurnTracker::new()),
            runtime_status: parts.runtime_status.clone(),
            supervisor: Arc::new(RuntimeSupervisor::with_instance_guard(
                parts.runtime_status,
                Arc::clone(&parts.instance_guard),
            )),
            usage_calibrator: Arc::new(UsageCalibrator::new()),
            _sealed: (),
        }
    }

    /// Builds a [`crate::agent_loop::TurnDependencies`] from the subset of this
    /// `AppState` that Turn execution actually needs.
    ///
    /// Scheduler queues and chain tracking, channel dispatch, and runtime
    /// observability remain on `AppState` so Turn logic cannot depend on them.
    /// The active-turn tracker is the exception: `TurnExecutor` owns its
    /// begin/end activity bookkeeping at the Turn boundary.
    pub(crate) fn turn_dependencies(&self) -> crate::agent_loop::TurnDependencies {
        crate::agent_loop::TurnDependencies {
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
            active_turns: Arc::clone(&self.active_turns),
        }
    }

    /// Returns an owned handle to the appropriate database for `scope`.
    ///
    /// # Panics
    ///
    /// Panics if `scope` is `Secret` but `secret_db` was not initialized
    /// (i.e., no secret channels in config).
    pub(crate) fn db_for(&self, scope: ConversationScope) -> Arc<Database> {
        match scope {
            ConversationScope::Normal => Arc::clone(&self.db),
            ConversationScope::Secret => Arc::clone(
                self.secret_db
                    .as_ref()
                    .expect("secret db required but not initialized"),
            ),
        }
    }

    /// Returns every initialized conversation scope with its database handle.
    ///
    /// Keeping scope enumeration here prevents startup and dispatcher code
    /// from growing separate normal/secret branches whenever a new database
    /// operation is added.
    pub(crate) fn scoped_databases(
        &self,
    ) -> impl Iterator<Item = (ConversationScope, Arc<Database>)> + '_ {
        std::iter::once((ConversationScope::Normal, Arc::clone(&self.db))).chain(
            self.secret_db
                .iter()
                .map(|db| (ConversationScope::Secret, Arc::clone(db))),
        )
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
        let mut observations = Vec::new();
        for (scope, db) in self.scoped_databases() {
            match call_blocking(db, |db| {
                db.load_calibration_observations(REPLAY_LIMIT_PER_KEY)
            })
            .await
            {
                Ok(o) => observations.extend(o),
                Err(e) => {
                    warn!(scope = %scope, error = %e, "calibration load failed; using defaults")
                }
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
        Arc::new(self.config_manager.current_blocking().config.clone())
    }
}

fn acquire_instance_guard(state_root: &str) -> Result<Arc<InstanceGuard>, EgoPulseError> {
    std::fs::create_dir_all(Path::new(state_root))
        .map_err(|e| EgoPulseError::Internal(format!("failed to create state root: {e}")))?;
    let instance_guard = InstanceGuard::acquire(Path::new(state_root))?;
    metrics::set_instance_lock_held(true);
    Ok(instance_guard)
}

fn build_channel_registry(
    config: &Config,
    config_manager: &Arc<ConfigManager>,
) -> Arc<ChannelRegistry> {
    let mut channels = ChannelRegistry::new();
    channels.register(Arc::new(WebAdapter));
    if config.voice_enabled() {
        channels.register(Arc::new(VoiceAdapter));
    }

    #[cfg(feature = "channel-discord")]
    if !config.discord_bots().is_empty() {
        channels.register(Arc::new(
            crate::channels::discord::DiscordAdapter::new_for_bots_with_manager(Arc::clone(
                config_manager,
            )),
        ));
    }

    #[cfg(feature = "channel-telegram")]
    if !config.telegram_bots().is_empty() {
        channels.register(Arc::new(
            crate::channels::telegram::TelegramAdapter::new_multi_with_manager(Arc::clone(
                config_manager,
            )),
        ));
    }

    Arc::new(channels)
}

async fn build_runtime_tooling(
    config: &Config,
    deps: &AppStateDependencies,
    config_manager: &Arc<ConfigManager>,
    channels: &Arc<ChannelRegistry>,
) -> Result<RuntimeTooling, EgoPulseError> {
    let workspace_dir = config.workspace_dir()?;
    let mcp_manager = Arc::new(tokio::sync::RwLock::new(
        crate::tools::mcp::McpManager::new(&workspace_dir).await?,
    ));

    let mut tools = build_tool_registry(config, &deps.skills, config_manager);
    tools.set_mcp_manager(Arc::clone(&mcp_manager));
    tools.register_tool(Box::new(crate::tools::SendAttachmentTool::new(
        workspace_dir.clone(),
        Arc::clone(channels),
        Arc::clone(&deps.db),
        deps.secret_db.clone(),
    )));

    let agent_send_intake = Arc::new(channel_input::TurnIntake::new());
    tools.register_tool(Box::new(crate::tools::AgentSendTool::new_with_manager(
        Arc::clone(&deps.db),
        deps.secret_db.clone(),
        Arc::clone(channels),
        Arc::clone(&agent_send_intake),
        Arc::clone(config_manager),
    )));

    Ok(RuntimeTooling {
        tools: Arc::new(tools),
        mcp_manager,
        agent_send_intake,
        workspace_dir,
    })
}

fn build_tool_registry(
    config: &Config,
    skills: &Arc<SkillManager>,
    config_manager: &Arc<ConfigManager>,
) -> ToolRegistry {
    let mut tools = ToolRegistry::new(config, Arc::clone(skills));
    tools.set_config_manager(Arc::clone(config_manager));
    tools
}

/// Builds the application state without recording a config file path.
///
/// # Errors
/// Returns `EgoPulseError` if the exclusive instance lock for the state root
/// cannot be acquired (another process holds it), or if dependency provisioning
/// (soul/config load, storage, LLM provider resolution) fails. See
/// [`build_app_state_with_path`] for the full contract.
pub async fn build_app_state(config: Config) -> Result<Arc<AppState>, EgoPulseError> {
    build_app_state_with_path(config, None).await
}

/// Builds the application state and keeps the config path for later saves.
///
/// # Errors
/// Returns `EgoPulseError` when any startup dependency fails:
/// - the exclusive instance lock cannot be acquired ([`crate::error::EgoPulseError::RuntimeAlreadyRunning`]),
///   checked before the database is opened;
/// - storage initialization or migration fails;
/// - configuration, soul-file, or skill loading fails;
/// - an LLM provider cannot be resolved.
pub async fn build_app_state_with_path(
    config: Config,
    config_path: Option<PathBuf>,
) -> Result<Arc<AppState>, EgoPulseError> {
    metrics::init_metrics();

    // Acquire the lock before opening storage so concurrent runtimes are
    // rejected before any database side effects occur.
    let instance_guard = acquire_instance_guard(&config.state_root)?;

    let deps = build_app_state_dependencies(&config, ProvisionDefaultSoul::Yes)?;
    let config_manager = Arc::new(ConfigManager::new(config.clone(), config_path.as_deref()));
    let channels = build_channel_registry(&config, &config_manager);
    let tooling = build_runtime_tooling(&config, &deps, &config_manager, &channels).await?;

    let runtime_status = Arc::new(RuntimeStatus::new());

    let state = Arc::new(AppState::from_parts(AppStateParts {
        db: deps.db,
        secret_db: deps.secret_db,
        config,
        config_manager,
        config_path,
        llm_override: None,
        channels,
        skills: deps.skills,
        tools: tooling.tools,
        mcp_manager: Some(Arc::clone(&tooling.mcp_manager)),
        assets: deps.assets,
        soul_agents: deps.soul_agents,
        memory_loader: deps.memory_loader,
        runtime_status: Arc::clone(&runtime_status),
        instance_guard: Arc::clone(&instance_guard),
    }));

    tooling.agent_send_intake.bind(&state);
    recover_runtime_state(&state).await?;
    spawn_runtime_background_tasks(&state, tooling.workspace_dir);

    Ok(state)
}

// ---------------------------------------------------------------------------
// Startup recovery and background services
// ---------------------------------------------------------------------------

async fn recover_runtime_state(state: &AppState) -> Result<(), EgoPulseError> {
    state.warm_up_calibrator().await;
    recover_durable_state(state).await?;
    rehydrate_origin_tracker(state)?;
    crate::sleep::recover_memory_publication(state)?;
    Ok(())
}

fn spawn_runtime_background_tasks(state: &Arc<AppState>, workspace_dir: PathBuf) {
    // Spawn order is part of the startup contract: recovered turns must be
    // dispatched before MCP reconnect work begins.
    spawn_turn_dispatcher(Arc::clone(state), state.supervisor.shutdown_token());

    let mcp_manager = state
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
        crate::tools::mcp::run_reconnect_loop(
            mcp_manager,
            workspace_dir,
            state.supervisor.shutdown_token(),
        ),
    );
}

/// Builds the minimal application state needed for manual sleep batch execution.
///
/// Sleep batch does not execute agent tools or channels, so this intentionally
/// avoids MCP initialization and the reconnect loop.
pub fn build_sleep_app_state_with_path(
    config: Config,
    config_path: Option<PathBuf>,
) -> Result<AppState, EgoPulseError> {
    let instance_guard = acquire_instance_guard(&config.state_root)?;

    let deps = build_app_state_dependencies(&config, ProvisionDefaultSoul::No)?;
    let channels = Arc::new(ChannelRegistry::new());
    let config_manager = Arc::new(ConfigManager::new(config.clone(), config_path.as_deref()));
    let tools = Arc::new(build_tool_registry(&config, &deps.skills, &config_manager));

    let runtime_status = Arc::new(RuntimeStatus::new());

    let state = AppState::from_parts(AppStateParts {
        db: deps.db,
        secret_db: deps.secret_db,
        config,
        config_manager,
        config_path,
        llm_override: None,
        channels,
        skills: deps.skills,
        tools,
        mcp_manager: None,
        assets: deps.assets,
        soul_agents: deps.soul_agents,
        memory_loader: deps.memory_loader,
        runtime_status,
        instance_guard,
    });
    // Re-drive memory publication for sleep runs interrupted mid-publication
    // before any new batch starts. The instance lock guarantees this process
    // is the sole writer.
    crate::sleep::recover_memory_publication(&state)?;
    Ok(state)
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

// ---------------------------------------------------------------------------
// Public entry points and channel lifecycle
// ---------------------------------------------------------------------------

/// Sends a single prompt to the configured LLM without session state.
pub async fn ask(config: Config, prompt: &str) -> Result<String, EgoPulseError> {
    let llm = create_provider(&config.resolve_global_llm())?;
    let messages = Arc::new(vec![Message::text("user", prompt)]);

    tokio::select! {
        response = llm.send_message("", messages, None) => Ok(response?.content),
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

/// Returns the logical session names ordered from most recently updated to oldest.
///
/// # Errors
/// Returns [`EgoPulseError`] when the session database cannot be queried.
pub async fn list_session_names(state: &AppState) -> Result<Vec<String>, EgoPulseError> {
    let sessions = call_blocking(Arc::clone(&state.db), |db| db.list_sessions()).await?;
    Ok(sessions
        .into_iter()
        .map(|session| session.surface_thread)
        .collect())
}

/// Starts the local TUI channel with a fully built application state.
pub async fn run_tui(
    config: Config,
    config_path: Option<PathBuf>,
    session: Option<&str>,
) -> Result<(), EgoPulseError> {
    let state = build_app_state_with_path(config, config_path).await?;
    channels::tui::run(state, session).await
}

fn spawn_web_channel(state: &Arc<AppState>) -> bool {
    if !state.config.web_enabled() {
        return false;
    }

    state
        .runtime_status
        .update_channel("web", ChannelState::Starting);
    let web_state = Arc::clone(state);
    let host = state.config.web_host().to_owned();
    let port = state.config.web_port();
    let token = state.supervisor.shutdown_token();
    info!("Starting Web UI server on {host}:{port}");
    state.supervisor.spawn_long_lived(
        TaskSpec::new(TaskKind::Channel, "web", Criticality::Critical),
        async move { crate::channels::web::run_server(web_state, &host, port, token).await },
    );
    true
}

#[cfg(feature = "channel-discord")]
fn spawn_discord_channels(state: &Arc<AppState>) -> bool {
    let bot_configs: Vec<_> = state
        .config
        .discord_bots()
        .into_iter()
        .map(|bot| (bot.bot_id.clone(), bot.token.to_string()))
        .collect();

    if bot_configs.is_empty() {
        tracing::warn!(
            "Discord channel is enabled but no bots have a token configured. \
             Set channels.discord.bots.<id>.token in egopulse.config.yaml."
        );
        return false;
    }

    state
        .runtime_status
        .update_channel("discord", ChannelState::Starting);
    let shared_chain_state = Arc::new(crate::channels::discord::BotChainState::new());
    for (bot_id, token) in bot_configs {
        let discord_state = Arc::clone(state);
        info!("Starting Discord bot '{bot_id}'...");
        let bot_id_for_task = bot_id.clone();
        let chain_state = Arc::clone(&shared_chain_state);
        let handle_name = format!("discord[{bot_id}]");
        state.supervisor.spawn_long_lived(
            TaskSpec::new(TaskKind::Channel, handle_name, Criticality::Critical),
            async move {
                crate::channels::discord::start_discord_bot_for_bot(
                    discord_state,
                    &token,
                    &bot_id_for_task,
                    chain_state,
                )
                .await
                .map_err(|error| {
                    EgoPulseError::Channel(ChannelError::SendFailed(format!(
                        "discord bot ({bot_id_for_task}) failed: {error}",
                    )))
                })
            },
        );
    }
    true
}

#[cfg(feature = "channel-telegram")]
fn spawn_telegram_channels(state: &Arc<AppState>) -> bool {
    let bot_configs: Vec<_> = state
        .config
        .telegram_bots()
        .into_iter()
        .map(|bot| (bot.bot_id.clone(), bot.token.to_string()))
        .collect();

    if bot_configs.is_empty() {
        if state.config.channel_enabled("telegram") {
            tracing::warn!(
                "Telegram channel is enabled but no bots have a token configured. \
                 Set channels.telegram.bots.<id>.token in egopulse.config.yaml."
            );
        }
        return false;
    }

    state
        .runtime_status
        .update_channel("telegram", ChannelState::Starting);
    let shared_chain_state = Arc::new(crate::channels::telegram::BotChainState::new());
    for (bot_id, token) in bot_configs {
        let telegram_state = Arc::clone(state);
        info!("Starting Telegram bot '{bot_id}'...");
        let bot_id_for_task = bot_id.clone();
        let chain_state = Arc::clone(&shared_chain_state);
        let handle_name = format!("telegram[{bot_id}]");
        state.supervisor.spawn_long_lived(
            TaskSpec::new(TaskKind::Channel, handle_name, Criticality::Critical),
            async move {
                crate::channels::telegram::start_telegram_bot_for_bot(
                    telegram_state,
                    &token,
                    &bot_id_for_task,
                    chain_state,
                )
                .await
                .map_err(|error| {
                    EgoPulseError::Channel(ChannelError::SendFailed(format!(
                        "telegram bot ({bot_id_for_task}) failed: {error}",
                    )))
                })
            },
        );
    }
    true
}

async fn spawn_runtime_services(state: &Arc<AppState>) {
    if state.config_path.is_some() {
        let reload_state = Arc::clone(state);
        let token = state.supervisor.shutdown_token();
        state.supervisor.spawn_long_lived(
            TaskSpec::new(
                TaskKind::ConfigReload,
                "config-reload",
                Criticality::NonCritical,
            ),
            async move { config_reload::run_config_reload_loop(reload_state, token).await },
        );
    }

    let scheduler_state = Arc::clone(state);
    let token = state.supervisor.shutdown_token();
    info!("Starting sleep batch scheduler");
    state.supervisor.spawn_long_lived(
        TaskSpec::new(
            TaskKind::SleepScheduler,
            "sleep-scheduler",
            Criticality::NonCritical,
        ),
        async move { crate::sleep::scheduler::run_scheduler_loop(scheduler_state, token).await },
    );

    match call_blocking(Arc::clone(&state.db), |db| db.reap_orphaned_pulse_runs()).await {
        Ok(n) if n > 0 => info!("reaped {n} orphaned pulse_runs on startup"),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "failed to reap orphaned pulse_runs on startup"),
    }

    let pulse_state = Arc::clone(state);
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

    if state.config.db.backup.scheduler_enabled() {
        let backup_state = Arc::clone(state);
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
}

async fn supervise_runtime(state: &AppState) -> Result<(), EgoPulseError> {
    state.supervisor.start_accepting();
    info!("Runtime active; waiting for Ctrl-C or channel failure");

    loop {
        if let Some(outcome) = state.supervisor.poll_long_lived() {
            let summary = outcome.failure_summary();
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

/// Starts all enabled channels and supervises them until shutdown or failure.
///
/// Every long-lived task (channel listeners, schedulers) is owned by the
/// runtime supervisor. The run loop watches for critical task failures and
/// Ctrl-C; on either trigger it runs `RuntimeSupervisor::shutdown`, which
/// stops accepting input, drains in-flight turns, then drains long-lived tasks
/// within bounded deadlines.
pub async fn start_channels(state: Arc<AppState>) -> Result<(), EgoPulseError> {
    let mut has_active_channels = spawn_web_channel(&state);

    #[cfg(feature = "channel-discord")]
    {
        has_active_channels |= spawn_discord_channels(&state);
    }
    #[cfg(feature = "channel-telegram")]
    {
        has_active_channels |= spawn_telegram_channels(&state);
    }

    if !has_active_channels {
        return Err(EgoPulseError::Config(
            crate::error::ConfigError::NoActiveChannels,
        ));
    }

    spawn_runtime_services(&state).await;
    supervise_runtime(&state).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::ResolvedLlmConfig;
    use crate::conversation::ConversationScope;

    fn test_config_for_runtime(state_root: String) -> crate::config::Config {
        crate::test_util::test_config(&state_root)
    }

    #[test]
    fn second_acquisition_on_same_state_root_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let _g1 = InstanceGuard::acquire(dir.path()).unwrap();
        let err = InstanceGuard::acquire(dir.path()).unwrap_err();
        assert!(
            matches!(err, EgoPulseError::RuntimeAlreadyRunning(_)),
            "second runtime must be rejected: {err}"
        );
    }

    #[test]
    fn lock_is_released_after_drop() {
        let dir = tempfile::TempDir::new().unwrap();
        let g1 = InstanceGuard::acquire(dir.path()).unwrap();
        drop(g1);
        let _g2 = InstanceGuard::acquire(dir.path())
            .expect("lock should be reacquirable after the first guard is dropped");
    }

    #[test]
    fn distinct_state_roots_do_not_conflict() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        let _g1 = InstanceGuard::acquire(a.path()).unwrap();
        let _g2 = InstanceGuard::acquire(b.path()).unwrap();
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
        let snapshot = state.runtime_status.snapshot();
        assert!(!snapshot.version.is_empty());
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
    fn cache_key_separates_provider_model_url_and_api_key() {
        let base = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        let mut different_api_key = base.clone();
        different_api_key.api_key = Some(secrecy::SecretString::new(
            "sk-other".to_string().into_boxed_str(),
        ));

        for different in [
            resolved_config("anthropic", "gpt-4o", "https://api.openai.com/v1"),
            resolved_config("openai", "gpt-4o-mini", "https://api.openai.com/v1"),
            resolved_config("openai", "gpt-4o", "https://proxy.example.com/v1"),
            different_api_key,
        ] {
            assert_ne!(
                base.cache_key_with_revision(0),
                different.cache_key_with_revision(0)
            );
        }

        let identical = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        assert_eq!(
            base.cache_key_with_revision(0),
            identical.cache_key_with_revision(0)
        );
    }

    #[tokio::test]
    async fn llm_for_context_reuses_cached_provider() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let state = build_app_state(config).await.expect("build state");
        let context = crate::test_util::cli_context("cache-test");

        let runtime = state.turn_dependencies();
        let snapshot = state.config_manager.current_blocking();
        let a = runtime
            .llm_for_context_with_snapshot(&context, &snapshot)
            .expect("llm");
        let b = runtime
            .llm_for_context_with_snapshot(&context, &snapshot)
            .expect("llm");

        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn llm_cache_keeps_providers_for_each_config_revision() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let state = build_app_state(config).await.expect("build state");
        let resolved = resolved_config("openai", "gpt-4o", "https://api.openai.com/v1");
        let runtime = state.turn_dependencies();

        let newer = runtime
            .cached_provider(&resolved, 2)
            .expect("newer revision provider");
        let older = runtime
            .cached_provider(&resolved, 1)
            .expect("older revision provider");

        assert!(!Arc::ptr_eq(&newer, &older));
        assert_eq!(state.llm_cache.lock().expect("cache lock").len(), 2);
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

        let snapshot = state.config_manager.current_blocking();
        let result = state
            .turn_dependencies()
            .llm_for_context_with_snapshot(&context, &snapshot)
            .expect("llm");
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
    fn db_for_routes_normal_and_secret_scopes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = build_sleep_state(&dir);
        let secret_path = dir.path().join("runtime").join("secret.db");
        let secret_db = Arc::new(Database::new_secret(&secret_path).expect("secret db"));
        state.secret_db = Some(Arc::clone(&secret_db));

        let normal_db = state.db_for(ConversationScope::Normal);
        let secret_result = state.db_for(ConversationScope::Secret);

        assert!(
            Arc::ptr_eq(&normal_db, &state.db),
            "Normal scope must return the primary database"
        );
        assert!(
            Arc::ptr_eq(&secret_result, &secret_db),
            "Secret scope must return the isolated secret database"
        );
        assert!(
            !Arc::ptr_eq(&normal_db, &secret_result),
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
}

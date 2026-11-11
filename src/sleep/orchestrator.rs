//! Sleep batch orchestrator — coordinates event_extraction, episodic_update,
//! semantic_update, and publish/finalize.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use chrono::Datelike;
use chrono_tz::OffsetComponents;

use tracing::{info, warn};

use crate::agent_loop::compaction::archive_conversation_blocking;
use crate::agent_loop::session_snapshot::SessionContext;
use crate::error::EgoPulseError;
use crate::llm::{LlmProvider, Message};
use crate::memory::{MemoryBundle, MemoryError};
use crate::runtime::AppState;
use crate::storage::{
    AgentSessionInfo, CheckpointSourceKind, Database, EpisodeEvent, MemoryFile, MemorySnapshot,
    RollupGranularity, SleepRun, SleepRunStatus, SleepRunTrigger, SleepStepCheckpoint,
    SleepStepName, SleepStepResult, SleepStepStatus, call_blocking,
};

use super::SleepBatchError;
use super::episodic_renderer;
use super::event_extraction::{self, ExtractedEvent};
use super::event_rollup;
use super::prompt::chunk_token_budget;
use super::semantic_update;

/// Threshold (≤ 64) at which sleep is skipped due to too few new messages.
pub(crate) const SKIP_THRESHOLD: i64 = 64;
/// Maximum number of source sessions included in sleep input.
const MAX_SOURCE_SESSIONS: usize = 20;
/// Number of trailing messages kept in `sessions.messages_json` after a sleep
/// batch clears the conversation buffer. The `messages` table retains
/// everything; this only controls the agent-loop's live context residue.
const SESSION_CLEAR_KEEP_RECENT: usize = 4;

/// Decision from checking whether a sleep run has any work to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputDecision {
    /// No work to do → skip the sleep run.
    Skip {
        /// Human-readable reason for the skip.
        reason: String,
        /// Number of new messages found (≤ SKIP_THRESHOLD).
        new_message_count: i64,
    },
    /// There is message volume above the threshold or unprocessed semantic
    /// events → proceed with sleep run.
    Proceed {
        /// Source sessions (limited to MAX_SOURCE_SESSIONS). Empty when the run
        /// only drains pending semantic events.
        sessions: Vec<AgentSessionInfo>,
        /// JSON array of source chat metadata (for sleep_runs.source_chats_json).
        source_chats_json: String,
    },
}

/// Collects sleep input from step checkpoints. Proceeds when new messages
/// exceed the threshold or when episode events remain unprocessed by
/// `semantic_update`; source sessions are selected only in the former case.
///
/// # Errors
///
/// Returns [`StorageError`] if DB queries fail.
pub(crate) fn collect_sleep_input(
    db: &Database,
    agent_id: &str,
) -> Result<InputDecision, crate::error::StorageError> {
    let new_message_count = db.count_agent_pending_sleep_messages(agent_id)?;
    let has_pending_events = db.has_agent_pending_semantic_events(agent_id)?;

    if new_message_count <= SKIP_THRESHOLD && !has_pending_events {
        let reason = format!(
            "{new_message_count} new messages; runs when there are more than {SKIP_THRESHOLD}"
        );
        return Ok(InputDecision::Skip {
            reason,
            new_message_count,
        });
    }

    // Only message extraction is threshold-gated. A run triggered solely by a
    // semantic backlog passes no sessions, so event_extraction is skipped while
    // the event-derived updates still run.
    let sessions = if new_message_count > SKIP_THRESHOLD {
        db.get_agent_sessions_with_pending_sleep_messages(agent_id, MAX_SOURCE_SESSIONS)?
    } else {
        Vec::new()
    };
    let source_chats_json =
        serde_json::to_string(&sessions).map_err(crate::error::StorageError::SessionSerialize)?;

    Ok(InputDecision::Proceed {
        sessions,
        source_chats_json,
    })
}

/// Resolve a timezone string to a [`chrono::FixedOffset`] for the current moment.
///
/// Accepts IANA timezone names (e.g. `America/Los_Angeles`, `Asia/Tokyo`),
/// `UTC`, `Z`, and `UTC±HH:MM` offset literals. Falls back to UTC on
/// unrecognised input.
pub(crate) fn resolve_fixed_offset(tz_str: &str) -> chrono::FixedOffset {
    if let Ok(tz) = tz_str.parse::<chrono_tz::Tz>() {
        let now = chrono::Utc::now().with_timezone(&tz);
        let offset_secs = now.offset().base_utc_offset().num_seconds() as i32;
        return chrono::FixedOffset::east_opt(offset_secs)
            .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC+0 is valid"));
    }
    let seconds = match tz_str {
        "UTC" | "Z" => 0,
        _ => {
            let offset_part = tz_str.strip_prefix("UTC").unwrap_or(tz_str);
            parse_hhmm_offset(offset_part).unwrap_or(0)
        }
    };
    chrono::FixedOffset::east_opt(seconds)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC+0 is valid"))
}

fn parse_hhmm_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let s = s.trim_start_matches(['+', '-']);
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 2 {
        let hours: i32 = parts[0].parse().ok()?;
        let minutes: i32 = parts[1].parse().ok()?;
        Some(sign * (hours * 60 + minutes) * 60)
    } else if s.len() >= 2 {
        let hours: i32 = s[..2].parse().ok()?;
        let minutes: i32 = if s.len() >= 4 {
            s[2..4].parse().ok()?
        } else {
            0
        };
        Some(sign * (hours * 60 + minutes) * 60)
    } else {
        None
    }
}

pub async fn run_sleep_batch(
    state: &AppState,
    agent_id: Option<&str>,
    trigger: SleepRunTrigger,
) -> Result<(), SleepBatchError> {
    run_sleep_batch_with_snapshot(
        state,
        agent_id,
        trigger,
        state.config_manager.current_blocking(),
    )
    .await
}

pub(crate) async fn run_sleep_batch_with_snapshot(
    state: &AppState,
    agent_id: Option<&str>,
    trigger: SleepRunTrigger,
    snapshot: Arc<crate::config::manager::ConfigSnapshot>,
) -> Result<(), SleepBatchError> {
    let resolved_agent = match agent_id {
        Some(id) => id.to_string(),
        None => snapshot.config.default_agent.as_str().to_string(),
    };

    let db = Arc::clone(&state.db);

    let agent_for_collect = resolved_agent.clone();
    let decision = call_blocking(Arc::clone(&db), move |db| {
        collect_sleep_input(db, &agent_for_collect)
    })
    .await?;

    match decision {
        InputDecision::Skip {
            reason,
            new_message_count,
        } => {
            info!(
                agent_id = %resolved_agent,
                new_message_count,
                reason,
                "sleep batch skipped"
            );
            call_blocking(db, move |db| {
                db.record_skipped_sleep_run(&resolved_agent, trigger, &reason)
            })
            .await?;
            Ok(())
        }
        InputDecision::Proceed {
            sessions,
            source_chats_json,
        } => {
            execute_batch(
                state,
                db,
                &resolved_agent,
                &sessions,
                &source_chats_json,
                trigger,
                snapshot,
            )
            .await
        }
    }
}

/// Extracts episode events from past conversation history for backfilling.
///
/// Unlike normal sleep batch (which runs the full event_extraction →
/// episodic_update → semantic_update → publish sequence), this only runs
/// event_extraction using the messages table. Old backfill events in the
/// same period are replaced in a single transaction with new results.
///
/// # Parameters
/// - `state`: Application state with DB and config.
/// - `agent_id`: Target agent; defaults to config's `default_agent`.
/// - `from` / `to`: UTC RFC3339 timestamp range `[from, to)` for messages.
///   `None` means no boundary.
///
/// # Errors
/// Returns [`SleepBatchError`] on database, I/O, or LLM failures.
/// Returns [`SleepBatchError::AlreadyRunning`] if a backfill is already in
/// progress for the same agent.
///
/// # Panics
/// None.
pub async fn run_events_extract(
    state: &AppState,
    agent_id: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(), SleepBatchError> {
    let snapshot = state.config_manager.current_blocking();
    let resolved_agent = match agent_id {
        Some(id) => id.to_string(),
        None => snapshot.config.default_agent.as_str().to_string(),
    };

    let from_owned = from.map(str::to_string);
    let to_owned = to.map(str::to_string);

    let db = Arc::clone(&state.db);

    let agent_for_run = resolved_agent.clone();
    let run_id = call_blocking(Arc::clone(&db), move |db| {
        db.try_create_sleep_run(&agent_for_run, SleepRunTrigger::Backfill)
    })
    .await?;

    let run_id = match run_id {
        Some(id) => id,
        None => {
            return Err(SleepBatchError::AlreadyRunning {
                agent_id: resolved_agent,
            });
        }
    };

    let result = async {
        let resolved = snapshot
            .config
            .resolve_sleep_batch_llm()
            .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

        let provider: Arc<dyn LlmProvider> =
            if let Some(override_provider) = state.llm_override.clone() {
                override_provider
            } else {
                let revision = snapshot.revision;
                let headers = state
                    .turn_dependencies()
                    .header_context_for_agent(&resolved, &resolved_agent)
                    .map_err(|error| SleepBatchError::Internal(error.to_string()))?;
                state
                    .turn_dependencies()
                    .cached_provider(&resolved, revision, headers.as_ref())
                    .map_err(|e| SleepBatchError::Llm(e.to_string()))?
            };

        let context_tokens = snapshot.config.resolve_context_window_tokens(
            &crate::config::ProviderId::new(&resolved.provider),
            &resolved.model,
        );
        let chunk_session_tokens = chunk_token_budget(context_tokens);

        let agent_for_chats = resolved_agent.clone();
        let from_for_chats = from_owned.clone();
        let to_for_chats = to_owned.clone();
        let chats: Vec<(i64, String, String)> = call_blocking(Arc::clone(&db), move |db| {
            db.get_agent_chats_with_messages_between(
                &agent_for_chats,
                from_for_chats.as_deref(),
                to_for_chats.as_deref(),
            )
        })
        .await?;

        let sources: Vec<(i64, &str, &str)> = chats
            .iter()
            .map(|(chat_id, channel, ext_id)| (*chat_id, channel.as_str(), ext_id.as_str()))
            .collect();

        let chunks = event_extraction::build_extract_chunks(
            &db,
            &sources,
            from_owned.as_deref(),
            to_owned.as_deref(),
            chunk_session_tokens,
        )?;

        let total_chunks = chunks.len();
        let (extracted_events, input_tokens, output_tokens) =
            event_extraction::run_extract_events_for_chunks(
                &provider,
                &resolved_agent,
                chunks,
                total_chunks,
            )
            .await?;

        let episode_events =
            event_extraction::to_episode_events(extracted_events, &resolved_agent, &run_id);
        let event_count = episode_events.len();

        let agent_for_replace = resolved_agent.clone();
        let run_id_for_replace = run_id.clone();
        let from_for_replace = from_owned.clone();
        let to_for_replace = to_owned.clone();
        call_blocking(Arc::clone(&db), move |db| {
            db.replace_backfill_episode_events(
                &agent_for_replace,
                from_for_replace.as_deref(),
                to_for_replace.as_deref(),
                &run_id_for_replace,
                &episode_events,
            )
        })
        .await?;

        info!(count = event_count, "backfilled episode events");

        let run_id_owned = run_id.clone();
        let source_chats_json =
            serde_json::to_string(&sources).unwrap_or_else(|_| "[]".to_string());
        call_blocking(Arc::clone(&db), move |db| {
            db.update_sleep_run_success(
                &run_id_owned,
                &source_chats_json,
                None,
                input_tokens,
                output_tokens,
            )
        })
        .await?;

        Ok::<(), SleepBatchError>(())
    }
    .await;

    if let Err(error) = result {
        warn!(error = %error, "events extract failed");
        let run_id_owned = run_id.clone();
        let error_message = error.to_string();
        call_blocking(db, move |db| {
            db.update_sleep_run_failed(&run_id_owned, &error_message)
        })
        .await?;
        return Err(error);
    }

    info!(agent_id = %resolved_agent, run_id = %run_id, "events extract completed");
    Ok(())
}

async fn call_rollup_llm_with_retry(
    provider: &Arc<dyn LlmProvider>,
    system_prompt: &str,
    input_json: &str,
    valid_keys: &HashSet<String>,
    agent_id: &str,
) -> Result<(Vec<event_rollup::RollupOutputItem>, i64, i64), SleepBatchError> {
    let user_prompt = event_rollup::build_rollup_user_prompt(input_json);
    let user_message = Message::text("user", user_prompt);

    let response = super::llm_call::send(
        provider,
        agent_id,
        system_prompt,
        Arc::new(vec![user_message.clone()]),
        "episodic_update",
    )
    .await?;

    let first_input = response.usage.as_ref().map_or(0, |u| u.input_tokens);
    let first_output = response.usage.as_ref().map_or(0, |u| u.output_tokens);
    let output_json = event_rollup::redact_secrets(&response.content);

    match event_rollup::parse_rollup_output(&output_json, valid_keys) {
        Ok(outputs) => Ok((outputs, first_input, first_output)),
        Err(first_error) => {
            warn!(
                agent_id = %agent_id,
                error = %first_error,
                "rollup parse failed; retrying once"
            );
            const ROLLUP_RETRY_GUARD: &str = "\
Your previous response was not valid JSON according to the expected schema. \
You must respond with ONLY a JSON object containing exactly one key: \
\"rollups\" (an array of rollup objects). \
Each rollup must have: granularity, period_key, summary_md, max_ripple, event_count. \
Do not include any other keys, markdown formatting, code blocks, or explanatory text. \
Output the raw JSON object and nothing else.";
            let retry_messages = vec![
                user_message,
                Message::text("assistant", &response.content),
                Message::text("user", ROLLUP_RETRY_GUARD),
            ];
            let retry_response = super::llm_call::send(
                provider,
                agent_id,
                system_prompt,
                Arc::new(retry_messages),
                "episodic_update",
            )
            .await?;

            let retry_input = retry_response.usage.as_ref().map_or(0, |u| u.input_tokens);
            let retry_output = retry_response.usage.as_ref().map_or(0, |u| u.output_tokens);
            let combined_input = first_input.saturating_add(retry_input);
            let combined_output = first_output.saturating_add(retry_output);
            let retry_json = event_rollup::redact_secrets(&retry_response.content);

            match event_rollup::parse_rollup_output(&retry_json, valid_keys) {
                Ok(outputs) => Ok((outputs, combined_input, combined_output)),
                Err(retry_error) => {
                    warn!(
                        agent_id = %agent_id,
                        error = %retry_error,
                        "rollup retry also failed"
                    );
                    Err(SleepBatchError::ParseFailed(retry_error.to_string()))
                }
            }
        }
    }
}

/// Batch execution context — holds shared state for step execution.
struct BatchContext {
    run_id: String,
    provider: Arc<dyn LlmProvider>,
    sessions: Vec<AgentSessionInfo>,
    /// Memory bundle captured at run start. Snapshot `content_before` values
    /// and the publication precondition are derived from this.
    base_memory: MemoryBundle,
    /// In-memory candidate updated by each step. Published atomically at
    /// finalize; never written to disk mid-run.
    candidate_memory: MemoryBundle,
    context_tokens: usize,
    config: Arc<crate::config::Config>,
    assets: Arc<crate::assets::AssetStore>,
}

struct ExtractionInput {
    chunks: Vec<String>,
    checkpoints: Vec<SleepStepCheckpoint>,
}

fn load_extraction_input(
    ctx: &BatchContext,
    db: &Database,
    agent_id: &str,
) -> Result<ExtractionInput, SleepBatchError> {
    let max_tokens = chunk_token_budget(ctx.context_tokens);
    let max_chars = max_tokens.saturating_mul(3);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut checkpoints = Vec::new();

    for session in &ctx.sessions {
        let source_id = session.chat_id.to_string();
        let Some(upper_bound) = db.get_latest_message_cursor(session.chat_id)? else {
            continue;
        };
        let checkpoint = db.get_sleep_checkpoint(
            agent_id,
            SleepStepName::EventExtraction,
            CheckpointSourceKind::Messages,
            &source_id,
        )?;
        let cursor = checkpoint
            .as_ref()
            .map(|value| (value.cursor_at.as_str(), value.cursor_id.as_str()));
        let messages = db.get_messages_after_cursor(
            session.chat_id,
            cursor,
            (&upper_bound.0, &upper_bound.1),
        )?;
        let Some(last) = messages.last() else {
            continue;
        };

        checkpoints.push(SleepStepCheckpoint {
            agent_id: agent_id.to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id,
            cursor_at: last.timestamp.clone(),
            cursor_id: last.id.clone(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });

        let text = event_extraction::messages_to_extract_text(&messages);
        for block in event_extraction::session_blocks(session, &text, max_chars) {
            event_extraction::append_chunk_block(&mut chunks, &mut current, block, max_chars);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    Ok(ExtractionInput {
        chunks,
        checkpoints,
    })
}

async fn execute_batch(
    state: &AppState,
    db: Arc<Database>,
    agent_id: &str,
    sessions: &[AgentSessionInfo],
    source_chats_json: &str,
    trigger: SleepRunTrigger,
    snapshot: Arc<crate::config::manager::ConfigSnapshot>,
) -> Result<(), SleepBatchError> {
    let run_id = create_sleep_run(&db, agent_id, trigger).await?;

    // Persist the archive-target plan (the serialized source sessions) BEFORE
    // any publication, so a crash between publication and finalization still
    // leaves startup recovery enough to drive the same archive/truncate step
    // and converge to the normal-path state.
    let run_id_for_source = run_id.clone();
    let source_chats = source_chats_json.to_string();
    call_blocking(Arc::clone(&db), move |db| {
        db.update_sleep_run_source_chats(&run_id_for_source, &source_chats)
    })
    .await?;

    let result = async {
        let mut ctx = prepare_batch_context(state, agent_id, sessions, &run_id, &snapshot).await?;
        run_event_extraction_step(&mut ctx, &db, agent_id).await?;
        run_episodic_update_step(&mut ctx, state, &db, agent_id).await?;
        run_semantic_update_step(&mut ctx, &db, agent_id).await?;

        finalize_batch(&ctx, state, &db, agent_id).await?;

        Ok::<(), SleepBatchError>(())
    }
    .await;

    if let Err(error) = result {
        // PublicationPending and ArchivePending intentionally leave the run
        // `running` so the candidate snapshot set, or the un-archived source
        // sessions, survive for retry (scheduler cycle or startup recovery);
        // only real failures mark the run failed.
        if !matches!(
            error,
            SleepBatchError::PublicationPending { .. } | SleepBatchError::ArchivePending { .. }
        ) {
            let run_id_owned = run_id.clone();
            let error_message = error.to_string();
            call_blocking(db, move |db| {
                db.update_sleep_run_failed(&run_id_owned, &error_message)
            })
            .await?;
        }
        return Err(error);
    }

    info!(agent_id = %agent_id, run_id = %run_id, "sleep batch completed");
    Ok(())
}

/// Creates a sleep run record and returns the run ID.
async fn create_sleep_run(
    db: &Arc<Database>,
    agent_id: &str,
    trigger: SleepRunTrigger,
) -> Result<String, SleepBatchError> {
    let agent_for_run = agent_id.to_string();
    let run_id = call_blocking(Arc::clone(db), move |db| {
        db.try_create_sleep_run(&agent_for_run, trigger)
    })
    .await?;

    run_id.ok_or_else(|| SleepBatchError::AlreadyRunning {
        agent_id: agent_id.to_string(),
    })
}

/// Prepares batch context: LLM provider, chunks, memory state.
async fn prepare_batch_context(
    state: &AppState,
    agent_id: &str,
    sessions: &[AgentSessionInfo],
    run_id: &str,
    snapshot: &Arc<crate::config::manager::ConfigSnapshot>,
) -> Result<BatchContext, SleepBatchError> {
    let config = Arc::new(snapshot.config.clone());
    let resolved = config
        .resolve_sleep_batch_llm()
        .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

    let provider: Arc<dyn LlmProvider> = if let Some(override_provider) = state.llm_override.clone()
    {
        override_provider
    } else {
        let revision = snapshot.revision;
        let headers = state
            .turn_dependencies()
            .header_context_for_agent(&resolved, agent_id)
            .map_err(|error| SleepBatchError::Internal(error.to_string()))?;
        state
            .turn_dependencies()
            .cached_provider(&resolved, revision, headers.as_ref())
            .map_err(|e| SleepBatchError::Llm(e.to_string()))?
    };

    let context_tokens = config.resolve_context_window_tokens(
        &crate::config::ProviderId::new(&resolved.provider),
        &resolved.model,
    );
    let memory_before = state
        .memory_loader
        .load_bundle(agent_id)
        .map_err(|e| SleepBatchError::Io(e.to_string()))?;

    Ok(BatchContext {
        run_id: run_id.to_string(),
        provider,
        sessions: sessions.to_vec(),
        base_memory: (*memory_before).clone(),
        candidate_memory: (*memory_before).clone(),
        context_tokens,
        config,
        assets: std::sync::Arc::clone(&state.assets),
    })
}

/// Event Extraction — extracts episode events from session chunks.
async fn run_event_extraction_step(
    ctx: &mut BatchContext,
    db: &Arc<Database>,
    agent_id: &str,
) -> Result<(), SleepBatchError> {
    let run_id = ctx.run_id.clone();
    call_blocking(Arc::clone(db), move |db| {
        db.start_sleep_step(&run_id, SleepStepName::EventExtraction)
    })
    .await?;

    let input = match load_extraction_input(ctx, db, agent_id) {
        Ok(input) => input,
        Err(error) => {
            finish_step_failed(
                db,
                &ctx.run_id,
                SleepStepName::EventExtraction,
                error.to_string(),
            )
            .await;
            return Ok(());
        }
    };
    if input.chunks.is_empty() {
        finish_step_skipped(db, &ctx.run_id, SleepStepName::EventExtraction).await;
        return Ok(());
    }

    let extract_chunks = input.chunks;
    let total_chunks = extract_chunks.len();

    let extract_result: Result<(Vec<ExtractedEvent>, i64, i64), SleepBatchError> = async {
        event_extraction::run_extract_events_for_chunks(
            &ctx.provider,
            agent_id,
            extract_chunks,
            total_chunks,
        )
        .await
    }
    .await;

    let run_id = ctx.run_id.clone();
    match extract_result {
        Ok((extracted_events, inp, out)) => {
            let episode_events =
                event_extraction::to_episode_events(extracted_events, agent_id, &ctx.run_id);
            let event_count = episode_events.len();
            let checkpoints = input.checkpoints;
            let rid = run_id.clone();
            let agent_id = agent_id.to_string();
            if let Err(error) = call_blocking(Arc::clone(db), move |db| {
                db.commit_event_extraction_success(
                    &rid,
                    &agent_id,
                    &episode_events,
                    SleepStepResult {
                        status: SleepStepStatus::Success,
                        input_tokens: inp,
                        output_tokens: out,
                        error_message: None,
                        metadata_json: None,
                    },
                    &checkpoints,
                )
            })
            .await
            {
                warn!(error = %error, "failed to commit event extraction");
                finish_step_failed(
                    db,
                    &ctx.run_id,
                    SleepStepName::EventExtraction,
                    error.to_string(),
                )
                .await;
                return Ok(());
            }
            info!(count = event_count, "extracted episode events");
        }
        Err(e) => {
            warn!(error = %e, "event extraction failed, continuing");
            let rid = run_id.clone();
            let err_msg = e.to_string();
            call_blocking(Arc::clone(db), move |db| {
                db.finish_sleep_step(
                    &rid,
                    SleepStepName::EventExtraction,
                    SleepStepResult {
                        status: SleepStepStatus::Failed,
                        input_tokens: 0,
                        output_tokens: 0,
                        error_message: Some(&err_msg),
                        metadata_json: None,
                    },
                )
            })
            .await
            .ok();
        }
    }
    Ok(())
}

async fn finish_step_failed(
    db: &Arc<Database>,
    run_id: &str,
    step_name: SleepStepName,
    error_message: String,
) {
    let run_id = run_id.to_string();
    call_blocking(Arc::clone(db), move |db| {
        db.finish_sleep_step(
            &run_id,
            step_name,
            SleepStepResult {
                status: SleepStepStatus::Failed,
                input_tokens: 0,
                output_tokens: 0,
                error_message: Some(&error_message),
                metadata_json: None,
            },
        )
    })
    .await
    .ok();
}

async fn finish_step_skipped(db: &Arc<Database>, run_id: &str, step_name: SleepStepName) {
    let run_id = run_id.to_string();
    call_blocking(Arc::clone(db), move |db| {
        db.finish_sleep_step(
            &run_id,
            step_name,
            SleepStepResult {
                status: SleepStepStatus::Skipped,
                input_tokens: 0,
                output_tokens: 0,
                error_message: None,
                metadata_json: None,
            },
        )
    })
    .await
    .ok();
}

/// Episodic Update — rollup generation and episodic.md rendering.
async fn run_episodic_update_step(
    ctx: &mut BatchContext,
    _state: &AppState,
    db: &Arc<Database>,
    agent_id: &str,
) -> Result<Option<String>, SleepBatchError> {
    let run_id = ctx.run_id.clone();
    call_blocking(Arc::clone(db), move |db| {
        db.start_sleep_step(&run_id, SleepStepName::EpisodicUpdate)
    })
    .await?;

    let tz_str = ctx.config.timezone.clone();
    let tz_chrono: chrono_tz::Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);
    let tz = resolve_fixed_offset(&tz_str);
    let now = chrono::Utc::now().with_timezone(&tz);
    let cw = event_rollup::current_week(now, tz_chrono);
    let cw_start = cw.period_start.to_rfc3339();
    let cw_end = cw.period_end_exclusive.to_rfc3339();

    let agent_for_events = agent_id.to_string();
    let current_week_events: Vec<EpisodeEvent> = match call_blocking(Arc::clone(db), move |db| {
        db.list_episode_events_in_range(&agent_for_events, &cw_start, &cw_end)
    })
    .await
    {
        Ok(events) => events,
        Err(e) => {
            warn!(error = %e, "episodic: failed to load current week events");
            finish_step_failed(
                db,
                &ctx.run_id,
                SleepStepName::EpisodicUpdate,
                e.to_string(),
            )
            .await;
            return Ok(None);
        }
    };

    let (step_in, step_out, changed) =
        match run_rollup_logic(ctx, db, agent_id, now, tz_chrono, &cw).await {
            Ok(result) => result,
            Err(error) => {
                warn!(error = %error, "episodic update failed");
                finish_step_failed(
                    db,
                    &ctx.run_id,
                    SleepStepName::EpisodicUpdate,
                    error.to_string(),
                )
                .await;
                return Ok(None);
            }
        };
    if !changed {
        finish_step_skipped(db, &ctx.run_id, SleepStepName::EpisodicUpdate).await;
        return Ok(None);
    }

    let Some(rendered) =
        episodic_renderer::render_episodic_view(db, agent_id, &tz_str, &cw, &current_week_events)
            .await
    else {
        let error = SleepBatchError::Internal("episodic renderer produced no output".to_string());
        finish_step_failed(
            db,
            &ctx.run_id,
            SleepStepName::EpisodicUpdate,
            error.to_string(),
        )
        .await;
        return Ok(None);
    };

    let content_before = ctx.base_memory.episodic.clone();
    let content_after = rendered.clone();
    let run_id = ctx.run_id.clone();
    let agent_id_owned = agent_id.to_string();
    if let Err(error) = call_blocking(Arc::clone(db), move |db| {
        db.commit_episodic_update_success(
            &run_id,
            &agent_id_owned,
            &content_before,
            &content_after,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: step_in,
                output_tokens: step_out,
                error_message: None,
                metadata_json: None,
            },
        )
    })
    .await
    {
        warn!(error = %error, "failed to commit episodic update");
        finish_step_failed(
            db,
            &ctx.run_id,
            SleepStepName::EpisodicUpdate,
            error.to_string(),
        )
        .await;
        return Ok(None);
    }

    ctx.candidate_memory.episodic = rendered.clone();
    Ok(Some(rendered))
}

/// Internal: Run rollup LLM calls for week and month rollups.
async fn run_rollup_logic(
    ctx: &mut BatchContext,
    db: &Arc<Database>,
    agent_id: &str,
    now: chrono::DateTime<chrono::FixedOffset>,
    tz_chrono: chrono_tz::Tz,
    cw: &event_rollup::WeekPeriod,
) -> Result<(i64, i64, bool), SleepBatchError> {
    let mut total_input: i64 = 0;
    let mut total_output: i64 = 0;
    let agent_for_plan = agent_id.to_string();
    let existing_week_rollups: Vec<event_rollup::ExistingRollupInfo> =
        call_blocking(Arc::clone(db), move |db| {
            db.list_episode_rollups(&agent_for_plan, RollupGranularity::Week, 100)
        })
        .await?
        .into_iter()
        .map(|r| event_rollup::ExistingRollupInfo {
            period_key: r.period_key,
            event_count: r.event_count,
            max_ripple: r.max_ripple,
            summary_md: r.summary_md,
        })
        .collect();

    let agent_for_months = agent_id.to_string();
    let existing_month_rollups: Vec<event_rollup::ExistingRollupInfo> =
        call_blocking(Arc::clone(db), move |db| {
            db.list_episode_rollups(&agent_for_months, RollupGranularity::Month, 100)
        })
        .await?
        .into_iter()
        .map(|r| event_rollup::ExistingRollupInfo {
            period_key: r.period_key,
            event_count: r.event_count,
            max_ripple: r.max_ripple,
            summary_md: r.summary_md,
        })
        .collect();

    let recent = event_rollup::recent_weeks(now, 4, tz_chrono);
    let earliest_start = recent
        .last()
        .map(|w| w.period_start.to_rfc3339())
        .unwrap_or_else(|| cw.period_start.to_rfc3339());
    let cw_end = cw.period_end_exclusive.to_rfc3339();
    let agent_for_all = agent_id.to_string();
    let all_events: Vec<EpisodeEvent> = call_blocking(Arc::clone(db), move |db| {
        db.list_episode_events_in_range(&agent_for_all, &earliest_start, &cw_end)
    })
    .await?;

    let planner_events: Vec<event_rollup::PlannerEvent> = all_events
        .iter()
        .map(|e| event_rollup::PlannerEvent {
            experienced_at: e.experienced_at.clone(),
            encoded_at: e.encoded_at.clone(),
            ripple_strength: e.ripple_strength,
        })
        .collect();

    let mut existing_week_rollups = existing_week_rollups;

    let week_requests = event_rollup::plan_week_rollup_updates(
        agent_id,
        now,
        tz_chrono,
        &event_rollup::RollupPlannerInput {
            existing_week_rollups: existing_week_rollups.clone(),
            events: planner_events,
        },
    );

    // Week rollup generation
    if !week_requests.is_empty() {
        let mut week_events_map: HashMap<String, Vec<event_rollup::RollupEvent>> = HashMap::new();
        for req in &week_requests {
            let req_start = req.period_start.clone();
            let req_end = req.period_end_exclusive.clone();
            let req_key = req.period_key.clone();
            let agent_for_range = agent_id.to_string();
            let period_events: Vec<EpisodeEvent> = call_blocking(Arc::clone(db), move |db| {
                db.list_episode_events_in_range(&agent_for_range, &req_start, &req_end)
            })
            .await?;

            let rollup_events: Vec<event_rollup::RollupEvent> = period_events
                .iter()
                .map(|e| event_rollup::RollupEvent {
                    id: e.id.clone(),
                    experienced_at: e.experienced_at.clone(),
                    kind: e.kind.to_string(),
                    title: e.title.clone(),
                    body_md: e.body_md.clone(),
                    ripple_strength: e.ripple_strength,
                    certainty: e.certainty.to_string(),
                })
                .collect();
            week_events_map.insert(req_key, rollup_events);
        }

        let week_input = event_rollup::build_week_rollup_requests(&week_requests, &week_events_map);
        let week_input_json = serde_json::to_string_pretty(&serde_json::json!({
            "rollup_requests": week_input
        }))
        .map_err(|e| SleepBatchError::Internal(e.to_string()))?;
        let week_input_json = event_rollup::redact_secrets(&week_input_json);

        let week_system_prompt = event_rollup::build_week_rollup_system_prompt(agent_id);
        let valid_keys: HashSet<String> =
            week_requests.iter().map(|r| r.period_key.clone()).collect();

        let (rollup_outputs, rollup_in, rollup_out) = call_rollup_llm_with_retry(
            &ctx.provider,
            &week_system_prompt,
            &week_input_json,
            &valid_keys,
            agent_id,
        )
        .await?;

        total_input = total_input.saturating_add(rollup_in);
        total_output = total_output.saturating_add(rollup_out);

        let requests_by_key: HashMap<&str, &event_rollup::RollupRequest> = week_requests
            .iter()
            .map(|r| (r.period_key.as_str(), r))
            .collect();

        for rollup_output in &rollup_outputs {
            let Some(request) = requests_by_key.get(rollup_output.period_key.as_str()) else {
                continue;
            };
            let (computed_max_ripple, computed_event_count) =
                event_rollup::compute_rollup_stats(week_events_map.get(&rollup_output.period_key));
            let rollup = crate::storage::EpisodeRollup {
                id: uuid::Uuid::new_v4().to_string(),
                agent_id: agent_id.to_string(),
                granularity: RollupGranularity::Week,
                period_key: rollup_output.period_key.clone(),
                period_start: request.period_start.clone(),
                period_end_exclusive: request.period_end_exclusive.clone(),
                summary_md: rollup_output.summary_md.clone(),
                max_ripple: computed_max_ripple,
                event_count: computed_event_count,
                generated_run_id: ctx.run_id.clone(),
                created_at: now.to_rfc3339(),
                updated_at: now.to_rfc3339(),
            };
            let rollup_for_db = rollup.clone();
            call_blocking(Arc::clone(db), move |db| {
                db.upsert_episode_rollup(&rollup_for_db)
            })
            .await?;
        }

        for rollup_output in &rollup_outputs {
            let (computed_max_ripple, computed_event_count) =
                event_rollup::compute_rollup_stats(week_events_map.get(&rollup_output.period_key));
            let updated = event_rollup::ExistingRollupInfo {
                period_key: rollup_output.period_key.clone(),
                event_count: computed_event_count,
                max_ripple: computed_max_ripple,
                summary_md: rollup_output.summary_md.clone(),
            };
            if let Some(idx) = existing_week_rollups
                .iter()
                .position(|r| r.period_key == rollup_output.period_key)
            {
                existing_week_rollups[idx] = updated;
            } else {
                existing_week_rollups.push(updated);
            }
        }
    }

    let month_requests = event_rollup::plan_month_rollup_updates(
        agent_id,
        now,
        tz_chrono,
        &existing_month_rollups,
        &existing_week_rollups,
    );

    // Month rollup generation
    if !month_requests.is_empty() {
        let mut week_rollups_map: HashMap<String, Vec<event_rollup::WeekRollupSummary>> =
            HashMap::new();
        for req in &month_requests {
            let summaries: Vec<event_rollup::WeekRollupSummary> = existing_week_rollups
                .iter()
                .filter(|wr| {
                    event_rollup::week_in_month(&wr.period_key, &req.period_key, tz_chrono)
                })
                .map(|wr| event_rollup::WeekRollupSummary {
                    period_key: wr.period_key.clone(),
                    summary_md: wr.summary_md.clone(),
                    max_ripple: wr.max_ripple,
                    event_count: wr.event_count,
                })
                .collect();
            week_rollups_map.insert(req.period_key.clone(), summaries);
        }

        let mut previous_month_map: HashMap<String, String> = HashMap::new();
        for req in &month_requests {
            if let Some(mp) = event_rollup::month_period_from_key(&req.period_key, tz_chrono) {
                let prev_year = if mp.period_start.month() == 1 {
                    mp.period_start.year() - 1
                } else {
                    mp.period_start.year()
                };
                let prev_month = if mp.period_start.month() == 1 {
                    12
                } else {
                    mp.period_start.month() - 1
                };
                let prev_key = format!("{}-{:02}", prev_year, prev_month);
                if let Some(prev_rollup) = existing_month_rollups
                    .iter()
                    .find(|r| r.period_key == prev_key)
                {
                    previous_month_map
                        .insert(req.period_key.clone(), prev_rollup.summary_md.clone());
                }
            }
        }

        let month_input = event_rollup::build_month_rollup_requests(
            &month_requests,
            &week_rollups_map,
            &previous_month_map,
        );
        let month_input_json = serde_json::to_string_pretty(&serde_json::json!({
            "rollup_requests": month_input
        }))
        .map_err(|e| SleepBatchError::Internal(e.to_string()))?;
        let month_input_json = event_rollup::redact_secrets(&month_input_json);

        let month_system_prompt = event_rollup::build_month_rollup_system_prompt(agent_id);
        let valid_keys: HashSet<String> = month_requests
            .iter()
            .map(|r| r.period_key.clone())
            .collect();

        let (rollup_outputs, rollup_in, rollup_out) = call_rollup_llm_with_retry(
            &ctx.provider,
            &month_system_prompt,
            &month_input_json,
            &valid_keys,
            agent_id,
        )
        .await?;

        total_input = total_input.saturating_add(rollup_in);
        total_output = total_output.saturating_add(rollup_out);

        let requests_by_key: HashMap<&str, &event_rollup::RollupRequest> = month_requests
            .iter()
            .map(|r| (r.period_key.as_str(), r))
            .collect();

        for rollup_output in &rollup_outputs {
            let Some(request) = requests_by_key.get(rollup_output.period_key.as_str()) else {
                continue;
            };

            let month_week_rollups: Vec<&event_rollup::ExistingRollupInfo> = existing_week_rollups
                .iter()
                .filter(|wr| {
                    event_rollup::week_in_month(
                        &wr.period_key,
                        &rollup_output.period_key,
                        tz_chrono,
                    )
                })
                .collect();
            let (computed_max_ripple, computed_event_count) =
                event_rollup::compute_month_rollup_stats(&month_week_rollups);

            let rollup = crate::storage::EpisodeRollup {
                id: uuid::Uuid::new_v4().to_string(),
                agent_id: agent_id.to_string(),
                granularity: RollupGranularity::Month,
                period_key: rollup_output.period_key.clone(),
                period_start: request.period_start.clone(),
                period_end_exclusive: request.period_end_exclusive.clone(),
                summary_md: rollup_output.summary_md.clone(),
                max_ripple: computed_max_ripple,
                event_count: computed_event_count,
                generated_run_id: ctx.run_id.clone(),
                created_at: now.to_rfc3339(),
                updated_at: now.to_rfc3339(),
            };
            let rollup_for_db = rollup.clone();
            call_blocking(Arc::clone(db), move |db| {
                db.upsert_episode_rollup(&rollup_for_db)
            })
            .await?;
        }
    }

    Ok((
        total_input,
        total_output,
        !week_requests.is_empty() || !month_requests.is_empty(),
    ))
}

/// semantic_update: folds episode events newer than the semantic checkpoint
/// into `semantic.md` (candidate only; published at finalize).
async fn run_semantic_update_step(
    ctx: &mut BatchContext,
    db: &Arc<Database>,
    agent_id: &str,
) -> Result<(), SleepBatchError> {
    let run_id = ctx.run_id.clone();
    call_blocking(Arc::clone(db), move |db| {
        db.start_sleep_step(&run_id, SleepStepName::SemanticUpdate)
    })
    .await?;

    let events = match load_semantic_events(db, agent_id) {
        Ok(events) => events,
        Err(error) => {
            warn!(error = %error, "semantic update: failed to load episode events");
            finish_step_failed(
                db,
                &ctx.run_id,
                SleepStepName::SemanticUpdate,
                error.to_string(),
            )
            .await;
            return Ok(());
        }
    };

    if events.is_empty() {
        finish_step_skipped(db, &ctx.run_id, SleepStepName::SemanticUpdate).await;
        return Ok(());
    }

    let max_chars = chunk_token_budget(ctx.context_tokens).saturating_mul(3);
    let batches = semantic_update::build_event_batches(&events, max_chars);
    let total_batches = batches.len();
    let mut working = ctx.candidate_memory.semantic.clone();
    let mut total_input: i64 = 0;
    let mut total_output: i64 = 0;
    let mut step_failed = false;
    let mut error_msg = None;

    for (index, events_json) in batches.into_iter().enumerate() {
        let input = match semantic_update::build_semantic_input(
            agent_id,
            working.clone(),
            events_json,
            ctx.context_tokens,
        ) {
            Ok(input) => input,
            Err(error) => {
                warn!(error = %error, "semantic update: failed to build input");
                step_failed = true;
                error_msg = Some(error.to_string());
                break;
            }
        };
        let system_prompt = semantic_update::build_semantic_system_prompt(&input);
        match semantic_update::send_semantic_request(
            &ctx.provider,
            agent_id,
            &system_prompt,
            index + 1,
            total_batches,
        )
        .await
        {
            Ok((semantic, inp, out)) => {
                total_input = total_input.saturating_add(inp);
                total_output = total_output.saturating_add(out);
                working = semantic;
            }
            Err(error) => {
                warn!(error = %error, "semantic update failed");
                step_failed = true;
                error_msg = Some(error.to_string());
                break;
            }
        }
    }

    if step_failed {
        let run_id = ctx.run_id.clone();
        call_blocking(Arc::clone(db), move |db| {
            db.finish_sleep_step(
                &run_id,
                SleepStepName::SemanticUpdate,
                SleepStepResult {
                    status: SleepStepStatus::Failed,
                    input_tokens: total_input,
                    output_tokens: total_output,
                    error_message: error_msg.as_deref(),
                    metadata_json: None,
                },
            )
        })
        .await?;
        return Ok(());
    }

    let last = events.last().expect("events are non-empty");
    let checkpoint = SleepStepCheckpoint {
        agent_id: agent_id.to_string(),
        step_name: SleepStepName::SemanticUpdate,
        source_kind: CheckpointSourceKind::EpisodeEvents,
        source_id: agent_id.to_string(),
        cursor_at: last.encoded_at.clone(),
        cursor_id: last.id.clone(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    let content_before = ctx.base_memory.semantic.clone();
    let content_after = working.clone();

    let run_id = ctx.run_id.clone();
    let agent_id_owned = agent_id.to_string();
    if let Err(error) = call_blocking(Arc::clone(db), move |db| {
        db.commit_semantic_update_success(
            &run_id,
            &agent_id_owned,
            &content_before,
            &content_after,
            &checkpoint,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: total_input,
                output_tokens: total_output,
                error_message: None,
                metadata_json: None,
            },
        )
    })
    .await
    {
        warn!(error = %error, "failed to commit semantic update");
        finish_step_failed(
            db,
            &ctx.run_id,
            SleepStepName::SemanticUpdate,
            error.to_string(),
        )
        .await;
        return Ok(());
    }

    ctx.candidate_memory.semantic = working;
    Ok(())
}

/// Loads episode events newer than the semantic checkpoint, bounded by the
/// latest event cursor at call time.
fn load_semantic_events(
    db: &Database,
    agent_id: &str,
) -> Result<Vec<EpisodeEvent>, SleepBatchError> {
    let checkpoint = db.get_sleep_checkpoint(
        agent_id,
        SleepStepName::SemanticUpdate,
        CheckpointSourceKind::EpisodeEvents,
        agent_id,
    )?;
    let cursor = checkpoint
        .as_ref()
        .map(|value| (value.cursor_at.clone(), value.cursor_id.clone()));
    let Some(upper_bound) = db.get_latest_episode_event_cursor(agent_id)? else {
        return Ok(Vec::new());
    };
    Ok(db.get_episode_events_after_cursor(
        agent_id,
        cursor
            .as_ref()
            .map(|(cursor_at, cursor_id)| (cursor_at.as_str(), cursor_id.as_str())),
        (&upper_bound.0, &upper_bound.1),
    )?)
}

/// Finalizes batch: writes memory files, archives sessions, logs usage.
async fn finalize_batch(
    ctx: &BatchContext,
    state: &AppState,
    db: &Arc<Database>,
    agent_id: &str,
) -> Result<(), SleepBatchError> {
    // Publish the candidate bundle BEFORE finalizing. A retryable publication
    // failure (manual-edit conflict or transient I/O, surfaced as
    // `SleepBatchError::Io`) is wrapped in `PublicationPending`: the run is
    // intentionally left `running` (not failed) so the candidate snapshot set
    // survives for retry by the next scheduler cycle or startup recovery. The
    // caller must NOT treat this as success. Non-retryable failures (incomplete
    // snapshot set, unsafe agent id, unclassifiable disk content) propagate
    // unchanged so `execute_batch` hard-fails the run instead of looping.
    if let Err(error) = publish_memory_bundle(state, db, ctx, agent_id).await {
        if let SleepBatchError::Io(reason) = error {
            warn!(
                agent_id = %agent_id,
                run_id = %ctx.run_id,
                error = %reason,
                "memory publication failed; run left running for retry"
            );
            return Err(SleepBatchError::PublicationPending {
                agent_id: agent_id.to_string(),
                run_id: ctx.run_id.clone(),
                reason,
            });
        }
        return Err(error);
    }

    // Publication succeeded: drive the shared post-publication finalization
    // (archive / truncate / finalize). Reused by startup recovery so both
    // paths converge to the same end state. The blocking helper does only
    // quick DB + file writes; like the checkpoint/archive reads elsewhere in
    // the batch it runs inline rather than via `call_blocking`.
    let run_id_for_load = ctx.run_id.clone();
    let run = call_blocking(Arc::clone(db), move |db| db.get_sleep_run(&run_id_for_load))
        .await?
        .ok_or_else(|| SleepBatchError::Internal(format!("sleep run vanished: {}", ctx.run_id)))?;
    let groups_dir = ctx.config.groups_dir();
    let secrets = crate::tools::collect_config_secrets(&ctx.config);
    let provider_name = ctx.provider.provider_name().to_string();
    let db_for_finalize = Arc::clone(db);
    // The post-publication finalization (archive writes, truncation, run
    // transition) is DB + filesystem I/O; run it on a blocking thread so the
    // async sleep batch never stalls the runtime on disk latency. This mirrors
    // how memory publication is offloaded above and keeps the normal path
    // symmetric with startup recovery, which runs this synchronously in a
    // blocking startup context.
    let assets = std::sync::Arc::clone(&ctx.assets);
    tokio::task::spawn_blocking(move || {
        finalize_published_run_blocking(
            db_for_finalize.as_ref(),
            assets.as_ref(),
            &groups_dir,
            &secrets,
            &run,
            Some(&provider_name),
        )
    })
    .await
    .map_err(|join_error| {
        SleepBatchError::Internal(format!("finalization task panicked: {join_error}"))
    })??;
    Ok(())
}

/// Synchronously drives the post-publication finalization for a run: archive +
/// truncate the source sessions, then transition the run `Running → Success`.
///
/// This is the single, idempotent convergence point shared by the normal path
/// ([`finalize_batch`], via `spawn_blocking`) and startup recovery
/// ([`recover_run_publication`], called directly). The archive-target sessions
/// are reconstructed from the run's `source_chats_json`, which is persisted
/// **before** publication so a crash between publication and this call still
/// leaves enough to converge.
///
/// Archive/truncate is a *required* condition for success: if any archivable
/// session fails to archive, the run is left `Running` (returning
/// [`SleepBatchError::ArchivePending`]) instead of being finalized as
/// `Success`. Idempotency: if the run is no longer `Running` the function is a
/// no-op, so a duplicate invocation (recovery after the normal path already
/// finalized) does not re-archive. Already-archived sessions are skipped on
/// retry because truncation removes their messages, so the archive cursor
/// check leaves them untouched. `finalize_sleep_run` itself is a conditional
/// `Running → Success` transition, providing the atomic gate.
///
/// `provider_name` controls the optional batch-level token metric bump; it is
/// `Some` on the normal path and `None` during recovery (per-call usage was
/// already logged by each step's LLM call).
fn finalize_published_run_blocking(
    db: &Database,
    assets: &crate::assets::AssetStore,
    groups_dir: &Path,
    secrets: &[(String, String)],
    run: &SleepRun,
    provider_name: Option<&str>,
) -> Result<(), SleepBatchError> {
    // Idempotency: nothing to do once the run left the Running state.
    if run.status != SleepRunStatus::Running {
        return Ok(());
    }
    let agent_id = run.agent_id.as_str();
    let sessions: Vec<AgentSessionInfo> =
        serde_json::from_str(&run.source_chats_json).map_err(|e| {
            SleepBatchError::Internal(format!("decode source_chats_json for run {}: {e}", run.id))
        })?;

    let mut archive_failures: Vec<String> = Vec::new();
    for session in &sessions {
        let source_id = session.chat_id.to_string();
        let event_checkpoint = db.get_sleep_checkpoint(
            agent_id,
            SleepStepName::EventExtraction,
            CheckpointSourceKind::Messages,
            &source_id,
        )?;
        // Archive only once event extraction has caught up with the session's
        // latest message; otherwise new activity arrived after this run's snapshot.
        let Some(event_checkpoint) = event_checkpoint else {
            continue;
        };
        let boundary = (event_checkpoint.cursor_at, event_checkpoint.cursor_id);
        let Some(latest) = db.get_latest_message_cursor(session.chat_id)? else {
            continue;
        };
        if latest > boundary {
            continue;
        }
        // Archivable now: the source can be durably moved and truncated. A
        // failure here MUST prevent the run from being finalized as `Success`.
        // Already-archived sessions are naturally skipped on retry: truncation
        // removes the messages, so `get_latest_message_cursor` returns `None`
        // and the session is not re-archived.
        if let Err(e) = archive_and_clear_session(db, assets, groups_dir, &run.id, session, secrets)
        {
            warn!(
                agent_id = %agent_id,
                chat_id = session.chat_id,
                error = %e,
                "failed to archive/clear session; run left running for retry"
            );
            archive_failures.push(session.chat_id.to_string());
        }
    }

    // Archive/truncate is a *required* condition for run success. If any
    // archivable session failed, do not finalize — leave the run `Running` so
    // startup recovery or the next scheduler cycle retries and converges.
    if !archive_failures.is_empty() {
        return Err(SleepBatchError::ArchivePending {
            agent_id: agent_id.to_string(),
            run_id: run.id.clone(),
            reason: format!("{} session(s) failed to archive", archive_failures.len()),
        });
    }

    let derived_status = db.finalize_sleep_run(&run.id)?;

    if let Some(provider) = provider_name {
        if let Ok(Some(token_run)) = db.get_sleep_run(&run.id) {
            if token_run.input_tokens > 0 || token_run.output_tokens > 0 {
                let provider_name = provider.to_string();
                crate::runtime::metrics::inc_llm_tokens_total(
                    "input",
                    &provider_name,
                    token_run.input_tokens,
                );
                crate::runtime::metrics::inc_llm_tokens_total(
                    "output",
                    &provider_name,
                    token_run.output_tokens,
                );
                // Per-call usage is logged by each sleep step's LLM call. The
                // batch-level aggregate has no single prompt estimate to pair
                // with, so it is not written to llm_usage_logs.
            }
        }
    }

    info!(
        agent_id = %agent_id,
        run_id = %run.id,
        status = %derived_status,
        "sleep batch finalized"
    );

    Ok(())
}

/// Ensures the run has a complete 2-file snapshot set, reads it back, and
/// publishes the candidate bundle atomically.
///
/// Returns an error when publication cannot complete (manual-edit conflict,
/// I/O failure, or an incomplete snapshot set). The caller leaves the run
/// `running` on error so startup recovery can retry and the candidate is not
/// lost; the current on-disk files are never overwritten on a conflict.
async fn publish_memory_bundle(
    state: &AppState,
    db: &Arc<Database>,
    ctx: &BatchContext,
    agent_id: &str,
) -> Result<(), SleepBatchError> {
    let run_id = ctx.run_id.clone();
    let agent_id_owned = agent_id.to_string();
    let base = ctx.base_memory.clone();
    call_blocking(Arc::clone(db), move |db| {
        db.ensure_memory_snapshots_complete(
            &run_id,
            &agent_id_owned,
            &[
                (MemoryFile::Episodic, base.episodic.as_str()),
                (MemoryFile::Semantic, base.semantic.as_str()),
            ],
        )
    })
    .await?;

    let run_id = ctx.run_id.clone();
    let snapshots =
        call_blocking(Arc::clone(db), move |db| db.get_snapshots_for_run(&run_id)).await?;

    let Some((before, after)) = build_bundles_from_snapshots(&snapshots) else {
        crate::runtime::metrics::inc_memory_snapshot_incomplete();
        return Err(SleepBatchError::Internal(format!(
            "incomplete memory snapshot set for run {}",
            ctx.run_id
        )));
    };

    // Publication performs several fsyncs; run it on a blocking thread so the
    // async sleep batch never stalls the runtime on disk latency.
    let memory_loader = Arc::clone(&state.memory_loader);
    let agent_id_owned = agent_id.to_string();
    let run_id_owned = ctx.run_id.clone();
    let publish_result = tokio::task::spawn_blocking(move || {
        memory_loader.publish_bundle(&agent_id_owned, &run_id_owned, &before, &after)
    })
    .await
    .map_err(|e| SleepBatchError::Io(format!("publication task failed: {e}")))?;
    publish_result.map_err(|e| match e {
        MemoryError::Conflict { agent_id, file } => SleepBatchError::Io(format!(
            "memory publication conflict: agent={agent_id} file={file}"
        )),
        MemoryError::UnsafeAgentId(a) => SleepBatchError::UnsafeAgentId(a),
        MemoryError::Io { path, source } => {
            SleepBatchError::Io(format!("memory io failed at {}: {source}", path.display()))
        }
        MemoryError::RecoveryValidation { .. } => SleepBatchError::Internal(e.to_string()),
    })?;
    Ok(())
}

/// Builds `(before, after)` bundles from a run's snapshots.
///
/// Returns `None` unless exactly one snapshot exists for each of the two
/// memory files.
fn build_bundles_from_snapshots(
    snapshots: &[MemorySnapshot],
) -> Option<(MemoryBundle, MemoryBundle)> {
    let mut before = MemoryBundle::default();
    let mut after = MemoryBundle::default();
    let mut seen_episodic = false;
    let mut seen_semantic = false;
    for snapshot in snapshots {
        match snapshot.file {
            MemoryFile::Episodic => {
                before.episodic = snapshot.content_before.clone();
                after.episodic = snapshot.content_after.clone();
                seen_episodic = true;
            }
            MemoryFile::Semantic => {
                before.semantic = snapshot.content_before.clone();
                after.semantic = snapshot.content_after.clone();
                seen_semantic = true;
            }
        }
    }
    // Both files must have a snapshot row; otherwise the bundle is incomplete
    // and publication must not proceed.
    (seen_episodic && seen_semantic).then_some((before, after))
}

/// Recovers memory publication for sleep runs left `running` by a crash.
///
/// For each running run: if its 2-file snapshot set is complete, re-drive the
/// publication from the persisted `after` bundle and transition the run to its
/// derived terminal status. If the snapshot set is incomplete, the run is
/// marked failed. If the on-disk content matches neither `before` nor `after`,
/// startup halts so the operator can intervene.
///
/// Must run before the TurnDispatcher and Channels start so Turns never read a
/// half-published bundle.
///
/// # Errors
///
/// Returns [`EgoPulseError`] only when on-disk content is unclassifiable
/// (startup must halt). Incomplete snapshots and failed publications are
/// logged and marked failed without aborting startup.
pub(crate) fn recover_memory_publication(state: &AppState) -> Result<(), EgoPulseError> {
    let runs = state
        .db
        .list_running_sleep_runs()
        .map_err(EgoPulseError::from)?;
    if runs.is_empty() {
        return Ok(());
    }
    for run in runs {
        recover_run_publication(state, &run)?;
    }
    Ok(())
}

fn recover_run_publication(state: &AppState, run: &SleepRun) -> Result<(), EgoPulseError> {
    let snapshots = state
        .db
        .get_snapshots_for_run(&run.id)
        .map_err(EgoPulseError::from)?;

    match build_bundles_from_snapshots(&snapshots) {
        Some((before, after)) => {
            state
                .memory_loader
                .recover_publication(&run.agent_id, &run.id, &before, &after)
                .map_err(|e| recovery_error(run, e))?;
            // Re-drive the same post-publication finalization the normal path
            // uses (archive / truncate / finalize), so a crash between
            // publication and finalization still converges to the on-success
            // state rather than leaving sessions un-archived. Idempotent: a
            // run the normal path already finalized is a no-op.
            let config = state.config_manager.current_blocking();
            let groups_dir = config.config.groups_dir();
            let secrets = crate::tools::collect_config_secrets(&config.config);
            if let Err(e) = finalize_published_run_blocking(
                &state.db,
                &state.assets,
                &groups_dir,
                &secrets,
                run,
                None,
            ) {
                // ArchivePending is a transient condition (archive I/O
                // temporarily unavailable), not a consistency violation. The
                // run stays `Running` and the next scheduler cycle retries —
                // startup must NOT abort over a retryable archive failure.
                if matches!(e, SleepBatchError::ArchivePending { .. }) {
                    tracing::warn!(
                        run_id = %run.id,
                        agent_id = %run.agent_id,
                        error = %e,
                        "archive pending after memory recovery; run left Running for retry"
                    );
                } else {
                    return Err(EgoPulseError::Internal(format!(
                        "finalize after memory recovery failed for run {}: {e}",
                        run.id
                    )));
                }
            }
            tracing::info!(
                run_id = %run.id,
                agent_id = %run.agent_id,
                "recovered memory publication on startup"
            );
        }
        None => {
            crate::runtime::metrics::inc_memory_snapshot_incomplete();
            let run_id = run.id.clone();
            state
                .db
                .update_sleep_run_failed(
                    &run_id,
                    "memory snapshot set incomplete at startup recovery",
                )
                .map_err(EgoPulseError::from)?;
            tracing::warn!(
                run_id = %run.id,
                agent_id = %run.agent_id,
                "memory snapshot set incomplete; run marked failed"
            );
        }
    }
    Ok(())
}

/// Maps a [`MemoryError`] from recovery into the startup-halting error.
///
/// [`MemoryError::RecoveryValidation`] halts startup; other variants are
/// unexpected during recovery and also halt to avoid silent loss.
fn recovery_error(run: &SleepRun, e: MemoryError) -> EgoPulseError {
    match e {
        MemoryError::RecoveryValidation {
            agent_id,
            run_id,
            file,
        } => EgoPulseError::Internal(format!(
            "memory recovery validation failed: agent={agent_id} run={run_id} file={file}"
        )),
        other => EgoPulseError::Internal(format!(
            "memory recovery failed for run {}: {other}",
            run.id
        )),
    }
}

fn archive_and_clear_session(
    db: &Database,
    assets: &crate::assets::AssetStore,
    groups_dir: &Path,
    run_id: &str,
    session: &AgentSessionInfo,
    secrets: &[(String, String)],
) -> Result<(), SleepBatchError> {
    let snapshot = db
        .load_session_snapshot(session.chat_id, 100)
        .map_err(SleepBatchError::Storage)?;

    if let Some(json) = &snapshot.messages_json {
        let context = parse_session_context(assets, json)?;
        let mut messages: Vec<Message> = Vec::new();
        if let Some(checkpoint) = &context.checkpoint {
            messages.push(crate::agent_loop::session_snapshot::checkpoint_message(
                checkpoint,
            ));
        }
        messages.extend(context.messages.iter().cloned());
        if !messages.is_empty() {
            // Deterministic idempotency key: a crash after the archive write but
            // before truncation produces the same file on re-finalization rather
            // than a duplicate. An I/O failure here MUST propagate so the caller
            // leaves the run `Running` instead of truncating an un-archived
            // session.
            let idempotency_key = format!("{run_id}-{}", session.chat_id);
            archive_conversation_blocking(
                groups_dir,
                &session.channel,
                session.chat_id,
                &messages,
                secrets,
                &idempotency_key,
            )
            .map_err(|e| SleepBatchError::ArchivePending {
                agent_id: String::new(),
                run_id: run_id.to_string(),
                reason: format!("archive I/O failed for chat {}: {e}", session.chat_id),
            })?;
        } else {
            info!(
                chat_id = session.chat_id,
                "skipping archive: messages_json parsed as empty"
            );
        }
    }

    // Only truncate after the archive is confirmed on disk, so an archive
    // failure never loses the conversation content.
    if let Some(revision) = snapshot.session_revision {
        let truncated_json = truncate_session_json(
            assets,
            snapshot.messages_json.as_deref(),
            SESSION_CLEAR_KEEP_RECENT,
        )?;
        let truncated = db
            .truncate_session_messages(session.chat_id, revision, &truncated_json)
            .map_err(SleepBatchError::Storage)?;
        if !truncated {
            warn!(
                chat_id = session.chat_id,
                "skipping session truncate: concurrent modification detected"
            );
        }
    }

    Ok(())
}

/// Sleep-batch session truncate: keeps the trailing `keep` user/assistant
/// text messages, dropping any `tool` role and any `assistant` with
/// `tool_calls`, and clears the checkpoint and the usage anchor so the next
/// turn starts from a clean context.
fn truncate_session_json(
    assets: &crate::assets::AssetStore,
    messages_json: Option<&str>,
    keep: usize,
) -> Result<String, SleepBatchError> {
    let context = match messages_json {
        Some(json) => parse_session_context(assets, json)?,
        None => SessionContext::default(),
    };
    let retained: Vec<Message> = context
        .messages
        .iter()
        .filter(|message| message.role != "tool" && message.tool_calls.is_empty())
        .cloned()
        .collect();
    let start = retained.len().saturating_sub(keep);
    let truncated = SessionContext {
        checkpoint: None,
        messages: std::sync::Arc::new(retained[start..].to_vec()),
        last_usage: None,
    };
    let snapshot = crate::agent_loop::session_snapshot::context_to_snapshot(assets, &truncated)
        .map_err(SleepBatchError::Storage)?;
    serde_json::to_string(&snapshot).map_err(|error| {
        SleepBatchError::Storage(crate::error::StorageError::SessionSerialize(error))
    })
}

fn parse_session_context(
    assets: &crate::assets::AssetStore,
    json: &str,
) -> Result<SessionContext, SleepBatchError> {
    let snapshot: crate::agent_loop::session_snapshot::ContextSnapshot = serde_json::from_str(json)
        .map_err(|error| {
            SleepBatchError::Storage(crate::error::StorageError::SessionSerialize(error))
        })?;
    crate::agent_loop::session_snapshot::context_from_snapshot(assets, snapshot)
        .map_err(SleepBatchError::Storage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmProvider, LlmUsage, Message, MessagesResponse, ToolDefinition};

    /// Wraps a legacy message-array JSON body in the snapshot envelope.
    fn session_envelope(messages: &str) -> String {
        format!(r#"{{"version":1,"messages":{messages}}}"#)
    }
    use crate::storage::{Database, EpisodeEventKind, SleepRunStatus};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct MockLlmProvider {
        response: String,
        input_tokens: i64,
        output_tokens: i64,
    }

    impl MockLlmProvider {
        fn new() -> Self {
            Self {
                response: serde_json::json!({
                    "semantic": ""
                })
                .to_string(),
                input_tokens: 0,
                output_tokens: 0,
            }
        }

        fn with_response(response: serde_json::Value) -> Self {
            Self {
                response: response.to_string(),
                input_tokens: 0,
                output_tokens: 0,
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MockLlmProvider {
        fn provider_name(&self) -> &str {
            "mock"
        }
        fn model_name(&self) -> &str {
            "mock-model"
        }
        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<Arc<Vec<ToolDefinition>>>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            Ok(MessagesResponse {
                content: self.response.clone(),
                reasoning_content: None,
                tool_calls: vec![],
                usage: if self.input_tokens > 0 || self.output_tokens > 0 {
                    Some(LlmUsage {
                        input_tokens: self.input_tokens,
                        output_tokens: self.output_tokens,
                        cached_input_tokens: None,
                    })
                } else {
                    None
                },
            })
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn store_msg(db: &Database, id: &str, chat_id: i64, content: &str, ts: &str) {
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE chat_id = ?2))",
            rusqlite::params![id, chat_id, "alice", content, "user", ts, "message"],
        )
        .expect("store message");
    }

    fn insert_episode_event(db: &Database, agent_id: &str, id: &str, encoded_at: &str) {
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT INTO episode_events
                (id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                 ripple_strength, certainty, sleep_run_id, source_refs_json,
                 created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3, 'insight', ?4, ?5, 3, 'stated', 'backfill', NULL, ?3, ?3)",
            rusqlite::params![id, agent_id, encoded_at, format!("event {id}"), "body"],
        )
        .expect("insert episode event");
    }

    fn create_chat(db: &Database, agent_id: &str, suffix: &str) -> i64 {
        db.resolve_or_create_chat_id("test", &format!("test:chat{suffix}"), "direct", agent_id)
            .expect("create chat")
    }

    fn seed_messages_for_proceed(db: &Database, agent_id: &str) {
        let chat_id = create_chat(db, agent_id, "");
        for i in 1..=(SKIP_THRESHOLD + 1) {
            store_msg(
                db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(
            chat_id,
            &session_envelope(r#"[{"role":"user","content":"hi"}]"#),
        )
        .expect("save session");
    }

    fn seed_sessions_above_threshold(db: &Database, agent_id: &str, n_chats: usize) {
        let needed = (SKIP_THRESHOLD + 1) as usize;
        let per_chat = needed.div_ceil(n_chats);
        for i in 0..n_chats {
            let cid = create_chat(db, agent_id, &format!("-{i}"));
            // Distinct pending ages per chat so ordering is observable.
            let ts = format!("2025-06-01T00:{i:02}:00Z");
            for j in 0..per_chat {
                store_msg(db, &format!("m{i}-{j}"), cid, "hi", &ts);
            }
            db.save_session(
                cid,
                &session_envelope(r#"[{"role":"user","content":"hi"}]"#),
            )
            .expect("save session");
        }
    }

    /// Seeds one chat whose oldest pending message is `(oldest_ts, oldest_id)`.
    ///
    /// A second message `(second_ts, second_id)` is added so that `MIN(timestamp)`
    /// and `MIN(id)` come from different rows (the bug the window-function query
    /// fixes), plus `n - 2` filler messages that never become the oldest tuple.
    /// Returns the created chat id.
    fn seed_oldest_pending_chat(
        db: &Database,
        agent_id: &str,
        suffix: &str,
        oldest: (&str, &str),
        second: (&str, &str),
        n: usize,
    ) -> i64 {
        let cid = create_chat(db, agent_id, suffix);
        store_msg(db, oldest.1, cid, "hi", oldest.0);
        store_msg(db, second.1, cid, "hi", second.0);
        for j in 0..n.saturating_sub(2) {
            store_msg(
                db,
                &format!("zz{j}"),
                cid,
                "hi",
                &format!("2025-06-01T00:12:{j:02}Z"),
            );
        }
        db.save_session(
            cid,
            &session_envelope(r#"[{"role":"user","content":"hi"}]"#),
        )
        .expect("save session");
        cid
    }

    fn build_test_state(db: Database, dir: &std::path::Path) -> AppState {
        build_test_state_with_llm(db, dir, Arc::new(MockLlmProvider::new()))
    }

    fn build_test_state_with_llm(
        db: Database,
        dir: &std::path::Path,
        llm: Arc<dyn LlmProvider>,
    ) -> AppState {
        let config = crate::test_util::test_config(&dir.to_string_lossy());
        crate::test_util::build_state_with_config(config, Some(llm), None, Some(Arc::new(db)), None)
    }

    fn event_response(title: &str) -> String {
        serde_json::json!({
            "events": [{
                "experienced_at": "2025-01-01T00:01:00Z",
                "kind": "decision",
                "title": title,
                "body_md": "decided to test",
                "ripple_strength": 3,
                "certainty": "stated"
            }]
        })
        .to_string()
    }

    fn semantic_response(text: &str) -> String {
        serde_json::json!({ "semantic": text }).to_string()
    }

    fn all_success_responses() -> Vec<String> {
        vec![
            event_response("extracted event"),
            // The rollup parser retries once, so the second event response is
            // the deliberately-invalid first attempt.
            event_response("ignored"),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            serde_json::json!({"semantic": "# Semantic\n\n- fact"}).to_string(),
        ]
    }

    #[tokio::test]
    async fn run_sleep_batch_skips_when_input_below_threshold() {
        // Arrange
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());

        // Act
        let result = run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual).await;

        // Assert
        assert!(result.is_ok());
        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, SleepRunStatus::Skipped);
        assert_eq!(runs[0].trigger, SleepRunTrigger::Manual);
        assert_eq!(
            runs[0].error_message.as_deref(),
            Some("0 new messages; runs when there are more than 64")
        );
    }

    #[tokio::test]
    async fn run_sleep_batch_creates_run_on_proceed() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(SequentialMockProvider::new(all_success_responses()));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, SleepRunStatus::Success);
    }

    #[tokio::test]
    async fn run_sleep_batch_drains_25_sessions_across_two_runs() {
        let (db, dir) = test_db();
        const N_CHATS: usize = 25;
        // 25 * PER_CHAT exceeds SKIP_THRESHOLD; after run 1 drains the first
        // MAX_SOURCE_SESSIONS (20) chats, the remaining 5 * PER_CHAT must stay above
        // SKIP_THRESHOLD so run 2 still proceeds. PER_CHAT = 13 -> 5 * 13 = 65 > 64.
        const PER_CHAT: usize = 13;
        let mut chat_ids = Vec::with_capacity(N_CHATS);
        for i in 0..N_CHATS {
            let cid = create_chat(&db, "test-agent", &format!("-{i}"));
            for j in 0..PER_CHAT {
                let ts = format!("2025-06-01T00:{i:02}:{j:02}Z");
                store_msg(&db, &format!("m{i}-{j}"), cid, "hi", &ts);
            }
            db.save_session(
                cid,
                &session_envelope(r#"[{"role":"user","content":"hi"}]"#),
            )
            .expect("save session");
            chat_ids.push(cid);
        }

        let llm = Arc::new(DrainMockProvider);
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("run 1 should drain the first 20 sessions");

        let hot_cid = chat_ids[0];
        store_msg(&state.db, "hot-1", hot_cid, "hi", "2025-12-01T00:00:00Z");
        let decision =
            collect_sleep_input(&state.db, "test-agent").expect("collect after hot message");
        let sessions = match &decision {
            InputDecision::Proceed { sessions, .. } => sessions,
            other => panic!("expected Proceed, got {other:?}"),
        };
        let ids: Vec<i64> = sessions.iter().map(|s| s.chat_id).collect();
        let unprocessed = &chat_ids[20..25];
        for u in unprocessed {
            assert!(
                ids.contains(u),
                "unprocessed chat {u} must remain pending; collected ids = {ids:?}"
            );
        }
        let hot_pos = ids
            .iter()
            .position(|c| *c == hot_cid)
            .expect("hot chat present");
        for u in unprocessed {
            let u_pos = ids
                .iter()
                .position(|c| *c == *u)
                .expect("unprocessed present");
            assert!(
                u_pos < hot_pos,
                "unprocessed chat {u} must precede hot chat {hot_cid}"
            );
        }

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("run 2 should drain the remaining sessions");

        let third = collect_sleep_input(&state.db, "test-agent").expect("collect run 3");
        assert!(
            matches!(third, InputDecision::Skip { .. }),
            "expected Skip after all 25 sessions drained, got {third:?}"
        );
    }

    #[tokio::test]
    async fn run_sleep_batch_rejects_double_execution() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let llm = Arc::new(MockLlmProvider::new());
        let state = build_test_state_with_llm(db, dir.path(), llm);

        state
            .db
            .create_sleep_run("test-agent", SleepRunTrigger::Manual)
            .expect("create running");

        let err = run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect_err("should reject double execution");
        assert!(
            matches!(err, SleepBatchError::AlreadyRunning { .. }),
            "expected AlreadyRunning, got {err:?}"
        );
    }

    #[tokio::test]
    async fn run_sleep_batch_saves_aggregate_snapshots() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let llm = Arc::new(SequentialMockProvider::new(vec![
            r#"{"events":[]}"#.to_string(),
            r#"{"events":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            serde_json::json!({
                "semantic": "# Semantic\n\n- fact"
            })
            .to_string(),
        ]));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs.len(), 1);

        let snapshots = state
            .db
            .get_snapshots_for_run(&runs[0].id)
            .expect("snapshots");
        assert!(!snapshots.is_empty(), "should have memory snapshots");
    }

    #[tokio::test]
    async fn run_sleep_batch_marks_success_on_completion() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(SequentialMockProvider::new(all_success_responses()));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Success);
    }

    #[tokio::test]
    async fn run_sleep_batch_completes_with_streaming_only_provider() {
        // Arrange
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        // The provider implements only the streaming method, so every batch
        // step relies on `send_message` collecting a streamed body.
        let llm = Arc::new(StreamingOnlyProvider {
            inner: Arc::new(SequentialMockProvider::new(all_success_responses())),
        });
        let state = build_test_state_with_llm(db, dir.path(), llm);

        // Act
        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        // Assert
        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Success);
    }

    #[tokio::test]
    async fn run_sleep_batch_persists_both_memory_files() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(SequentialMockProvider::new(all_success_responses()));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        assert!(
            memory_dir.join("episodic.md").exists(),
            "episodic.md missing"
        );
        assert!(
            memory_dir.join("semantic.md").exists(),
            "semantic.md missing"
        );
    }

    #[tokio::test]
    async fn run_sleep_batch_creates_complete_snapshot_set_for_every_file() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        // Extraction yields no events, so semantic is skipped and neither file
        // changes — yet each must still have a snapshot.
        let llm = Arc::new(SequentialMockProvider::new(vec![
            r#"{"events":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
        ]));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        let snapshots = state
            .db
            .get_snapshots_for_run(&runs[0].id)
            .expect("snapshots");
        assert_eq!(
            snapshots.len(),
            2,
            "both memory files must have a snapshot, even unchanged ones"
        );
        // Semantic did not change; its snapshot must still exist with before == after.
        let semantic = snapshots
            .iter()
            .find(|snapshot| snapshot.file == MemoryFile::Semantic)
            .expect("semantic snapshot");
        assert_eq!(semantic.content_before, semantic.content_after);
    }

    #[tokio::test]
    async fn run_sleep_batch_detects_manual_edit_conflict() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        // Seed base memory so the run starts from a non-empty bundle.
        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        std::fs::create_dir_all(&memory_dir).expect("create memory dir");
        std::fs::write(memory_dir.join("episodic.md"), "base ep").expect("write base");

        let llm = Arc::new(SequentialMockProvider::new(all_success_responses()));
        // Simulate a manual edit to episodic.md after the run captured base but
        // before publication. The run loads base at start, so the edit happens
        // after that snapshot but before finalize.
        let provider = Arc::new(ConflictEditProvider {
            inner: llm,
            edited: std::sync::Mutex::new(false),
            memory_dir: memory_dir.clone(),
        });
        let state = build_test_state_with_llm(db, dir.path(), provider);

        // A manual-edit conflict cannot publish: the run stays `running` so
        // startup recovery can retry and the candidate is not lost, while the
        // manual edit is preserved on disk.
        let _ = run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual).await;

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(
            runs[0].status,
            SleepRunStatus::Running,
            "manual edit conflict must leave the run running for recovery"
        );
        // The manual edit is preserved (publication did not overwrite it).
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("episodic.md")).unwrap(),
            "manually edited"
        );
    }

    #[test]
    fn recover_memory_publication_converges_after_partial_rename() {
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());

        let before = MemoryBundle {
            episodic: "old ep".to_string(),
            semantic: "old sem".to_string(),
        };
        let after = MemoryBundle {
            episodic: "new ep".to_string(),
            semantic: "new sem".to_string(),
        };

        // Create a running sleep run with a complete snapshot set.
        let run_id = state
            .db
            .try_create_sleep_run("test-agent", SleepRunTrigger::Manual)
            .expect("create")
            .expect("run id");
        mark_all_steps_success(&state.db, &run_id);
        seed_complete_snapshots(&state.db, &run_id, "test-agent", &before, &after);

        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();

        // Crash point 1: only episodic renamed to `after`.
        std::fs::write(memory_dir.join("episodic.md"), "new ep").unwrap();
        std::fs::write(memory_dir.join("semantic.md"), "old sem").unwrap();
        recover_memory_publication(&state).expect("recover 1");
        assert_bundle(&memory_dir, &after);
        assert_eq!(
            state.db.get_sleep_run(&run_id).unwrap().unwrap().status,
            SleepRunStatus::Success
        );
    }

    #[test]
    fn recover_memory_publication_converges_after_all_renamed_but_not_finalized() {
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());

        let before = MemoryBundle {
            episodic: "old ep".to_string(),
            semantic: "old sem".to_string(),
        };
        let after = MemoryBundle {
            episodic: "new ep".to_string(),
            semantic: "new sem".to_string(),
        };

        let run_id = state
            .db
            .try_create_sleep_run("test-agent", SleepRunTrigger::Manual)
            .expect("create")
            .expect("run id");
        mark_all_steps_success(&state.db, &run_id);
        seed_complete_snapshots(&state.db, &run_id, "test-agent", &before, &after);

        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        // All files already at `after`, but the run is still `running`.
        std::fs::write(memory_dir.join("episodic.md"), "new ep").unwrap();
        std::fs::write(memory_dir.join("semantic.md"), "new sem").unwrap();

        recover_memory_publication(&state).expect("recover");
        assert_bundle(&memory_dir, &after);
        assert_eq!(
            state.db.get_sleep_run(&run_id).unwrap().unwrap().status,
            SleepRunStatus::Success
        );
    }

    #[test]
    fn recover_memory_publication_converges_after_one_file_renamed() {
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());

        let before = MemoryBundle {
            episodic: "old ep".to_string(),
            semantic: "old sem".to_string(),
        };
        let after = MemoryBundle {
            episodic: "new ep".to_string(),
            semantic: "new sem".to_string(),
        };

        let run_id = state
            .db
            .try_create_sleep_run("test-agent", SleepRunTrigger::Manual)
            .expect("create")
            .expect("run id");
        mark_all_steps_success(&state.db, &run_id);
        seed_complete_snapshots(&state.db, &run_id, "test-agent", &before, &after);

        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        // Crash point: episodic was renamed to `after`, semantic is still `before`.
        std::fs::write(memory_dir.join("episodic.md"), "new ep").unwrap();
        std::fs::write(memory_dir.join("semantic.md"), "old sem").unwrap();

        recover_memory_publication(&state).expect("recover");
        assert_bundle(&memory_dir, &after);
        assert_eq!(
            state.db.get_sleep_run(&run_id).unwrap().unwrap().status,
            SleepRunStatus::Success
        );
    }

    #[test]
    fn recover_memory_publication_halts_on_unclassifiable_content() {
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());

        let before = MemoryBundle {
            episodic: "old ep".to_string(),
            semantic: "old sem".to_string(),
        };
        let after = MemoryBundle {
            episodic: "new ep".to_string(),
            semantic: "new sem".to_string(),
        };

        let run_id = state
            .db
            .try_create_sleep_run("test-agent", SleepRunTrigger::Manual)
            .expect("create")
            .expect("run id");
        seed_complete_snapshots(&state.db, &run_id, "test-agent", &before, &after);

        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        // A third-party edit that matches neither before nor after.
        std::fs::write(memory_dir.join("episodic.md"), "mystery").unwrap();
        std::fs::write(memory_dir.join("semantic.md"), "old sem").unwrap();

        let err = recover_memory_publication(&state).expect_err("should halt");
        assert!(
            err.to_string()
                .contains("memory recovery validation failed")
        );
    }

    #[test]
    fn recover_memory_publication_marks_failed_when_snapshots_incomplete() {
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());

        let run_id = state
            .db
            .try_create_sleep_run("test-agent", SleepRunTrigger::Manual)
            .expect("create")
            .expect("run id");
        // Only one snapshot exists → incomplete.
        seed_one_snapshot(
            &state.db,
            &run_id,
            "test-agent",
            MemoryFile::Episodic,
            "a",
            "b",
        );

        recover_memory_publication(&state).expect("does not halt");
        assert_eq!(
            state.db.get_sleep_run(&run_id).unwrap().unwrap().status,
            SleepRunStatus::Failed
        );
    }

    #[test]
    fn finalize_archives_session_after_event_extraction_alone() {
        // Major 4: a crash between memory publication and session archiving
        // must still converge to the normal-path state. Recovery drives the
        // same archive/truncate/finalize, reconstructing targets from the
        // source_chats_json persisted before publication.
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());
        let agent = "test-agent";

        // A source session with 8 messages, all settled past both checkpoints.
        let chat_id = create_chat(&state.db, agent, "-arch");
        for i in 1..=8 {
            store_msg(
                &state.db,
                &format!("a-{i}"),
                chat_id,
                &format!("msg{i}"),
                &format!("2025-06-01T00:00:{i:02}Z"),
            );
        }
        let messages_json = (1..=8)
            .map(|i| format!(r#"{{"role":"user","content":"msg{i}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        state
            .db
            .save_session(chat_id, &session_envelope(&format!("[{messages_json}]")))
            .expect("save session");

        // Checkpoints at the last message so the session is archivable.
        let conn = state.db.get_conn().expect("pool");
        conn.execute(
            "INSERT INTO sleep_step_checkpoints
             (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
             VALUES (?1, 'event_extraction', 'messages', ?2, ?3, ?4, ?5)",
            rusqlite::params![
                agent,
                chat_id.to_string(),
                "2025-06-01T00:00:08Z",
                "a-8",
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .expect("seed checkpoint");
        drop(conn);

        // Run with complete snapshots + the archive plan persisted pre-crash.
        let before = MemoryBundle {
            episodic: "old ep".to_string(),
            semantic: "old sem".to_string(),
        };
        let after = MemoryBundle {
            episodic: "new ep".to_string(),
            semantic: "new sem".to_string(),
        };
        let run_id = state
            .db
            .try_create_sleep_run(agent, SleepRunTrigger::Manual)
            .expect("create")
            .expect("run id");
        mark_all_steps_success(&state.db, &run_id);
        seed_complete_snapshots(&state.db, &run_id, agent, &before, &after);
        let plan = serde_json::to_string(&[AgentSessionInfo {
            chat_id,
            channel: "test".to_string(),
            external_chat_id: "test:chat-arch".to_string(),
            updated_at: "2025-06-01T00:00:00Z".to_string(),
            message_count: 8,
            estimated_tokens: 0,
        }])
        .expect("serialize plan");
        state
            .db
            .update_sleep_run_source_chats(&run_id, &plan)
            .expect("persist plan");

        // Memory files already at `after` (publication completed pre-crash).
        let memory_dir = dir.path().join("agents").join(agent).join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("episodic.md"), "new ep").unwrap();
        std::fs::write(memory_dir.join("semantic.md"), "new sem").unwrap();

        recover_memory_publication(&state).expect("recover");

        // Run finalized...
        assert_eq!(
            state.db.get_sleep_run(&run_id).unwrap().unwrap().status,
            SleepRunStatus::Success
        );
        // ...and the session was archived + truncated (converges with the
        // normal path), not left dangling as before.
        let snap = state
            .db
            .load_session_snapshot(chat_id, 100)
            .expect("snapshot");
        let kept =
            parse_session_context(&state.assets, snap.messages_json.as_deref().unwrap_or("{}"))
                .expect("snapshot")
                .messages
                .len();
        assert!(
            kept <= SESSION_CLEAR_KEEP_RECENT,
            "session should be truncated to <= {} messages, got {}",
            SESSION_CLEAR_KEEP_RECENT,
            kept
        );
        let archive_dir = state
            .config
            .groups_dir()
            .join("test")
            .join(chat_id.to_string())
            .join("conversations");
        let archived = std::fs::read_dir(&archive_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert!(archived >= 1, "archive file should exist");
    }

    fn mark_all_steps_success(db: &Database, run_id: &str) {
        let conn = db.get_conn().expect("pool");
        let now = chrono::Utc::now().to_rfc3339();
        for step in ["event_extraction", "episodic_update", "semantic_update"] {
            conn.execute(
                "UPDATE sleep_run_steps
                 SET status = 'success', started_at = ?1, finished_at = ?1
                 WHERE sleep_run_id = ?2 AND step_name = ?3",
                rusqlite::params![now, run_id, step],
            )
            .expect("mark step success");
        }
    }

    fn seed_complete_snapshots(
        db: &Database,
        run_id: &str,
        agent_id: &str,
        before: &MemoryBundle,
        after: &MemoryBundle,
    ) {
        for (file, b, a) in [
            (MemoryFile::Episodic, &before.episodic, &after.episodic),
            (MemoryFile::Semantic, &before.semantic, &after.semantic),
        ] {
            seed_one_snapshot(db, run_id, agent_id, file, b, a);
        }
    }

    fn seed_one_snapshot(
        db: &Database,
        run_id: &str,
        agent_id: &str,
        file: MemoryFile,
        before: &str,
        after: &str,
    ) {
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT INTO memory_snapshots (id, run_id, agent_id, file, content_before, content_after, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                run_id,
                agent_id,
                file.to_string(),
                before,
                after,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .expect("insert snapshot");
    }

    fn assert_bundle(memory_dir: &std::path::Path, expected: &MemoryBundle) {
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("episodic.md")).unwrap(),
            expected.episodic
        );
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("semantic.md")).unwrap(),
            expected.semantic
        );
    }

    /// Provider that edits `episodic.md` on-disk right before the memory update
    /// step runs, simulating a concurrent manual edit between base capture and
    /// publication.
    struct ConflictEditProvider {
        inner: Arc<dyn LlmProvider>,
        edited: std::sync::Mutex<bool>,
        memory_dir: std::path::PathBuf,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ConflictEditProvider {
        fn provider_name(&self) -> &str {
            self.inner.provider_name()
        }
        fn model_name(&self) -> &str {
            self.inner.model_name()
        }
        async fn send_message(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<ToolDefinition>>>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let is_memory_step = system.contains("意味記憶（semantic.md）を更新する");
            if is_memory_step {
                let mut edited = self.edited.lock().unwrap();
                if !*edited {
                    *edited = true;
                    std::fs::write(self.memory_dir.join("episodic.md"), "manually edited")
                        .expect("manual edit");
                }
            }
            self.inner.send_message(system, messages, tools).await
        }
        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    #[tokio::test]
    async fn run_sleep_batch_marks_failed_on_error() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let llm = Arc::new(MockLlmProvider::with_response(
            serde_json::json!({"bad": true}),
        ));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch completes even with step failures");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Failed);
    }

    #[tokio::test]
    async fn run_sleep_batch_handles_missing_memory_files() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(MockLlmProvider::new());
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch should succeed even without memory files");
    }

    #[tokio::test]
    async fn run_sleep_batch_handles_no_memory_dir() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(MockLlmProvider::new());
        let state = build_test_state_with_llm(db, dir.path(), llm);

        // Delete the agents dir entirely
        let _ = std::fs::remove_dir_all(dir.path().join("agents"));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch should succeed");
    }

    #[tokio::test]
    async fn run_sleep_batch_uses_default_agent_when_none() {
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());
        let result = run_sleep_batch(&state, None, SleepRunTrigger::Manual).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn scheduled_run_records_trigger_type() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(SequentialMockProvider::new(all_success_responses()));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Scheduled)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs[0].trigger, SleepRunTrigger::Scheduled);
        assert_eq!(runs[0].status, SleepRunStatus::Success);
    }

    struct SequentialMockProvider {
        responses: std::sync::Mutex<Vec<String>>,
    }

    impl SequentialMockProvider {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for SequentialMockProvider {
        fn provider_name(&self) -> &str {
            "sequential-mock"
        }
        fn model_name(&self) -> &str {
            "sequential-mock-model"
        }
        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<Arc<Vec<ToolDefinition>>>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            // Pop the first response, fall back to empty if exhausted
            let mut responses = self.responses.lock().unwrap();
            let response = if responses.is_empty() {
                String::new()
            } else {
                responses.remove(0)
            };
            Ok(MessagesResponse {
                content: response,
                reasoning_content: None,
                tool_calls: vec![],
                usage: None,
            })
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    /// Provider that implements only the streaming path, so batch callers
    /// exercise the trait's default `send_message`.
    struct StreamingOnlyProvider {
        inner: Arc<dyn LlmProvider>,
    }

    #[async_trait]
    impl LlmProvider for StreamingOnlyProvider {
        fn provider_name(&self) -> &str {
            self.inner.provider_name()
        }
        fn model_name(&self) -> &str {
            self.inner.model_name()
        }
        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            self.inner
                .send_message_streaming(system, messages, tools, on_delta)
                .await
        }
    }

    /// Provider for the multi-run drain test: returns an event-extraction-shaped
    /// response for event/rollup calls and a one-key semantic response for the
    /// semantic update step. The semantic step is identified by its prompt's
    /// unique wording.
    struct DrainMockProvider;

    #[async_trait]
    impl LlmProvider for DrainMockProvider {
        fn provider_name(&self) -> &str {
            "drain-mock"
        }
        fn model_name(&self) -> &str {
            "drain-mock-model"
        }
        async fn send_message(
            &self,
            system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<Arc<Vec<ToolDefinition>>>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let content = if system.contains("意味記憶（semantic.md）を更新する") {
                serde_json::json!({
                    "semantic": "# Semantic\n\n- fact"
                })
                .to_string()
            } else if system.to_lowercase().contains("rollup") || system.contains("ロールアップ")
            {
                r#"{"rollups":[]}"#.to_string()
            } else {
                r#"{"events":[]}"#.to_string()
            };
            Ok(MessagesResponse {
                content,
                reasoning_content: None,
                tool_calls: vec![],
                usage: None,
            })
        }
        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    #[tokio::test]
    async fn run_sleep_batch_retries_on_invalid_json_then_succeeds() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let good_response = serde_json::json!({
            "semantic": "retried"
        })
        .to_string();
        let provider = SequentialMockProvider::new(vec![
            event_response("retry event"),
            r#"{"not":"valid rollups"}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"not":"valid semantic"}"#.to_string(),
            good_response,
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("should succeed after retry");
    }

    #[tokio::test]
    async fn run_sleep_batch_fails_when_retry_also_invalid() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let bad1 = r#"{"bad":1}"#.to_string();
        let bad2 = r#"not json at all"#.to_string();
        let provider = SequentialMockProvider::new(vec![
            bad1.clone(),
            bad1,
            bad2.clone(),
            bad2.clone(),
            bad2.clone(),
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch completes even with step failures");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, SleepRunStatus::Failed);
    }

    #[tokio::test]
    async fn run_sleep_batch_logs_llm_usage_with_sleep_batch_request_kind() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let llm = Arc::new(SequentialMockWithUsage::new(vec![
            (event_response("usage event"), 50, 30),
            (r#"{"not":"valid rollups"}"#.to_string(), 10, 10),
            (r#"{"rollups":[]}"#.to_string(), 10, 10),
            (r#"{"rollups":[]}"#.to_string(), 10, 10),
            (semantic_response("# Semantic"), 100, 200),
        ]));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert!(runs[0].input_tokens > 0 || runs[0].output_tokens > 0);
    }

    struct SequentialMockWithUsage {
        responses: std::sync::Mutex<Vec<(String, i64, i64)>>,
    }

    impl SequentialMockWithUsage {
        fn new(responses: Vec<(String, i64, i64)>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for SequentialMockWithUsage {
        fn provider_name(&self) -> &str {
            "seq-usage-mock"
        }
        fn model_name(&self) -> &str {
            "seq-usage-model"
        }
        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<Arc<Vec<ToolDefinition>>>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let mut responses = self.responses.lock().unwrap();
            let (content, input_tokens, output_tokens) = if responses.is_empty() {
                (String::new(), 0_i64, 0_i64)
            } else {
                responses.remove(0)
            };
            Ok(MessagesResponse {
                content,
                reasoning_content: None,
                tool_calls: vec![],
                usage: Some(LlmUsage {
                    input_tokens,
                    output_tokens,
                    cached_input_tokens: None,
                }),
            })
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    #[tokio::test]
    async fn run_sleep_batch_extracts_events_before_semantic_update() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let events_response = serde_json::json!({
            "events": [{
                "experienced_at": "2025-01-01T00:01:00Z",
                "kind": "decision",
                "title": "test event",
                "body_md": "decided to test",
                "ripple_strength": 3,
                "certainty": "stated"
            }]
        })
        .to_string();

        let provider = SequentialMockWithUsage::new(vec![
            (events_response.clone(), 50, 50),
            (events_response, 50, 50),
            (r#"{"rollups":[]}"#.to_string(), 50, 50),
            (r#"{"rollups":[]}"#.to_string(), 50, 50),
            (r#"{"semantic":""}"#.to_string(), 50, 50),
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Success);

        let events = state
            .db
            .list_episode_events_by_run(&runs[0].id)
            .expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "test event");
        assert_eq!(events[0].kind, EpisodeEventKind::Decision);
        assert_eq!(events[0].agent_id, "test-agent");
    }

    #[tokio::test]
    async fn run_sleep_batch_saves_extracted_events_to_db() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let events_response = serde_json::json!({
            "events": [
                {
                    "experienced_at": "2025-01-01T00:01:00Z",
                    "kind": "insight",
                    "title": "learned rust",
                    "body_md": "discovered ownership model",
                    "ripple_strength": 4,
                    "certainty": "stated",
                    "source_message_ids": ["m-2"]
                },
                {
                    "experienced_at": "2025-01-01T00:02:00Z",
                    "kind": "anomaly",
                    "title": "unexpected error",
                    "body_md": "crash on startup",
                    "ripple_strength": 5,
                    "certainty": "derived",
                    "source_message_ids": ["m-3"]
                }
            ]
        })
        .to_string();
        let memory_response = serde_json::json!({
            "semantic": ""
        })
        .to_string();
        let rollup_response = r#"{"rollups":[]}"#.to_string();

        let provider = SequentialMockWithUsage::new(vec![
            (events_response, 100, 100),
            (rollup_response.clone(), 50, 50),
            (rollup_response, 50, 50),
            (memory_response, 100, 100),
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        let events = state
            .db
            .list_episode_events_by_run(&runs[0].id)
            .expect("events");
        assert_eq!(events.len(), 2);
        let titles: Vec<&str> = events.iter().map(|e| e.title.as_str()).collect();
        assert!(titles.contains(&"learned rust"));
        assert!(titles.contains(&"unexpected error"));
        let kinds: Vec<EpisodeEventKind> = events.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EpisodeEventKind::Insight));
        assert!(kinds.contains(&EpisodeEventKind::Anomaly));
    }

    #[tokio::test]
    async fn run_sleep_batch_extract_call_failure_continues() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let memory_response = serde_json::json!({
            "semantic": "updated"
        })
        .to_string();

        let provider = SequentialMockProvider::new(vec![
            r#"{"not_events":[]}"#.to_string(),
            r#"{"not_events":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            memory_response,
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch should continue despite extract failure");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::PartialFailure);

        let events = state
            .db
            .list_episode_events_by_run(&runs[0].id)
            .expect("events");
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn sleep_batch_continues_after_event_extraction_failure() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let provider = SequentialMockProvider::new(vec![
            r#"not json"#.to_string(),
            r#"not json"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"semantic":"updated"}"#.to_string(),
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        let steps = state.db.list_sleep_run_steps(&runs[0].id).expect("steps");

        let episodic = steps
            .iter()
            .find(|s| s.step_name == SleepStepName::EpisodicUpdate)
            .expect("episodic step exists");
        let semantic = steps
            .iter()
            .find(|s| s.step_name == SleepStepName::SemanticUpdate)
            .expect("semantic step exists");

        assert_eq!(episodic.status, SleepStepStatus::Success);
        assert_eq!(semantic.status, SleepStepStatus::Skipped);
    }

    #[tokio::test]
    async fn run_sleep_batch_extract_call_tokens_logged() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let events_response = serde_json::json!({
            "events": [{
                "experienced_at": "2025-01-01T00:01:00Z",
                "kind": "decision",
                "title": "t",
                "body_md": "b",
                "ripple_strength": 3,
                "certainty": "stated"
            }]
        })
        .to_string();
        let rollup_response = r#"{"rollups":[]}"#.to_string();
        let memory_response = serde_json::json!({
            "semantic": "s"
        })
        .to_string();

        let provider = SequentialMockWithUsage::new(vec![
            (events_response, 50, 30),
            (rollup_response.clone(), 40, 20),
            (rollup_response, 40, 20),
            (memory_response, 60, 40),
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert!(runs[0].input_tokens > 0);
        assert!(runs[0].output_tokens > 0);
    }

    /// Provider that dispatches on the system prompt and records semantic
    /// prompts. Semantic responses are consumed in order, so retry and
    /// multi-batch scenarios stay explicit.
    struct ScenarioProvider {
        events_response: String,
        rollup_response: String,
        semantic_responses: std::sync::Mutex<Vec<String>>,
        semantic_prompts: std::sync::Mutex<Vec<String>>,
        semantic_calls: std::sync::atomic::AtomicUsize,
    }

    impl ScenarioProvider {
        fn new(events_response: String, semantic_responses: Vec<String>) -> Self {
            Self {
                events_response,
                rollup_response: r#"{"rollups":[]}"#.to_string(),
                semantic_responses: std::sync::Mutex::new(semantic_responses),
                semantic_prompts: std::sync::Mutex::new(Vec::new()),
                semantic_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn semantic_prompts(&self) -> Vec<String> {
            self.semantic_prompts.lock().unwrap().clone()
        }

        fn semantic_calls(&self) -> usize {
            self.semantic_calls
                .load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LlmProvider for ScenarioProvider {
        fn provider_name(&self) -> &str {
            "scenario-mock"
        }

        fn model_name(&self) -> &str {
            "scenario-mock-model"
        }

        async fn send_message(
            &self,
            system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<Arc<Vec<ToolDefinition>>>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let content = if system.contains("意味記憶（semantic.md）を更新する") {
                self.semantic_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.semantic_prompts
                    .lock()
                    .unwrap()
                    .push(system.to_string());
                let mut responses = self.semantic_responses.lock().unwrap();
                if responses.is_empty() {
                    String::new()
                } else {
                    responses.remove(0)
                }
            } else if system.to_lowercase().contains("rollup") || system.contains("ロールアップ")
            {
                self.rollup_response.clone()
            } else {
                self.events_response.clone()
            };
            Ok(MessagesResponse {
                content,
                reasoning_content: None,
                tool_calls: vec![],
                usage: None,
            })
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    #[tokio::test]
    async fn semantic_update_reads_events_not_messages() {
        // Arrange
        let (db, dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "-semantic-input");
        for i in 1..=(SKIP_THRESHOLD + 1) {
            store_msg(
                &db,
                &format!("raw-{i}"),
                chat_id,
                "RAW-MESSAGE-ONLY-TOKEN",
                &format!("2025-01-01T00:00:{:02}Z", i % 60),
            );
        }
        db.save_session(
            chat_id,
            &session_envelope(r#"[{"role":"user","content":"hi"}]"#),
        )
        .expect("save session");
        let provider = Arc::new(ScenarioProvider::new(
            event_response("Extracted Event Title"),
            vec![semantic_response("# Semantic")],
        ));
        let state = build_test_state_with_llm(db, dir.path(), provider.clone());

        // Act
        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        // Assert
        let prompts = provider.semantic_prompts();
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert!(prompts[0].contains("Extracted Event Title"));
        assert!(!prompts[0].contains("RAW-MESSAGE-ONLY-TOKEN"));
    }

    #[tokio::test]
    async fn semantic_update_skips_without_new_events_and_calls_no_llm() {
        // Arrange
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let provider = Arc::new(ScenarioProvider::new(
            r#"{"events":[]}"#.to_string(),
            vec![semantic_response("# Semantic")],
        ));
        let state = build_test_state_with_llm(db, dir.path(), provider.clone());

        // Act
        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        // Assert
        assert_eq!(provider.semantic_calls(), 0);
        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        let steps = state.db.list_sleep_run_steps(&runs[0].id).expect("steps");
        let semantic = steps
            .iter()
            .find(|step| step.step_name == SleepStepName::SemanticUpdate)
            .expect("semantic step");
        assert_eq!(semantic.status, SleepStepStatus::Skipped);
        let checkpoint = state
            .db
            .get_sleep_checkpoint(
                "test-agent",
                SleepStepName::SemanticUpdate,
                CheckpointSourceKind::EpisodeEvents,
                "test-agent",
            )
            .expect("checkpoint");
        assert!(checkpoint.is_none());
    }

    #[tokio::test]
    async fn semantic_update_advances_event_checkpoint_to_last_event() {
        // Arrange
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let events = serde_json::json!({
            "events": [
                {
                    "experienced_at": "2025-01-01T00:01:00Z",
                    "kind": "decision",
                    "title": "first",
                    "body_md": "body-1",
                    "ripple_strength": 3,
                    "certainty": "stated"
                },
                {
                    "experienced_at": "2025-01-01T00:02:00Z",
                    "kind": "insight",
                    "title": "second",
                    "body_md": "body-2",
                    "ripple_strength": 4,
                    "certainty": "derived"
                }
            ]
        })
        .to_string();
        let provider = Arc::new(ScenarioProvider::new(
            events,
            vec![semantic_response("# Semantic")],
        ));
        let state = build_test_state_with_llm(db, dir.path(), provider.clone());

        // Act
        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        // Assert
        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        let mut stored_events = state
            .db
            .list_episode_events_by_run(&runs[0].id)
            .expect("events");
        stored_events.sort_by(|a, b| {
            (a.encoded_at.clone(), a.id.clone()).cmp(&(b.encoded_at.clone(), b.id.clone()))
        });
        let last = stored_events.last().expect("last event");
        let checkpoint = state
            .db
            .get_sleep_checkpoint(
                "test-agent",
                SleepStepName::SemanticUpdate,
                CheckpointSourceKind::EpisodeEvents,
                "test-agent",
            )
            .expect("checkpoint")
            .expect("checkpoint exists");
        assert_eq!(checkpoint.cursor_at, last.encoded_at);
        assert_eq!(checkpoint.cursor_id, last.id);
    }

    #[tokio::test]
    async fn semantic_update_failure_keeps_checkpoint_and_base_semantic() {
        // Arrange
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        std::fs::create_dir_all(&memory_dir).expect("memory dir");
        std::fs::write(memory_dir.join("semantic.md"), "BASE SEMANTIC").expect("base semantic");
        let provider = Arc::new(ScenarioProvider::new(
            event_response("failure event"),
            vec!["not json".to_string(), "not json".to_string()],
        ));
        let state = build_test_state_with_llm(db, dir.path(), provider.clone());

        // Act
        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        // Assert
        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        let steps = state.db.list_sleep_run_steps(&runs[0].id).expect("steps");
        let semantic = steps
            .iter()
            .find(|step| step.step_name == SleepStepName::SemanticUpdate)
            .expect("semantic step");
        assert_eq!(semantic.status, SleepStepStatus::Failed);
        let checkpoint = state
            .db
            .get_sleep_checkpoint(
                "test-agent",
                SleepStepName::SemanticUpdate,
                CheckpointSourceKind::EpisodeEvents,
                "test-agent",
            )
            .expect("checkpoint");
        assert!(checkpoint.is_none());
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("semantic.md")).expect("semantic"),
            "BASE SEMANTIC"
        );
    }

    #[tokio::test]
    async fn semantic_update_carries_memory_across_batches() {
        // Arrange
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let events = serde_json::json!({
            "events": [
                {
                    "experienced_at": "2025-01-01T00:01:00Z",
                    "kind": "decision",
                    "title": "batch event 1",
                    "body_md": "x".repeat(10_000),
                    "ripple_strength": 3,
                    "certainty": "stated"
                },
                {
                    "experienced_at": "2025-01-01T00:02:00Z",
                    "kind": "decision",
                    "title": "batch event 2",
                    "body_md": "y".repeat(10_000),
                    "ripple_strength": 3,
                    "certainty": "stated"
                },
                {
                    "experienced_at": "2025-01-01T00:03:00Z",
                    "kind": "decision",
                    "title": "batch event 3",
                    "body_md": "z".repeat(10_000),
                    "ripple_strength": 3,
                    "certainty": "stated"
                },
                {
                    "experienced_at": "2025-01-01T00:04:00Z",
                    "kind": "decision",
                    "title": "batch event 4",
                    "body_md": "w".repeat(10_000),
                    "ripple_strength": 3,
                    "certainty": "stated"
                },
                {
                    "experienced_at": "2025-01-01T00:05:00Z",
                    "kind": "decision",
                    "title": "batch event 5",
                    "body_md": "v".repeat(10_000),
                    "ripple_strength": 3,
                    "certainty": "stated"
                }
            ]
        })
        .to_string();
        let provider = Arc::new(ScenarioProvider::new(
            events,
            vec![
                semantic_response("# Semantic batch 1"),
                semantic_response("# Semantic batch 2"),
            ],
        ));
        let state = build_test_state_with_llm(db, dir.path(), provider.clone());

        // Act
        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        // Assert
        let prompts = provider.semantic_prompts();
        assert_eq!(prompts.len(), 2);
        assert!(prompts[1].contains("# Semantic batch 1"));
        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("semantic.md")).expect("semantic"),
            "# Semantic batch 2"
        );
    }

    #[tokio::test]
    async fn publication_leaves_legacy_prospective_file_untouched() {
        // Arrange
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        std::fs::create_dir_all(&memory_dir).expect("memory dir");
        std::fs::write(memory_dir.join("prospective.md"), "LEGACY PROSPECTIVE")
            .expect("legacy prospective");
        let provider = Arc::new(ScenarioProvider::new(
            event_response("publication event"),
            vec![semantic_response("# Semantic new")],
        ));
        let state = build_test_state_with_llm(db, dir.path(), provider.clone());

        // Act
        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        // Assert
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("prospective.md")).expect("legacy"),
            "LEGACY PROSPECTIVE"
        );
        assert!(memory_dir.join("episodic.md").exists());
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("semantic.md")).expect("semantic"),
            "# Semantic new"
        );
    }

    #[test]
    fn inclusive_week_end_subtracts_one_day_from_exclusive_end() {
        use chrono::TimeZone;
        let tz = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
        let period_end_exclusive = tz.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let week_end_date = period_end_exclusive.date_naive() - chrono::Duration::days(1);
        let week_end = week_end_date.format("%Y-%m-%d").to_string();
        assert_eq!(week_end, "2026-05-31");
    }

    use crate::storage::SleepRunTrigger;

    fn ensure_sleep_runs_table(db: &Database) {
        let conn = db.get_conn().expect("pool");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sleep_runs (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                trigger_type TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                source_chats_json TEXT NOT NULL DEFAULT '[]',
                source_digest_md TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                error_message TEXT
            )",
        )
        .expect("create sleep_runs table");
    }

    fn create_completed_sleep_run(db: &Database, agent_id: &str) -> String {
        ensure_sleep_runs_table(db);
        let run_id = db
            .create_sleep_run(agent_id, SleepRunTrigger::Manual)
            .expect("create sleep run");
        db.update_sleep_run_success(&run_id, "[]", None, 10, 5)
            .expect("complete sleep run");
        run_id
    }

    #[test]
    fn collect_returns_skip_when_no_messages() {
        let (db, _dir) = test_db();
        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Skip {
                reason,
                new_message_count,
            } => {
                assert_eq!(new_message_count, 0);
                assert!(reason.contains("0"));
                assert!(reason.contains("more than 64"));
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn collect_proceeds_when_semantic_events_are_pending() {
        // Arrange
        let (db, _dir) = test_db();
        insert_episode_event(&db, "test-agent", "backfill-e1", "2026-01-01T00:00:01Z");

        // Act
        let result = collect_sleep_input(&db, "test-agent").expect("collect");

        // Assert
        match result {
            InputDecision::Proceed {
                sessions,
                source_chats_json,
            } => {
                assert!(
                    sessions.is_empty(),
                    "message extraction must stay threshold-gated"
                );
                assert_eq!(source_chats_json, "[]");
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sleep_batch_processes_semantic_backlog_without_new_messages() {
        // Arrange
        let (db, dir) = test_db();
        insert_episode_event(&db, "test-agent", "backfill-e1", "2026-01-01T00:00:01Z");
        let provider = Arc::new(ScenarioProvider::new(
            r#"{"events":[]}"#.to_string(),
            vec![semantic_response("# Semantic from backlog")],
        ));
        let state = build_test_state_with_llm(db, dir.path(), provider);

        // Act
        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        // Assert
        let runs = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Success);
        let steps = state.db.list_sleep_run_steps(&runs[0].id).expect("steps");
        let extraction = steps
            .iter()
            .find(|step| step.step_name == SleepStepName::EventExtraction)
            .expect("extraction step");
        let semantic = steps
            .iter()
            .find(|step| step.step_name == SleepStepName::SemanticUpdate)
            .expect("semantic step");
        assert_eq!(extraction.status, SleepStepStatus::Skipped);
        assert_eq!(semantic.status, SleepStepStatus::Success);
        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("semantic.md")).expect("semantic"),
            "# Semantic from backlog"
        );
    }

    #[test]
    fn collect_returns_skip_when_below_threshold() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=SKIP_THRESHOLD {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Skip {
                reason: _,
                new_message_count,
            } => {
                assert_eq!(new_message_count, SKIP_THRESHOLD);
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn collect_returns_proceed_above_threshold() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=(SKIP_THRESHOLD + 1) {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(
            chat_id,
            &session_envelope(r#"[{"role":"user","content":"hi"}]"#),
        )
        .expect("save session");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Proceed {
                sessions,
                source_chats_json,
            } => {
                assert!(!sessions.is_empty());
                assert!(!source_chats_json.is_empty());
                assert!(source_chats_json.starts_with('['));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn collect_since_last_successful_run() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");

        let run_id = create_completed_sleep_run(&db, "test-agent");
        let run = db.get_sleep_run(&run_id).expect("get run").expect("exists");
        let _finished_at = run.finished_at.expect("has finished_at");

        store_msg(&db, "old-1", chat_id, "old", "2020-01-01T00:00:01Z");
        store_msg(&db, "old-2", chat_id, "old", "2020-01-01T00:00:02Z");
        store_msg(&db, "old-3", chat_id, "old", "2020-01-01T00:00:03Z");

        db.save_session(
            chat_id,
            &session_envelope(r#"[{"role":"user","content":"hi"}]"#),
        )
        .expect("save session");

        let after_cutoff = chrono::Utc::now().to_rfc3339();
        for i in 1..=(SKIP_THRESHOLD + 1) {
            store_msg(&db, &format!("new-{i}"), chat_id, "new", &after_cutoff);
        }

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        assert!(
            matches!(result, InputDecision::Proceed { .. }),
            "messages above threshold should trigger Proceed"
        );
    }

    #[test]
    fn collect_first_run_no_previous_run() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=(SKIP_THRESHOLD + 1) {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(
            chat_id,
            &session_envelope(r#"[{"role":"user","content":"hi"}]"#),
        )
        .expect("save session");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        assert!(
            matches!(result, InputDecision::Proceed { .. }),
            "messages above threshold with no previous run should trigger Proceed"
        );
    }

    #[test]
    fn collect_respects_max_sessions_limit() {
        let (db, _dir) = test_db();
        seed_sessions_above_threshold(&db, "test-agent", MAX_SOURCE_SESSIONS + 5);

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Proceed { sessions, .. } => {
                assert_eq!(sessions.len(), MAX_SOURCE_SESSIONS);
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn collect_source_chats_json_format() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=(SKIP_THRESHOLD + 1) {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(
            chat_id,
            &session_envelope(r#"[{"role":"user","content":"hi"}]"#),
        )
        .expect("save session");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        let json_str = match &result {
            InputDecision::Proceed {
                source_chats_json, ..
            } => source_chats_json,
            other => panic!("expected Proceed, got {other:?}"),
        };

        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(json_str).expect("valid JSON array");
        assert!(!parsed.is_empty(), "should contain at least one entry");

        let entry = &parsed[0];
        assert!(entry.get("chat_id").is_some(), "missing chat_id");
        assert!(entry.get("channel").is_some(), "missing channel");
        assert!(
            entry.get("external_chat_id").is_some(),
            "missing external_chat_id"
        );
        assert!(entry.get("updated_at").is_some(), "missing updated_at");
        assert!(
            entry.get("message_count").is_some(),
            "missing message_count"
        );
        assert!(
            entry.get("estimated_tokens").is_some(),
            "missing estimated_tokens"
        );
    }

    #[test]
    fn count_pending_counts_same_message_id_across_chats() {
        let (db, _dir) = test_db();
        // The messages table has a composite primary key (id, chat_id), so the
        // same message id may exist in two different chats. The pending count
        // must be based on that composite identity, not on DISTINCT m.id alone.
        let c1 = create_chat(&db, "test-agent", "-1");
        store_msg(&db, "1", c1, "hi", "2025-01-01T00:00:01Z");
        let c2 = create_chat(&db, "test-agent", "-2");
        store_msg(&db, "1", c2, "hi", "2025-01-01T00:00:02Z");
        db.save_session(c1, &session_envelope(r#"[{"role":"user","content":"hi"}]"#))
            .expect("save session");
        db.save_session(c2, &session_envelope(r#"[{"role":"user","content":"hi"}]"#))
            .expect("save session");

        let count = db
            .count_agent_pending_sleep_messages("test-agent")
            .expect("count");
        assert_eq!(
            count, 2,
            "duplicate message id across distinct chats must both count as pending"
        );
    }

    #[test]
    fn collect_source_chats_json_sorted_oldest_pending_first() {
        let (db, _dir) = test_db();
        // Three chats are created in order cA, cB, cC, but their oldest pending
        // (timestamp, id) tuples are deliberately ordered cC < cB < cA. This
        // differs from both the creation/save (updated_at) order and the buggy
        // MIN(timestamp)/MIN(id) ordering, so the test fails if ordering regresses.
        let c_a = seed_oldest_pending_chat(
            &db,
            "test-agent",
            "-a",
            ("2025-06-01T00:10:00Z", "z"),
            ("2025-06-01T00:11:00Z", "a"),
            22,
        );
        let c_b = seed_oldest_pending_chat(
            &db,
            "test-agent",
            "-b",
            ("2025-06-01T00:10:00Z", "a"),
            ("2025-06-01T00:11:00Z", "z"),
            22,
        );
        let c_c = seed_oldest_pending_chat(
            &db,
            "test-agent",
            "-c",
            ("2025-06-01T00:09:00Z", "m"),
            ("2025-06-01T00:11:00Z", "zzz"),
            22,
        );

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        let sessions = match &result {
            InputDecision::Proceed { sessions, .. } => sessions,
            other => panic!("expected Proceed, got {other:?}"),
        };

        let ids: Vec<i64> = sessions.iter().map(|s| s.chat_id).collect();
        assert_eq!(
            ids,
            vec![c_c, c_b, c_a],
            "sessions must be ordered by each chat's oldest pending (timestamp, id)"
        );
    }

    #[test]
    fn collect_skip_includes_reason_and_count() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        store_msg(&db, "m-1", chat_id, "hi", "2025-01-01T00:00:01Z");
        store_msg(&db, "m-2", chat_id, "hi", "2025-01-01T00:00:02Z");
        store_msg(&db, "m-3", chat_id, "hi", "2025-01-01T00:00:03Z");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Skip {
                reason,
                new_message_count,
            } => {
                assert!(!reason.is_empty(), "reason should not be empty");
                assert!(reason.contains("3"), "reason should mention count");
                assert!(
                    reason.contains("more than 64"),
                    "reason should mention threshold"
                );
                assert_eq!(new_message_count, 3);
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn next_sleep_run_retries_only_failed_step_input() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let first_semantic_fail = vec![
            event_response("first run event"),
            r#"{"not":"valid rollups"}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"not json"#.to_string(),
            r#"not json"#.to_string(),
        ];
        let llm1 = Arc::new(SequentialMockProvider::new(first_semantic_fail));
        let state = build_test_state_with_llm(db, dir.path(), llm1);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("first batch");

        let runs_after_first = state
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs_after_first.len(), 1);

        let steps_first = state
            .db
            .list_sleep_run_steps(&runs_after_first[0].id)
            .expect("steps");
        let semantic_first = steps_first
            .iter()
            .find(|s| s.step_name == SleepStepName::SemanticUpdate)
            .expect("semantic");
        assert_eq!(semantic_first.status, SleepStepStatus::Failed);

        // No new messages are added: the unprocessed semantic events alone must
        // make the next cycle proceed and retry the failed step.
        let provider = Arc::new(ScenarioProvider::new(
            r#"{"events":[]}"#.to_string(),
            vec![semantic_response("# Semantic second")],
        ));
        let config = crate::test_util::test_config(&dir.path().to_string_lossy());
        let state2 = crate::test_util::build_state_for_restart_simulation(
            config,
            Some(provider),
            None,
            Some(Arc::clone(&state.db)),
            None,
        );

        run_sleep_batch(&state2, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("second batch");

        let runs_after_second = state2
            .db
            .list_sleep_runs("test-agent", &[], None, 10)
            .expect("list");
        assert_eq!(runs_after_second.len(), 2);
        assert_eq!(runs_after_second[0].status, SleepRunStatus::Success);
    }

    #[test]
    fn truncate_session_json_preserves_filtering_and_tail_contract() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = crate::assets::AssetStore::new(dir.path()).expect("assets");

        let result = truncate_session_json(
            &assets,
            Some(&session_envelope(
                r#"[
                    {"role":"user","content":"m1"},
                    {"role":"assistant","content":"m2"},
                    {"role":"user","content":"m3"},
                    {"role":"assistant","content":"m4"},
                    {"role":"user","content":"m5"},
                    {"role":"assistant","content":"m6"}
                ]"#,
            )),
            4,
        )
        .expect("truncate");
        let parsed: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        assert_eq!(parsed["version"], 1);
        assert!(parsed["checkpoint"].is_null(), "checkpoint must be cleared");
        assert!(
            parsed["last_usage"].is_null(),
            "usage anchor must be cleared"
        );
        let messages = parsed["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["content"], "m3");
        assert_eq!(messages[3]["content"], "m6");

        for json in [None, Some(session_envelope("[]"))] {
            let parsed: serde_json::Value = serde_json::from_str(
                &truncate_session_json(&assets, json.as_deref(), 4).expect("truncate"),
            )
            .expect("valid JSON");
            assert_eq!(parsed["messages"].as_array().expect("messages").len(), 0);
        }

        let filtered_tool = session_envelope(
            r#"[
                {"role":"user","content":"m1"},
                {"role":"assistant","content":"a1"},
                {"role":"tool","content":"result","tool_call_id":"call_x"},
                {"role":"user","content":"m2"},
                {"role":"assistant","content":"a2"}
            ]"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(
            &truncate_session_json(&assets, Some(&filtered_tool), 4).expect("truncate"),
        )
        .expect("valid JSON");
        let messages = parsed["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 4);
        assert!(messages.iter().all(|message| message["role"] != "tool"));

        let assistant_tool_call = session_envelope(
            r#"[
                {"role":"user","content":"m1"},
                {"role":"assistant","content":"","tool_calls":[{"id":"call_x","name":"read","arguments":{}}]},
                {"role":"assistant","content":"a1"},
                {"role":"user","content":"m2"},
                {"role":"assistant","content":"a2"}
            ]"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(
            &truncate_session_json(&assets, Some(&assistant_tool_call), 4).expect("truncate"),
        )
        .expect("valid JSON");
        let messages = parsed["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 4);
        assert!(
            messages
                .iter()
                .all(|message| message["tool_calls"].is_null())
        );

        let filtered_tail = session_envelope(
            r#"[
                {"role":"user","content":"u1"},
                {"role":"assistant","content":"","tool_calls":[{"id":"c1","name":"read","arguments":{}}]},
                {"role":"tool","content":"r1","tool_call_id":"c1"},
                {"role":"assistant","content":"a1"},
                {"role":"user","content":"u2"},
                {"role":"assistant","content":"","tool_calls":[{"id":"c2","name":"read","arguments":{}}]},
                {"role":"tool","content":"r2","tool_call_id":"c2"},
                {"role":"assistant","content":"a2"},
                {"role":"user","content":"u3"},
                {"role":"assistant","content":"a3"}
            ]"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(
            &truncate_session_json(&assets, Some(&filtered_tail), 4).expect("truncate"),
        )
        .expect("valid JSON");
        assert_eq!(
            parsed["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .map(|message| message["content"].as_str().expect("content"))
                .collect::<Vec<_>>(),
            ["u2", "a2", "u3", "a3"]
        );
    }

    #[test]
    fn session_truncate_rejects_a_corrupt_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = crate::assets::AssetStore::new(dir.path()).expect("assets");

        let error = truncate_session_json(&assets, Some("{not-json"), 4)
            .expect_err("a corrupt snapshot must not be treated as empty");

        assert!(matches!(
            error,
            SleepBatchError::Storage(crate::error::StorageError::SessionSerialize(_))
        ));
    }

    #[test]
    fn archive_failure_leaves_run_running_not_success() {
        use crate::storage::SleepRunTrigger;

        let (db, dir) = test_db();
        let run_id = db
            .try_create_sleep_run("test-agent", SleepRunTrigger::Manual)
            .expect("create run")
            .expect("run id");
        let sessions = serde_json::json!([{
            "chat_id": 1,
            "channel": "cli",
            "external_chat_id": "",
            "updated_at": "2025-01-01T00:00:00Z",
            "message_count": 1,
            "estimated_tokens": 1
        }])
        .to_string();
        db.update_sleep_run_source_chats(&run_id, &sessions)
            .expect("set source chats");
        let run = db
            .get_sleep_run(&run_id)
            .expect("read run")
            .expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Running);

        // Force every DB read inside finalization to fail (simulating an
        // archive/truncate I/O failure).
        db.fault_inject_next_get_conn(u32::MAX);

        let assets = crate::assets::AssetStore::new(dir.path()).expect("assets");
        let result = finalize_published_run_blocking(
            &db,
            &assets,
            &dir.path().join("groups"),
            &[],
            &run,
            None,
        );
        assert!(
            result.is_err(),
            "archive/truncate failure must not finalize the run"
        );
        // The run was never transitioned away from Running.
        assert_eq!(
            run.status,
            SleepRunStatus::Running,
            "run must remain Running on archive/truncate failure"
        );
    }

    // --- Blocker 3: archive file I/O failure must propagate and prevent truncate ---

    #[test]
    fn archive_io_failure_propagates_and_preserves_session() {
        let (db, dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "-archive-io");

        // Save a session with messages so archive has content to write.
        db.save_session(
            chat_id,
            &session_envelope(r#"[{"role":"user","content":"hello"}]"#),
        )
        .expect("save session");

        let session = AgentSessionInfo {
            chat_id,
            channel: "test".to_string(),
            external_chat_id: "test:chat-archive-io".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
            message_count: 1,
            estimated_tokens: 1,
        };

        // Point groups_dir under a file (not a directory) so create_dir_all
        // fails — simulating an archive I/O failure that must propagate.
        let blocking_file = dir.path().join("blocking-file");
        std::fs::write(&blocking_file, "not a directory").expect("write blocking file");
        let bad_groups_dir = blocking_file.join("groups");

        let assets = crate::assets::AssetStore::new(dir.path()).expect("assets");
        let result = archive_and_clear_session(
            &db,
            &assets,
            &bad_groups_dir,
            "run-archive-io",
            &session,
            &[],
        );
        assert!(
            matches!(result, Err(SleepBatchError::ArchivePending { .. })),
            "archive I/O failure must propagate as ArchivePending"
        );

        // The session must NOT have been truncated — messages_json preserved.
        let snapshot = db.load_session_snapshot(chat_id, 100).expect("snapshot");
        assert!(
            snapshot.messages_json.as_deref().unwrap_or("[]") != "[]",
            "session must retain its messages after archive failure"
        );
    }

    #[test]
    fn archive_uses_deterministic_idempotency_key() {
        let (db, dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "-archive-det");

        db.save_session(
            chat_id,
            &session_envelope(r#"[{"role":"user","content":"hello"}]"#),
        )
        .expect("save session");

        let session = AgentSessionInfo {
            chat_id,
            channel: "test".to_string(),
            external_chat_id: "test:chat-archive-det".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
            message_count: 1,
            estimated_tokens: 1,
        };

        let groups_dir = dir.path().join("groups");

        // First call: archives successfully.
        let assets = crate::assets::AssetStore::new(dir.path()).expect("assets");
        archive_and_clear_session(&db, &assets, &groups_dir, "run-det", &session, &[])
            .expect("archive success");

        let expected_path = groups_dir
            .join("test")
            .join(chat_id.to_string())
            .join("conversations")
            .join(format!("run-det-{chat_id}.md"));
        assert!(
            expected_path.exists(),
            "archive file must use deterministic name: {}",
            expected_path.display()
        );

        // After archive + truncate, a retry must NOT overwrite the original
        // archive with the now-truncated session. The existence guard skips
        // the write, preserving the original full conversation.
        let original_bytes = std::fs::read_to_string(&expected_path).expect("read original");
        assert!(
            original_bytes.contains("hello"),
            "original archive must contain the conversation"
        );

        // Simulate post-truncation: session now has different (shorter) content.
        db.save_session(
            chat_id,
            &session_envelope(r#"[{"role":"user","content":"truncated-only"}]"#),
        )
        .expect("re-save truncated session");
        archive_and_clear_session(&db, &assets, &groups_dir, "run-det", &session, &[])
            .expect("re-archive (skipped)");
        let retry_bytes = std::fs::read_to_string(&expected_path).expect("read retry");
        assert_eq!(
            original_bytes, retry_bytes,
            "archive must not be overwritten with truncated content on retry"
        );
        assert!(
            !retry_bytes.contains("truncated-only"),
            "truncated content must not have replaced the original archive"
        );
    }
}

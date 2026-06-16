//! Sleep batch orchestrator — coordinates 4 independent steps:
//! event_extraction, episodic_update, semantic_update, prospective_update.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Datelike;
use chrono_tz::OffsetComponents;

use tracing::{info, warn};

use crate::agent_loop::compaction::archive_conversation_blocking;
use crate::llm::{LlmProvider, Message};
use crate::memory::MemoryContent;
use crate::runtime::AppState;
use crate::storage::{
    AgentSessionInfo, CheckpointSourceKind, Database, EpisodeEvent, MemoryFile, RollupGranularity,
    SleepRunTrigger, SleepStepCheckpoint, SleepStepName, SleepStepResult, SleepStepStatus,
    call_blocking,
};

use super::SleepBatchError;
use super::episodic_renderer;
use super::event_extraction::{self, ExtractedEvent};
use super::event_rollup;
use super::memory_update;

/// Threshold (≤ 16) at which sleep is skipped due to too few new messages.
const SKIP_THRESHOLD: i64 = 16;
/// Maximum number of source sessions included in sleep input.
const MAX_SOURCE_SESSIONS: usize = 20;

/// Decision from checking whether enough new messages exist for a sleep run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputDecision {
    /// Not enough new messages → skip the sleep run.
    Skip {
        /// Human-readable reason for the skip.
        reason: String,
        /// Number of new messages found (≤ SKIP_THRESHOLD).
        new_message_count: i64,
    },
    /// Enough new messages → proceed with sleep run.
    Proceed {
        /// Source sessions (limited to MAX_SOURCE_SESSIONS).
        sessions: Vec<AgentSessionInfo>,
        /// JSON array of source chat metadata (for sleep_runs.source_chats_json).
        source_chats_json: String,
    },
}

/// Collects sleep input from the database: counts new messages since the last
/// successful sleep run, and if above threshold, fetches source session info.
///
/// # Errors
///
/// Returns [`StorageError`] if DB queries fail.
pub(crate) fn collect_sleep_input(
    db: &Database,
    agent_id: &str,
) -> Result<InputDecision, crate::error::StorageError> {
    let latest_run = db.get_latest_successful_run(agent_id)?;
    let cutoff = latest_run.as_ref().and_then(|r| r.finished_at.as_deref());

    let new_message_count = db.count_agent_messages_since(agent_id, cutoff)?;

    if new_message_count <= SKIP_THRESHOLD {
        let reason =
            format!("new messages ({new_message_count}) at or below threshold ({SKIP_THRESHOLD})");
        return Ok(InputDecision::Skip {
            reason,
            new_message_count,
        });
    }

    let sessions = db.get_agent_sessions_since(agent_id, cutoff, MAX_SOURCE_SESSIONS)?;
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
    let resolved_agent = match agent_id {
        Some(id) => id.to_string(),
        None => state.config.default_agent.as_str().to_string(),
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
            )
            .await
        }
    }
}

/// Extracts episode events from past conversation history for backfilling.
///
/// Unlike normal sleep batch (which runs 4 steps), this only runs event extraction
/// (Event Extraction) using the messages table. Old backfill events in the
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
    let resolved_agent = match agent_id {
        Some(id) => id.to_string(),
        None => state.config.default_agent.as_str().to_string(),
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
        let agents_dir = PathBuf::from(&state.config.state_root).join("agents");
        recover_memory_write(&agents_dir, &resolved_agent)?;

        let resolved = state
            .config
            .resolve_sleep_batch_llm()
            .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

        let provider: Arc<dyn LlmProvider> =
            if let Some(override_provider) = state.llm_override.clone() {
                override_provider
            } else {
                state
                    .cached_provider(&resolved)
                    .map_err(|e| SleepBatchError::Llm(e.to_string()))?
            };

        let context_tokens = state.config.resolve_context_window_tokens(
            &crate::config::ProviderId::new(&resolved.provider),
            &resolved.model,
        );
        let chunk_session_tokens = memory_update::sleep_chunk_session_tokens(context_tokens);

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
) -> Result<(Vec<event_rollup::Call2RollupOutput>, i64, i64), SleepBatchError> {
    let user_prompt = event_rollup::build_call2_user_prompt(input_json);
    let user_message = Message::text("user", user_prompt);

    let response = provider
        .send_message(system_prompt, Arc::new(vec![user_message.clone()]), None)
        .await
        .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

    let first_input = response.usage.as_ref().map_or(0, |u| u.input_tokens);
    let first_output = response.usage.as_ref().map_or(0, |u| u.output_tokens);
    let output_json = event_rollup::redact_secrets(&response.content);

    match event_rollup::parse_call2_output(&output_json, valid_keys) {
        Ok(outputs) => Ok((outputs, first_input, first_output)),
        Err(first_error) => {
            warn!(
                agent_id = %agent_id,
                error = %first_error,
                "Call2 parse failed; retrying once"
            );
            const CALL2_RETRY_GUARD: &str = "\
Your previous response was not valid JSON according to the expected schema. \
You must respond with ONLY a JSON object containing exactly one key: \
\"rollups\" (an array of rollup objects). \
Each rollup must have: granularity, period_key, summary_md, max_ripple, event_count. \
Do not include any other keys, markdown formatting, code blocks, or explanatory text. \
Output the raw JSON object and nothing else.";
            let retry_messages = vec![
                user_message,
                Message::text("assistant", &response.content),
                Message::text("user", CALL2_RETRY_GUARD),
            ];
            let retry_response = provider
                .send_message(system_prompt, Arc::new(retry_messages), None)
                .await
                .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

            let retry_input = retry_response.usage.as_ref().map_or(0, |u| u.input_tokens);
            let retry_output = retry_response.usage.as_ref().map_or(0, |u| u.output_tokens);
            let combined_input = first_input.saturating_add(retry_input);
            let combined_output = first_output.saturating_add(retry_output);
            let retry_json = event_rollup::redact_secrets(&retry_response.content);

            match event_rollup::parse_call2_output(&retry_json, valid_keys) {
                Ok(outputs) => Ok((outputs, combined_input, combined_output)),
                Err(retry_error) => {
                    warn!(
                        agent_id = %agent_id,
                        error = %retry_error,
                        "Call2 retry also failed"
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
    agents_dir: PathBuf,
    provider: Arc<dyn LlmProvider>,
    sessions: Vec<AgentSessionInfo>,
    current_memory: MemoryContent,
    context_tokens: usize,
}

struct MessageStepInput {
    chunks: Vec<String>,
    checkpoints: Vec<SleepStepCheckpoint>,
}

fn load_message_step_input(
    ctx: &BatchContext,
    db: &Database,
    agent_id: &str,
    step_name: SleepStepName,
) -> Result<MessageStepInput, SleepBatchError> {
    let max_tokens = memory_update::sleep_chunk_session_tokens(ctx.context_tokens);
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
            step_name,
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
            step_name,
            source_kind: CheckpointSourceKind::Messages,
            source_id,
            cursor_at: last.timestamp.clone(),
            cursor_id: last.id.clone(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });

        let text = event_extraction::messages_to_extract_text(&messages);
        for block in memory_update::session_blocks(session, &text, max_chars) {
            memory_update::append_chunk_block(&mut chunks, &mut current, block, max_chars);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    Ok(MessageStepInput {
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
) -> Result<(), SleepBatchError> {
    let run_id = create_sleep_run(&db, agent_id, trigger).await?;

    let result = async {
        let mut ctx = prepare_batch_context(state, agent_id, sessions, &run_id).await?;
        run_event_extraction_step(&mut ctx, &db, agent_id).await?;
        run_episodic_update_step(&mut ctx, state, &db, agent_id).await?;
        run_memory_update_step(&mut ctx, &db, agent_id).await?;

        finalize_batch(&ctx, state, &db, agent_id, sessions, source_chats_json).await?;

        Ok::<(), SleepBatchError>(())
    }
    .await;

    if let Err(error) = result {
        let run_id_owned = run_id.clone();
        let error_message = error.to_string();
        call_blocking(db, move |db| {
            db.update_sleep_run_failed(&run_id_owned, &error_message)
        })
        .await?;
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
) -> Result<BatchContext, SleepBatchError> {
    let agents_dir = PathBuf::from(&state.config.state_root).join("agents");
    recover_memory_write(&agents_dir, agent_id)?;

    let resolved = state
        .config
        .resolve_sleep_batch_llm()
        .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

    let provider: Arc<dyn LlmProvider> = if let Some(override_provider) = state.llm_override.clone()
    {
        override_provider
    } else {
        state
            .cached_provider(&resolved)
            .map_err(|e| SleepBatchError::Llm(e.to_string()))?
    };

    let context_tokens = state.config.resolve_context_window_tokens(
        &crate::config::ProviderId::new(&resolved.provider),
        &resolved.model,
    );
    let memory_before = state.memory_loader.load(agent_id);

    Ok(BatchContext {
        run_id: run_id.to_string(),
        agents_dir,
        provider,
        sessions: sessions.to_vec(),
        current_memory: memory_before.unwrap_or_default(),
        context_tokens,
    })
}

/// Step 1: Event Extraction — extracts episode events from session chunks.
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

    let input = match load_message_step_input(ctx, db, agent_id, SleepStepName::EventExtraction) {
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

/// Step 2: Episodic Update — rollup generation and episodic.md rendering.
async fn run_episodic_update_step(
    ctx: &mut BatchContext,
    state: &AppState,
    db: &Arc<Database>,
    agent_id: &str,
) -> Result<Option<String>, SleepBatchError> {
    let run_id = ctx.run_id.clone();
    call_blocking(Arc::clone(db), move |db| {
        db.start_sleep_step(&run_id, SleepStepName::EpisodicUpdate)
    })
    .await?;

    let tz_str = &state.config.timezone;
    let tz_chrono: chrono_tz::Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);
    let tz = resolve_fixed_offset(tz_str);
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
        episodic_renderer::render_episodic_view(db, agent_id, tz_str, &cw, &current_week_events)
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

    let before = ctx.current_memory.clone();
    let mut after = before.clone();
    after.episodic = Some(rendered.clone());
    if let Err(error) = write_memory_files(
        &ctx.agents_dir,
        agent_id,
        &[
            ("episodic.md", rendered.as_str()),
            (
                "semantic.md",
                before.semantic.clone().unwrap_or_default().as_str(),
            ),
            (
                "prospective.md",
                before.prospective.clone().unwrap_or_default().as_str(),
            ),
        ],
    ) {
        finish_step_failed(
            db,
            &ctx.run_id,
            SleepStepName::EpisodicUpdate,
            error.to_string(),
        )
        .await;
        return Ok(None);
    }
    let run_id = ctx.run_id.clone();
    let agent_id_owned = agent_id.to_string();
    let content_before = before.episodic.clone().unwrap_or_default();
    let content_after = rendered.clone();
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
        let _ = write_memory_files(
            &ctx.agents_dir,
            agent_id,
            &[
                (
                    "episodic.md",
                    before.episodic.clone().unwrap_or_default().as_str(),
                ),
                (
                    "semantic.md",
                    before.semantic.clone().unwrap_or_default().as_str(),
                ),
                (
                    "prospective.md",
                    before.prospective.clone().unwrap_or_default().as_str(),
                ),
            ],
        );
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

    ctx.current_memory = after;
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
        let mut week_events_map: HashMap<String, Vec<event_rollup::Call2Event>> = HashMap::new();
        for req in &week_requests {
            let req_start = req.period_start.clone();
            let req_end = req.period_end_exclusive.clone();
            let req_key = req.period_key.clone();
            let agent_for_range = agent_id.to_string();
            let period_events: Vec<EpisodeEvent> = call_blocking(Arc::clone(db), move |db| {
                db.list_episode_events_in_range(&agent_for_range, &req_start, &req_end)
            })
            .await?;

            let call2_events: Vec<event_rollup::Call2Event> = period_events
                .iter()
                .map(|e| event_rollup::Call2Event {
                    id: e.id.clone(),
                    experienced_at: e.experienced_at.clone(),
                    kind: e.kind.to_string(),
                    title: e.title.clone(),
                    body_md: e.body_md.clone(),
                    ripple_strength: e.ripple_strength,
                    certainty: e.certainty.to_string(),
                })
                .collect();
            week_events_map.insert(req_key, call2_events);
        }

        let week_input = event_rollup::build_call2_input(&week_requests, &week_events_map);
        let week_input_json = serde_json::to_string_pretty(&serde_json::json!({
            "rollup_requests": week_input
        }))
        .map_err(|e| SleepBatchError::Internal(e.to_string()))?;
        let week_input_json = event_rollup::redact_secrets(&week_input_json);

        let week_system_prompt = event_rollup::build_call2_system_prompt_week(agent_id);
        let valid_keys: HashSet<String> =
            week_requests.iter().map(|r| r.period_key.clone()).collect();

        let (rollup_outputs, call2_in, call2_out) = call_rollup_llm_with_retry(
            &ctx.provider,
            &week_system_prompt,
            &week_input_json,
            &valid_keys,
            agent_id,
        )
        .await?;

        total_input = total_input.saturating_add(call2_in);
        total_output = total_output.saturating_add(call2_out);

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
        let mut week_rollups_map: HashMap<String, Vec<event_rollup::Call2WeekRollupSummary>> =
            HashMap::new();
        for req in &month_requests {
            let summaries: Vec<event_rollup::Call2WeekRollupSummary> = existing_week_rollups
                .iter()
                .filter(|wr| {
                    event_rollup::week_in_month(&wr.period_key, &req.period_key, tz_chrono)
                })
                .map(|wr| event_rollup::Call2WeekRollupSummary {
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

        let month_input = event_rollup::build_call2_input_month(
            &month_requests,
            &week_rollups_map,
            &previous_month_map,
        );
        let month_input_json = serde_json::to_string_pretty(&serde_json::json!({
            "rollup_requests": month_input
        }))
        .map_err(|e| SleepBatchError::Internal(e.to_string()))?;
        let month_input_json = event_rollup::redact_secrets(&month_input_json);

        let month_system_prompt = event_rollup::build_call2_system_prompt_month(agent_id);
        let valid_keys: HashSet<String> = month_requests
            .iter()
            .map(|r| r.period_key.clone())
            .collect();

        let (rollup_outputs, call2_in, call2_out) = call_rollup_llm_with_retry(
            &ctx.provider,
            &month_system_prompt,
            &month_input_json,
            &valid_keys,
            agent_id,
        )
        .await?;

        total_input = total_input.saturating_add(call2_in);
        total_output = total_output.saturating_add(call2_out);

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

/// Steps 3 & 4: Memory Update — updates semantic and prospective memory via a single LLM call.
async fn run_memory_update_step(
    ctx: &mut BatchContext,
    db: &Arc<Database>,
    agent_id: &str,
) -> Result<(), SleepBatchError> {
    let run_id = ctx.run_id.clone();
    call_blocking(Arc::clone(db), move |db| {
        db.start_memory_update_steps(&run_id)
    })
    .await?;

    let prospective_input =
        load_message_step_input(ctx, db, agent_id, SleepStepName::ProspectiveUpdate)?;
    let semantic_checkpoint = db.get_sleep_checkpoint(
        agent_id,
        SleepStepName::SemanticUpdate,
        CheckpointSourceKind::EpisodeEvents,
        agent_id,
    )?;
    let semantic_cursor = semantic_checkpoint
        .as_ref()
        .map(|value| (value.cursor_at.as_str(), value.cursor_id.as_str()));
    let events = match db.get_latest_episode_event_cursor(agent_id)? {
        Some(upper_bound) => db.get_episode_events_after_cursor(
            agent_id,
            semantic_cursor,
            (&upper_bound.0, &upper_bound.1),
        )?,
        None => Vec::new(),
    };

    if prospective_input.chunks.is_empty() && events.is_empty() {
        let run_id = ctx.run_id.clone();
        call_blocking(Arc::clone(db), move |db| {
            db.finish_memory_update_steps(
                &run_id,
                SleepStepResult {
                    status: SleepStepStatus::Skipped,
                    input_tokens: 0,
                    output_tokens: 0,
                    error_message: None,
                    metadata_json: None,
                },
            )
        })
        .await?;
        return Ok(());
    }

    let mut checkpoints = prospective_input.checkpoints;
    if let Some(last) = events.last() {
        checkpoints.push(SleepStepCheckpoint {
            agent_id: agent_id.to_string(),
            step_name: SleepStepName::SemanticUpdate,
            source_kind: CheckpointSourceKind::EpisodeEvents,
            source_id: agent_id.to_string(),
            cursor_at: last.encoded_at.clone(),
            cursor_id: last.id.clone(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });
    }

    let event_text = if events.is_empty() {
        String::new()
    } else {
        let event_values = events
            .iter()
            .map(|event| {
                serde_json::json!({
                    "id": event.id,
                    "experienced_at": event.experienced_at,
                    "kind": event.kind.to_string(),
                    "title": event.title,
                    "body_md": event.body_md,
                    "ripple_strength": event.ripple_strength,
                    "certainty": event.certainty.to_string(),
                })
            })
            .collect::<Vec<_>>();
        format!(
            "<episode-events>\n{}\n</episode-events>\n",
            serde_json::to_string(&event_values)
                .map_err(|error| SleepBatchError::Internal(error.to_string()))?
        )
    };
    let mut chunks = prospective_input.chunks;
    if chunks.is_empty() {
        chunks.push(event_text);
    } else if !event_text.is_empty() {
        chunks[0] = format!("{event_text}\n{}", chunks[0]);
    }

    let total_chunks = chunks.len();
    let mut working_memory = ctx.current_memory.clone();
    let mut total_input: i64 = 0;
    let mut total_output: i64 = 0;
    let mut step_failed = false;
    let mut error_msg = None;

    for (index, sessions_text) in chunks.into_iter().enumerate() {
        let input = match memory_update::build_sleep_input_from_parts(
            agent_id,
            working_memory.clone(),
            sessions_text,
            ctx.context_tokens,
            0,
        ) {
            Ok(input) => input,
            Err(e) => {
                warn!(error = %e, "failed to build sleep input");
                step_failed = true;
                error_msg = Some(e.to_string());
                break;
            }
        };

        let system_prompt = memory_update::build_sleep_system_prompt(&input);
        match memory_update::send_sleep_request(
            &ctx.provider,
            agent_id,
            &system_prompt,
            index + 1,
            total_chunks,
        )
        .await
        {
            Ok((output, inp, out)) => {
                total_input = total_input.saturating_add(inp);
                total_output = total_output.saturating_add(out);
                working_memory.semantic = Some(output.semantic.clone());
                working_memory.prospective = Some(output.prospective.clone());
            }
            Err(e) => {
                warn!(error = %e, "memory update failed");
                step_failed = true;
                error_msg = Some(e.to_string());
                break;
            }
        }
    }

    if step_failed {
        let run_id = ctx.run_id.clone();
        call_blocking(Arc::clone(db), move |db| {
            db.finish_memory_update_steps(
                &run_id,
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

    let before = ctx.current_memory.clone();
    let snapshots = [
        (
            MemoryFile::Semantic,
            before.semantic.clone().unwrap_or_default(),
            working_memory.semantic.clone().unwrap_or_default(),
        ),
        (
            MemoryFile::Prospective,
            before.prospective.clone().unwrap_or_default(),
            working_memory.prospective.clone().unwrap_or_default(),
        ),
    ]
    .into_iter()
    .filter(|(_, content_before, content_after)| content_before != content_after)
    .collect::<Vec<_>>();

    if snapshots.is_empty() {
        let run_id = ctx.run_id.clone();
        let agent_id = agent_id.to_string();
        call_blocking(Arc::clone(db), move |db| {
            db.commit_memory_update_success(
                &run_id,
                &agent_id,
                SleepStepResult {
                    status: SleepStepStatus::Success,
                    input_tokens: total_input,
                    output_tokens: total_output,
                    error_message: None,
                    metadata_json: None,
                },
                &checkpoints,
                &[],
            )
        })
        .await?;
        return Ok(());
    }

    if let Err(error) = write_memory_files(
        &ctx.agents_dir,
        agent_id,
        &[
            (
                "episodic.md",
                working_memory.episodic.clone().unwrap_or_default().as_str(),
            ),
            (
                "semantic.md",
                working_memory.semantic.clone().unwrap_or_default().as_str(),
            ),
            (
                "prospective.md",
                working_memory
                    .prospective
                    .clone()
                    .unwrap_or_default()
                    .as_str(),
            ),
        ],
    ) {
        let run_id = ctx.run_id.clone();
        let error_message = error.to_string();
        call_blocking(Arc::clone(db), move |db| {
            db.finish_memory_update_steps(
                &run_id,
                SleepStepResult {
                    status: SleepStepStatus::Failed,
                    input_tokens: total_input,
                    output_tokens: total_output,
                    error_message: Some(&error_message),
                    metadata_json: None,
                },
            )
        })
        .await?;
        return Ok(());
    }
    let run_id = ctx.run_id.clone();
    let agent_id_owned = agent_id.to_string();
    if let Err(error) = call_blocking(Arc::clone(db), move |db| {
        db.commit_memory_update_success(
            &run_id,
            &agent_id_owned,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: total_input,
                output_tokens: total_output,
                error_message: None,
                metadata_json: None,
            },
            &checkpoints,
            &snapshots,
        )
    })
    .await
    {
        let _ = write_memory_files(
            &ctx.agents_dir,
            agent_id,
            &[
                (
                    "episodic.md",
                    before.episodic.clone().unwrap_or_default().as_str(),
                ),
                (
                    "semantic.md",
                    before.semantic.clone().unwrap_or_default().as_str(),
                ),
                (
                    "prospective.md",
                    before.prospective.clone().unwrap_or_default().as_str(),
                ),
            ],
        );
        let run_id = ctx.run_id.clone();
        let error_message = error.to_string();
        call_blocking(Arc::clone(db), move |db| {
            db.finish_memory_update_steps(
                &run_id,
                SleepStepResult {
                    status: SleepStepStatus::Failed,
                    input_tokens: total_input,
                    output_tokens: total_output,
                    error_message: Some(&error_message),
                    metadata_json: None,
                },
            )
        })
        .await?;
        return Ok(());
    }

    ctx.current_memory = working_memory;
    Ok(())
}

/// Finalizes batch: writes memory files, archives sessions, logs usage.
async fn finalize_batch(
    ctx: &BatchContext,
    state: &AppState,
    db: &Arc<Database>,
    agent_id: &str,
    sessions: &[AgentSessionInfo],
    source_chats_json: &str,
) -> Result<(), SleepBatchError> {
    let groups_dir = state.config.groups_dir();
    let secrets = crate::tools::collect_config_secrets(&state.config);
    for session in sessions {
        let source_id = session.chat_id.to_string();
        let event_checkpoint = db.get_sleep_checkpoint(
            agent_id,
            SleepStepName::EventExtraction,
            CheckpointSourceKind::Messages,
            &source_id,
        )?;
        let prospective_checkpoint = db.get_sleep_checkpoint(
            agent_id,
            SleepStepName::ProspectiveUpdate,
            CheckpointSourceKind::Messages,
            &source_id,
        )?;
        let (Some(event_checkpoint), Some(prospective_checkpoint)) =
            (event_checkpoint, prospective_checkpoint)
        else {
            continue;
        };
        let boundary = std::cmp::min(
            (event_checkpoint.cursor_at, event_checkpoint.cursor_id),
            (
                prospective_checkpoint.cursor_at,
                prospective_checkpoint.cursor_id,
            ),
        );
        let Some(latest) = db.get_latest_message_cursor(session.chat_id)? else {
            continue;
        };
        if latest > boundary {
            continue;
        }
        if let Err(e) = archive_and_clear_session(db, &groups_dir, session, &secrets) {
            warn!(
                agent_id = %agent_id,
                chat_id = session.chat_id,
                error = %e,
                "failed to archive/clear session (continuing)"
            );
        }
    }

    let run_id_for_source = ctx.run_id.clone();
    let source_chats = source_chats_json.to_string();
    call_blocking(Arc::clone(db), move |db| {
        db.update_sleep_run_source_chats(&run_id_for_source, &source_chats)
    })
    .await?;

    let run_id_for_finalize = ctx.run_id.clone();
    let derived_status = call_blocking(Arc::clone(db), move |db| {
        db.finalize_sleep_run(&run_id_for_finalize)
    })
    .await?;

    let run_id_for_tokens = ctx.run_id.clone();
    if let Ok(Some(run)) = call_blocking(Arc::clone(db), move |db| {
        db.get_sleep_run(&run_id_for_tokens)
    })
    .await
    {
        if run.input_tokens > 0 || run.output_tokens > 0 {
            let provider_name = ctx.provider.provider_name().to_string();
            let model_name = ctx.provider.model_name().to_string();
            crate::runtime::metrics::inc_llm_tokens_total(
                "input",
                &provider_name,
                run.input_tokens,
            );
            crate::runtime::metrics::inc_llm_tokens_total(
                "output",
                &provider_name,
                run.output_tokens,
            );
            let db_for_usage = Arc::clone(db);
            let input_tokens = run.input_tokens;
            let output_tokens = run.output_tokens;
            tokio::spawn(async move {
                let _ = crate::storage::call_blocking(db_for_usage, move |db| {
                    db.log_llm_usage(&crate::storage::LlmUsageLogEntry {
                        chat_id: 0,
                        caller_channel: "sleep_batch",
                        provider: &provider_name,
                        model: &model_name,
                        input_tokens,
                        output_tokens,
                        request_kind: "sleep_batch",
                    })
                })
                .await
                .inspect_err(|e| warn!(error = %e, "sleep batch llm usage logging failed"));
            });
        }
    }

    info!(
        agent_id = %agent_id,
        run_id = %ctx.run_id,
        status = %derived_status,
        "sleep batch finalized"
    );

    Ok(())
}

fn safe_agent_id_for_write(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && !id.contains("..")
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains(':')
}

pub(crate) fn recover_memory_write(
    agents_dir: &Path,
    agent_id: &str,
) -> Result<(), SleepBatchError> {
    if !safe_agent_id_for_write(agent_id) {
        return Err(SleepBatchError::UnsafeAgentId(agent_id.to_string()));
    }

    let agent_dir = agents_dir.join(agent_id);
    if !agent_dir.exists() {
        return Ok(());
    }

    let memory_dir = agent_dir.join("memory");

    if !memory_dir.exists() {
        let entries = std::fs::read_dir(&agent_dir)
            .map_err(|e| SleepBatchError::Io(format!("failed to read agent dir: {e}")))?;

        let mut backups: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("memory.backup-")
            })
            .collect();

        backups.sort_by(|a, b| {
            let mtime_a = a.metadata().and_then(|m| m.modified()).ok();
            let mtime_b = b.metadata().and_then(|m| m.modified()).ok();
            mtime_b.cmp(&mtime_a)
        });

        if let Some(newest) = backups.into_iter().next() {
            let backup_path = newest.path();
            info!(
                agent_id = %agent_id,
                path = %backup_path.display(),
                "restoring memory from backup"
            );
            std::fs::rename(&backup_path, &memory_dir)
                .map_err(|e| SleepBatchError::Io(format!("failed to restore backup: {e}")))?;
        }
    }

    let entries = std::fs::read_dir(&agent_dir)
        .map_err(|e| SleepBatchError::Io(format!("failed to read agent dir: {e}")))?;

    for entry in entries {
        let entry =
            entry.map_err(|e| SleepBatchError::Io(format!("failed to read dir entry: {e}")))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with("memory.tmp-") || name_str.starts_with("memory.backup-") {
            let path = entry.path();
            info!(
                agent_id = %agent_id,
                path = %path.display(),
                "cleaning up stale memory directory"
            );
            if let Err(e) = std::fs::remove_dir_all(&path) {
                info!(
                    agent_id = %agent_id,
                    path = %path.display(),
                    error = %e,
                    "failed to remove stale directory (continuing)"
                );
            }
        }
    }

    Ok(())
}

pub(crate) fn write_memory_files(
    agents_dir: &Path,
    agent_id: &str,
    files: &[(&str, &str)],
) -> Result<(), SleepBatchError> {
    if !safe_agent_id_for_write(agent_id) {
        return Err(SleepBatchError::UnsafeAgentId(agent_id.to_string()));
    }

    recover_memory_write(agents_dir, agent_id)?;

    let agent_dir = agents_dir.join(agent_id);
    std::fs::create_dir_all(&agent_dir)
        .map_err(|e| SleepBatchError::Io(format!("failed to create agent dir: {e}")))?;

    let uuid = uuid::Uuid::new_v4();
    let tmp_dir = agent_dir.join(format!("memory.tmp-{uuid}"));
    let memory_dir = agent_dir.join("memory");
    let backup_dir = agent_dir.join(format!("memory.backup-{uuid}"));

    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| SleepBatchError::Io(format!("failed to create tmp dir: {e}")))?;

    let write_result = (|| -> Result<(), SleepBatchError> {
        for (name, content) in files {
            std::fs::write(tmp_dir.join(name), content)
                .map_err(|e| SleepBatchError::Io(format!("failed to write {name}: {e}")))?;
        }
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    if memory_dir.exists() {
        std::fs::rename(&memory_dir, &backup_dir).map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            SleepBatchError::Io(format!("failed to rename memory to backup: {e}"))
        })?;
    }

    if let Err(e) = std::fs::rename(&tmp_dir, &memory_dir) {
        if backup_dir.exists() {
            let _ = std::fs::rename(&backup_dir, &memory_dir);
        }
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(SleepBatchError::Io(format!(
            "failed to rename tmp to memory: {e}"
        )));
    }

    if backup_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&backup_dir) {
            info!(
                agent_id = %agent_id,
                error = %e,
                "failed to remove backup dir (non-fatal)"
            );
        }
    }

    Ok(())
}

fn archive_and_clear_session(
    db: &Database,
    groups_dir: &Path,
    session: &AgentSessionInfo,
    secrets: &[(String, String)],
) -> Result<(), SleepBatchError> {
    let snapshot = db
        .load_session_snapshot(session.chat_id, 100)
        .map_err(SleepBatchError::Storage)?;

    if let Some(json) = &snapshot.messages_json {
        let messages = parse_messages_json(json);
        if !messages.is_empty() {
            archive_conversation_blocking(
                groups_dir,
                &session.channel,
                session.chat_id,
                &messages,
                secrets,
            );
        } else {
            info!(
                chat_id = session.chat_id,
                "skipping archive: messages_json parsed as empty"
            );
        }
    }

    if let Some(updated_at) = &snapshot.updated_at {
        let cleared = db
            .clear_session_messages(session.chat_id, updated_at)
            .map_err(SleepBatchError::Storage)?;
        if !cleared {
            warn!(
                chat_id = session.chat_id,
                "skipping session clear: concurrent modification detected"
            );
        }
    }

    Ok(())
}

fn parse_messages_json(json: &str) -> Vec<Message> {
    serde_json::from_str::<Vec<Message>>(json).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmProvider, LlmUsage, Message, MessagesResponse, ToolDefinition};
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
                    "semantic": "",
                    "prospective": ""
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

        fn with_usage(input: i64, output: i64) -> Self {
            Self {
                response: serde_json::json!({
                    "semantic": "",
                    "prospective": ""
                })
                .to_string(),
                input_tokens: input,
                output_tokens: output,
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
                    })
                } else {
                    None
                },
            })
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
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, chat_id, "alice", content, "user", ts, "message"],
        )
        .expect("store message");
    }

    fn create_chat(db: &Database, agent_id: &str, suffix: &str) -> i64 {
        db.resolve_or_create_chat_id(
            "test",
            &format!("test:chat{suffix}"),
            Some(&format!("chat{suffix}")),
            "direct",
            agent_id,
        )
        .expect("create chat")
    }

    fn seed_messages_for_proceed(db: &Database, agent_id: &str) {
        let chat_id = create_chat(db, agent_id, "");
        for i in 1..=17 {
            store_msg(
                db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
            .expect("save session");
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

    fn all_success_responses() -> Vec<String> {
        vec![
            r#"{"events":[]}"#.to_string(),
            r#"{"events":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"semantic":"","prospective":""}"#.to_string(),
        ]
    }

    // --- integration tests (run_sleep_batch) ---

    #[tokio::test]
    async fn run_sleep_batch_skips_when_input_below_threshold() {
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());
        let result = run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual).await;
        assert!(result.is_ok());
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

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, SleepRunStatus::Success);
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
                "semantic": "# Semantic\n\n- fact",
                "prospective": "# Prospective\n\n- todo"
            })
            .to_string(),
        ]));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs.len(), 1);

        let snapshots = state
            .db
            .get_snapshots_for_run(&runs[0].id)
            .expect("snapshots");
        assert!(!snapshots.is_empty(), "should have memory snapshots");
    }

    #[tokio::test]
    async fn run_sleep_batch_recovers_backup_before_building_input() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let agent_dir = dir.path().join("agents").join("test-agent");
        let backup_dir = agent_dir.join("memory.backup-test");
        std::fs::create_dir_all(&backup_dir).expect("create backup dir");
        std::fs::write(backup_dir.join("semantic.md"), "old memory").expect("write backup");

        let llm = Arc::new(MockLlmProvider::new());
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");
    }

    #[tokio::test]
    async fn run_sleep_batch_does_not_record_phases_json() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(MockLlmProvider::new());
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert!(runs[0].source_digest_md.is_none());
    }

    #[tokio::test]
    async fn run_sleep_batch_does_not_record_summary_md() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(MockLlmProvider::new());
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert!(runs[0].source_digest_md.is_none());
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

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Success);
    }

    #[tokio::test]
    async fn run_sleep_batch_persists_all_three_memory_files() {
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
        assert!(
            memory_dir.join("prospective.md").exists(),
            "prospective.md missing"
        );
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

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
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
    async fn scheduled_run_records_success_status() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(SequentialMockProvider::new(all_success_responses()));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Scheduled)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Success);
    }

    #[tokio::test]
    async fn scheduled_run_records_memory_snapshots() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(SequentialMockProvider::new(vec![
            r#"{"events":[]}"#.to_string(),
            r#"{"events":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"rollups":[]}"#.to_string(),
            r#"{"semantic":"s","prospective":"p"}"#.to_string(),
        ]));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Scheduled)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        let snapshots = state
            .db
            .get_snapshots_for_run(&runs[0].id)
            .expect("snapshots");
        assert!(!snapshots.is_empty());
    }

    #[tokio::test]
    async fn scheduled_run_records_source_chats_json() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(MockLlmProvider::new());
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Scheduled)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert!(!runs[0].source_chats_json.is_empty());
    }

    #[tokio::test]
    async fn scheduled_run_records_failed_status() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let llm = Arc::new(MockLlmProvider::with_response(
            serde_json::json!({"bad": true}),
        ));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        let _ = run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Scheduled).await;

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Failed);
    }
    // --- write_memory_files tests ---

    #[test]
    fn write_memory_files_writes_all_given_files() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_files(
            dir.path(),
            "agent",
            &[("semantic.md", "sem"), ("prospective.md", "pro")],
        )
        .expect("write");

        let memory_dir = dir.path().join("agent").join("memory");
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("semantic.md")).unwrap(),
            "sem"
        );
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("prospective.md")).unwrap(),
            "pro"
        );
    }

    #[test]
    fn write_memory_files_writes_single_file() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_files(dir.path(), "agent", &[("episodic.md", "ep")]).expect("write");

        let memory_dir = dir.path().join("agent").join("memory");
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("episodic.md")).unwrap(),
            "ep"
        );
    }

    #[test]
    fn write_memory_files_writes_three_files() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_files(
            dir.path(),
            "agent",
            &[
                ("episodic.md", "ep"),
                ("semantic.md", "sem"),
                ("prospective.md", "pro"),
            ],
        )
        .expect("write");

        let memory_dir = dir.path().join("agent").join("memory");
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("episodic.md")).unwrap(),
            "ep"
        );
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("semantic.md")).unwrap(),
            "sem"
        );
        assert_eq!(
            std::fs::read_to_string(memory_dir.join("prospective.md")).unwrap(),
            "pro"
        );
    }

    #[test]
    fn write_memory_files_creates_memory_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_files(dir.path(), "agent", &[("semantic.md", "s")]).expect("write");

        let memory_dir = dir.path().join("agent").join("memory");
        assert!(memory_dir.exists());
    }

    #[test]
    fn write_memory_files_rejects_unsafe_agent_id() {
        let dir = tempfile::tempdir().unwrap();
        let err = write_memory_files(dir.path(), "../etc", &[("semantic.md", "s")])
            .expect_err("should reject");
        assert!(matches!(err, SleepBatchError::UnsafeAgentId(_)));
    }

    #[test]
    fn write_memory_files_preserves_existing_on_write_error() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent").join("memory");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("semantic.md"), "old").unwrap();

        // This should succeed — we're writing valid content
        write_memory_files(dir.path(), "agent", &[("semantic.md", "new")]).expect("write");
    }

    #[test]
    fn write_memory_files_recovers_backup_on_start() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let backup_dir = agent_dir.join("memory.backup-test");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("semantic.md"), "recovered").unwrap();

        write_memory_files(dir.path(), "agent", &[("semantic.md", "new")]).expect("write");
    }

    #[test]
    fn write_memory_files_cleans_tmp_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let tmp_dir = agent_dir.join("memory.tmp-stale");
        std::fs::create_dir_all(&tmp_dir).unwrap();

        write_memory_files(dir.path(), "agent", &[("semantic.md", "s")]).expect("write");
        assert!(!tmp_dir.exists());
    }

    #[test]
    fn write_memory_files_documents_rename_limit() {
        // Verify the function handles concurrent writes gracefully
        let dir = tempfile::tempdir().unwrap();
        write_memory_files(dir.path(), "agent", &[("semantic.md", "s")]).expect("first write");
        write_memory_files(dir.path(), "agent", &[("semantic.md", "s")]).expect("second write");
    }

    // --- retry integration ---

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
    }

    #[tokio::test]
    async fn run_sleep_batch_retries_on_invalid_json_then_succeeds() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let good_response = serde_json::json!({
            "semantic": "retried",
            "prospective": "retried"
        })
        .to_string();
        let provider = SequentialMockProvider::new(vec![
            r#"{"not":"valid events"}"#.to_string(),
            r#"{"not":"valid events"}"#.to_string(),
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

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, SleepRunStatus::Failed);
    }

    #[tokio::test]
    async fn run_sleep_batch_logs_llm_usage_with_sleep_batch_request_kind() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let llm = Arc::new(MockLlmProvider::with_usage(100, 200));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
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
                }),
            })
        }
    }

    #[tokio::test]
    async fn run_sleep_batch_extracts_events_before_memory_update() {
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
            (r#"{"semantic":"","prospective":""}"#.to_string(), 50, 50),
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
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
            "semantic": "",
            "prospective": ""
        })
        .to_string();
        let call2_response = r#"{"rollups":[]}"#.to_string();

        let provider = SequentialMockWithUsage::new(vec![
            (events_response, 100, 100),
            (call2_response, 50, 50),
            (memory_response, 100, 100),
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
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
            "semantic": "updated",
            "prospective": "updated"
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

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
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
            r#"{"semantic":"updated","prospective":"updated"}"#.to_string(),
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        let steps = state.db.list_sleep_run_steps(&runs[0].id).expect("steps");

        let episodic = steps
            .iter()
            .find(|s| s.step_name == SleepStepName::EpisodicUpdate)
            .expect("episodic step exists");
        let semantic = steps
            .iter()
            .find(|s| s.step_name == SleepStepName::SemanticUpdate)
            .expect("semantic step exists");
        let prospective = steps
            .iter()
            .find(|s| s.step_name == SleepStepName::ProspectiveUpdate)
            .expect("prospective step exists");

        assert_eq!(episodic.status, SleepStepStatus::Success);
        assert_eq!(semantic.status, SleepStepStatus::Success);
        assert_eq!(prospective.status, SleepStepStatus::Success);
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
        let call2_response = r#"{"rollups":[]}"#.to_string();
        let memory_response = serde_json::json!({
            "semantic": "s",
            "prospective": "p"
        })
        .to_string();

        let provider = SequentialMockWithUsage::new(vec![
            (events_response, 50, 30),
            (call2_response, 40, 20),
            (memory_response, 60, 40),
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert!(runs[0].input_tokens > 0);
        assert!(runs[0].output_tokens > 0);
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

    // -----------------------------------------------------------------------
    // collect_sleep_input tests
    // -----------------------------------------------------------------------

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
                assert!(reason.contains("threshold"));
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn collect_returns_skip_when_below_threshold() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=16 {
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
                assert_eq!(new_message_count, 16);
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn collect_returns_proceed_above_threshold() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=17 {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
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

        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
            .expect("save session");

        let after_cutoff = chrono::Utc::now().to_rfc3339();
        for i in 1..=17 {
            store_msg(&db, &format!("new-{i}"), chat_id, "new", &after_cutoff);
        }

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        assert!(
            matches!(result, InputDecision::Proceed { .. }),
            "17 new messages (> 16 threshold) should trigger Proceed"
        );
    }

    #[test]
    fn collect_first_run_no_previous_run() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=17 {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
            .expect("save session");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        assert!(
            matches!(result, InputDecision::Proceed { .. }),
            "17 messages with no previous run should trigger Proceed"
        );
    }

    #[test]
    fn collect_respects_max_sessions_limit() {
        let (db, _dir) = test_db();
        for i in 0..25 {
            let cid = create_chat(&db, "test-agent", &format!("-{i}"));
            db.save_session(cid, r#"[{"role":"user","content":"hi"}]"#)
                .expect("save session");
            store_msg(&db, &format!("m{i}"), cid, "hi", "2025-06-01T00:00:00Z");
        }

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
        for i in 1..=17 {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
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
    fn collect_source_chats_json_sorted_newest_first() {
        let (db, _dir) = test_db();
        for i in 0..17 {
            let cid = create_chat(&db, "test-agent", &format!("-{i}"));
            store_msg(&db, &format!("m{i}"), cid, "hi", "2025-06-01T00:00:00Z");
            db.save_session(cid, r#"[{"role":"user","content":"hi"}]"#)
                .expect("save session");
        }

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        let json_str = match &result {
            InputDecision::Proceed {
                source_chats_json, ..
            } => source_chats_json,
            other => panic!("expected Proceed, got {other:?}"),
        };

        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(json_str).expect("valid JSON array");
        assert!(
            parsed.len() >= 2,
            "need at least 2 entries to check ordering"
        );

        let timestamps: Vec<String> = parsed
            .iter()
            .map(|v| v["updated_at"].as_str().unwrap_or("").to_string())
            .collect();

        for i in 0..timestamps.len() - 1 {
            assert!(
                timestamps[i] >= timestamps[i + 1],
                "expected newest first: {i}='{}' < {j}='{}'",
                timestamps[i],
                timestamps[i + 1],
                j = i + 1,
            );
        }
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
                    reason.contains("threshold"),
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
            r#"{"events":[]}"#.to_string(),
            r#"{"events":[]}"#.to_string(),
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

        let runs_after_first = state.db.list_sleep_runs("test-agent", 10).expect("list");
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

        let all_success_second = vec![
            r#"{"rollups":[]}"#.to_string(),
            r#"{"semantic":"second","prospective":"second"}"#.to_string(),
        ];
        let llm2 = Arc::new(SequentialMockProvider::new(all_success_second));
        let config = crate::test_util::test_config(&dir.path().to_string_lossy());
        let state2 = crate::test_util::build_state_with_config(
            config,
            Some(llm2),
            None,
            Some(Arc::clone(&state.db)),
            None,
        );

        run_sleep_batch(&state2, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("second batch");

        let runs_after_second = state2.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs_after_second.len(), 2);
        assert_eq!(runs_after_second[0].status, SleepRunStatus::Success);
    }
}

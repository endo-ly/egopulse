//! Sleep batch orchestrator — coordinates Call 1 (extract), Call 2 (rollup), and Call 3 (memory update).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tracing::{info, warn};

use crate::agent_loop::compaction::archive_conversation_blocking;
use crate::llm::{LlmProvider, Message};
use crate::memory::{MemoryContent, collect_sleep_input};
use crate::runtime::AppState;
use crate::storage::{
    AgentSessionInfo, Database, EpisodeEvent, MemoryFile, RollupGranularity, SleepRunTrigger,
    call_blocking,
};

use super::episodic_renderer;
use super::extract::{self, ExtractedEvent};
use super::memory_update;
use super::rollup;

#[derive(Debug, Error)]
pub enum SleepBatchError {
    #[error("already running for agent '{agent_id}'")]
    AlreadyRunning { agent_id: String },
    #[error(transparent)]
    Storage(#[from] crate::error::StorageError),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("parse failed: {0}")]
    ParseFailed(String),
    #[error("context overflow for agent '{agent_id}'")]
    ContextOverflow { agent_id: String },
    #[error("I/O error: {0}")]
    Io(String),
    #[error("unsafe agent_id: {0}")]
    UnsafeAgentId(String),
    #[error("LLM error: {0}")]
    Llm(String),
}

fn parse_timezone_offset_seconds(tz_str: &str) -> i32 {
    if let Some(offset_part) = tz_str.strip_prefix("UTC") {
        if let Ok(seconds) = parse_hhmm_offset(offset_part) {
            return seconds;
        }
    }
    match tz_str {
        "Asia/Tokyo" => 9 * 3600,
        "UTC" | "Z" => 0,
        _ => 9 * 3600,
    }
}

fn parse_hhmm_offset(s: &str) -> Result<i32, ()> {
    let s = s.trim();
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let s = s.trim_start_matches(['+', '-']);
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 2 {
        let hours: i32 = parts[0].parse().map_err(|_| ())?;
        let minutes: i32 = parts[1].parse().map_err(|_| ())?;
        Ok(sign * (hours * 60 + minutes) * 60)
    } else if s.len() >= 2 {
        let hours: i32 = s[..2].parse().map_err(|_| ())?;
        let minutes: i32 = if s.len() >= 4 {
            s[2..4].parse().map_err(|_| ())?
        } else {
            0
        };
        Ok(sign * (hours * 60 + minutes) * 60)
    } else {
        Err(())
    }
}

fn extract_date_only(dt_str: &str) -> String {
    dt_str.get(..10).unwrap_or(dt_str).to_string()
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
        crate::memory::InputDecision::Skip {
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
        crate::memory::InputDecision::Proceed {
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

async fn execute_batch(
    state: &AppState,
    db: Arc<Database>,
    agent_id: &str,
    sessions: &[AgentSessionInfo],
    source_chats_json: &str,
    trigger: SleepRunTrigger,
) -> Result<(), SleepBatchError> {
    let agent_for_run = agent_id.to_string();
    let run_id = call_blocking(Arc::clone(&db), move |db| {
        db.try_create_sleep_run(&agent_for_run, trigger)
    })
    .await?;

    let run_id = match run_id {
        Some(id) => id,
        None => {
            return Err(SleepBatchError::AlreadyRunning {
                agent_id: agent_id.to_string(),
            });
        }
    };

    let result = async {
        let agents_dir = PathBuf::from(&state.config.state_root).join("agents");
        recover_memory_write(&agents_dir, agent_id)?;

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
        let session_chunks =
            memory_update::build_session_text_chunks(&db, sessions, chunk_session_tokens)?;

        let memory_before = state.memory_loader.load(agent_id);
        save_aggregate_snapshots(&db, &run_id, agent_id, memory_before.as_ref(), None).await?;

        // Call 1: Event Extraction (best-effort)
        let extract_result: Result<(Vec<ExtractedEvent>, i64, i64), SleepBatchError> = async {
            let total_chunks = session_chunks.len();
            extract::run_extract_events_for_chunks(
                &provider,
                agent_id,
                session_chunks.clone(),
                total_chunks,
            )
            .await
        }
        .await;

        let (mut input_tokens, mut output_tokens) = match extract_result {
            Ok((extracted_events, inp, out)) => {
                if !extracted_events.is_empty() {
                    let now = chrono::Utc::now().to_rfc3339();
                    let agent_for_events = agent_id.to_string();
                    let run_id_for_insert = run_id.clone();
                    let episode_events: Vec<EpisodeEvent> = extracted_events
                        .into_iter()
                        .map(|e| EpisodeEvent {
                            id: uuid::Uuid::new_v4().to_string(),
                            agent_id: agent_for_events.clone(),
                            experienced_at: e.experienced_at,
                            encoded_at: now.clone(),
                            kind: e.kind,
                            title: e.title,
                            body_md: e.body_md,
                            ripple_strength: e.ripple_strength,
                            certainty: e.certainty,
                            sleep_run_id: run_id_for_insert.clone(),
                            source_refs_json: None,
                            created_at: now.clone(),
                            updated_at: now.clone(),
                        })
                        .collect();
                    let event_count = episode_events.len();
                    call_blocking(Arc::clone(&db), move |db| {
                        db.insert_episode_events(&run_id_for_insert, &episode_events)
                    })
                    .await?;
                    info!(count = event_count, "extracted episode events");
                }
                (inp, out)
            }
            Err(e) => {
                warn!(error = %e, "event extraction failed, continuing with memory update");
                (0, 0)
            }
        };

        let mut current_memory = memory_before.unwrap_or_default();

        // Call 2: Episodic View Materialization (best-effort)
        let rendered_episodic;
        {
            let tz_str = &state.config.timezone;
            let tz_seconds: i32 = parse_timezone_offset_seconds(tz_str);
            let tz = chrono::FixedOffset::east_opt(tz_seconds)
                .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("UTC+0 is valid"));
            let now = chrono::Utc::now().with_timezone(&tz);

            let cw = rollup::current_week(now);

            let cw_start = cw.period_start.to_rfc3339();
            let cw_end = cw.period_end_exclusive.to_rfc3339();

            let agent_for_events = agent_id.to_string();
            let current_week_events: Vec<EpisodeEvent> =
                match call_blocking(Arc::clone(&db), move |db| {
                    db.list_episode_events_in_range(&agent_for_events, &cw_start, &cw_end)
                })
                .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        warn!(error = %e, "Call2: failed to load current week events");
                        Vec::new()
                    }
                };

            let call2_llm_result: Result<(), SleepBatchError> = async {
                let agent_for_plan = agent_id.to_string();
                let existing_week_rollups: Vec<rollup::ExistingRollupInfo> =
                    call_blocking(Arc::clone(&db), move |db| {
                        db.list_episode_rollups(&agent_for_plan, RollupGranularity::Week, 100)
                    })
                    .await?
                    .into_iter()
                    .map(|r| rollup::ExistingRollupInfo {
                        period_key: r.period_key,
                        event_count: r.event_count,
                        max_ripple: r.max_ripple,
                        summary_md: r.summary_md,
                    })
                    .collect();

                let agent_for_months = agent_id.to_string();
                let existing_month_rollups: Vec<rollup::ExistingRollupInfo> =
                    call_blocking(Arc::clone(&db), move |db| {
                        db.list_episode_rollups(&agent_for_months, RollupGranularity::Month, 100)
                    })
                    .await?
                    .into_iter()
                    .map(|r| rollup::ExistingRollupInfo {
                        period_key: r.period_key,
                        event_count: r.event_count,
                        max_ripple: r.max_ripple,
                        summary_md: r.summary_md,
                    })
                    .collect();

                let recent = rollup::recent_weeks(now, 4);
                let earliest_start = recent
                    .last()
                    .map(|w| w.period_start.to_rfc3339())
                    .unwrap_or_else(|| cw.period_start.to_rfc3339());
                let agent_for_all = agent_id.to_string();
                let all_events: Vec<EpisodeEvent> = call_blocking(Arc::clone(&db), move |db| {
                    db.list_episode_events_in_range(
                        &agent_for_all,
                        &earliest_start,
                        &cw.period_end_exclusive.to_rfc3339(),
                    )
                })
                .await?;

                let planner_events: Vec<rollup::PlannerEvent> = all_events
                    .iter()
                    .map(|e| rollup::PlannerEvent {
                        experienced_at: e.experienced_at.clone(),
                        encoded_at: e.encoded_at.clone(),
                        ripple_strength: e.ripple_strength,
                    })
                    .collect();

                let planner_input = rollup::RollupPlannerInput {
                    existing_week_rollups,
                    existing_month_rollups,
                    events: planner_events,
                };

                let rollup_requests = rollup::plan_rollup_updates(agent_id, now, &planner_input);

                if !rollup_requests.is_empty() {
                    let mut events_map: HashMap<String, Vec<rollup::Call2Event>> = HashMap::new();
                    for req in &rollup_requests {
                        let req_start = req.period_start.clone();
                        let req_end = req.period_end_exclusive.clone();
                        let req_key = req.period_key.clone();
                        let agent_for_range = agent_id.to_string();
                        let period_events: Vec<EpisodeEvent> =
                            call_blocking(Arc::clone(&db), move |db| {
                                db.list_episode_events_in_range(
                                    &agent_for_range,
                                    &req_start,
                                    &req_end,
                                )
                            })
                            .await?;

                        let call2_events: Vec<rollup::Call2Event> = period_events
                            .iter()
                            .map(|e| rollup::Call2Event {
                                id: e.id.clone(),
                                experienced_at: e.experienced_at.clone(),
                                kind: e.kind.to_string(),
                                title: e.title.clone(),
                                body_md: e.body_md.clone(),
                                ripple_strength: e.ripple_strength,
                                certainty: e.certainty.to_string(),
                            })
                            .collect();
                        events_map.insert(req_key, call2_events);
                    }

                    let input = rollup::build_call2_input(
                        agent_id,
                        &run_id,
                        &now,
                        tz_str,
                        &cw,
                        &rollup_requests,
                        &events_map,
                    );
                    let input_json = serde_json::to_string_pretty(&input)
                        .map_err(|e| SleepBatchError::Internal(e.to_string()))?;
                    let input_json = rollup::redact_secrets(&input_json);

                    let system_prompt = rollup::build_call2_system_prompt(agent_id);
                    let user_prompt = rollup::build_call2_user_prompt(&input_json);
                    let user_message = Message::text("user", user_prompt);
                    let response = provider
                        .send_message(&system_prompt, Arc::new(vec![user_message]), None)
                        .await
                        .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

                    input_tokens = input_tokens
                        .saturating_add(response.usage.as_ref().map_or(0, |u| u.input_tokens));
                    output_tokens = output_tokens
                        .saturating_add(response.usage.as_ref().map_or(0, |u| u.output_tokens));

                    let output_json = rollup::redact_secrets(&response.content);
                    let valid_keys: std::collections::HashSet<String> = rollup_requests
                        .iter()
                        .map(|r| r.period_key.clone())
                        .collect();
                    let rollup_outputs = rollup::parse_call2_output(&output_json, &valid_keys)
                        .map_err(|e| SleepBatchError::ParseFailed(e.to_string()))?;

                    let requests_by_key: std::collections::HashMap<&str, &rollup::RollupRequest> =
                        rollup_requests
                            .iter()
                            .map(|r| (r.period_key.as_str(), r))
                            .collect();

                    for rollup_output in &rollup_outputs {
                        let Some(request) = requests_by_key.get(rollup_output.period_key.as_str())
                        else {
                            continue;
                        };
                        let granularity = match rollup_output.granularity.as_str() {
                            "week" => RollupGranularity::Week,
                            "month" => RollupGranularity::Month,
                            _ => continue,
                        };
                        let rollup = crate::storage::EpisodeRollup {
                            id: uuid::Uuid::new_v4().to_string(),
                            agent_id: agent_id.to_string(),
                            granularity,
                            period_key: rollup_output.period_key.clone(),
                            period_start: request.period_start.clone(),
                            period_end_exclusive: request.period_end_exclusive.clone(),
                            summary_md: rollup_output.summary_md.clone(),
                            max_ripple: rollup_output.max_ripple,
                            event_count: rollup_output.event_count,
                            generated_run_id: run_id.clone(),
                            created_at: now.to_rfc3339(),
                            updated_at: now.to_rfc3339(),
                        };
                        let rollup_for_db = rollup.clone();
                        call_blocking(Arc::clone(&db), move |db| {
                            db.upsert_episode_rollup(&rollup_for_db)
                        })
                        .await?;
                    }
                }

                Ok(())
            }
            .await;

            if let Err(e) = call2_llm_result {
                warn!(error = %e, "Call2 rollup generation failed (best-effort, continuing)");
            }

            // Episodic Renderer
            let renderer_events: Vec<episodic_renderer::RendererEvent> = current_week_events
                .iter()
                .map(|e| episodic_renderer::RendererEvent {
                    experienced_at: e.experienced_at.clone(),
                    kind: e.kind.to_string(),
                    title: e.title.clone(),
                    body_md: e.body_md.clone(),
                    ripple_strength: e.ripple_strength,
                })
                .collect();

            let agent_for_rw = agent_id.to_string();
            let recent_week_rollups: Vec<crate::storage::EpisodeRollup> =
                call_blocking(Arc::clone(&db), move |db| {
                    db.list_episode_rollups(&agent_for_rw, RollupGranularity::Week, 4)
                })
                .await
                .unwrap_or_default();
            let rw_renderer: Vec<episodic_renderer::RendererRollup> = recent_week_rollups
                .iter()
                .map(|r| episodic_renderer::RendererRollup {
                    period_key: r.period_key.clone(),
                    period_start: r.period_start.clone(),
                    period_end_exclusive: r.period_end_exclusive.clone(),
                    summary_md: r.summary_md.clone(),
                    max_ripple: r.max_ripple,
                    granularity: r.granularity,
                })
                .collect();

            let agent_for_rm = agent_id.to_string();
            let recent_month_rollups: Vec<crate::storage::EpisodeRollup> =
                call_blocking(Arc::clone(&db), move |db| {
                    db.list_episode_rollups(&agent_for_rm, RollupGranularity::Month, 2)
                })
                .await
                .unwrap_or_default();
            let rm_renderer: Vec<episodic_renderer::RendererRollup> = recent_month_rollups
                .iter()
                .map(|r| episodic_renderer::RendererRollup {
                    period_key: r.period_key.clone(),
                    period_start: r.period_start.clone(),
                    period_end_exclusive: r.period_end_exclusive.clone(),
                    summary_md: r.summary_md.clone(),
                    max_ripple: r.max_ripple,
                    granularity: r.granularity,
                })
                .collect();

            let before_period = cw.period_start.to_rfc3339();
            let agent_for_bg = agent_id.to_string();
            let background_rollups: Vec<crate::storage::EpisodeRollup> =
                call_blocking(Arc::clone(&db), move |db| {
                    db.list_background_episode_rollups(&agent_for_bg, 4, &before_period)
                })
                .await
                .unwrap_or_default();
            let bg_renderer: Vec<episodic_renderer::RendererRollup> = background_rollups
                .iter()
                .map(|r| episodic_renderer::RendererRollup {
                    period_key: r.period_key.clone(),
                    period_start: r.period_start.clone(),
                    period_end_exclusive: r.period_end_exclusive.clone(),
                    summary_md: r.summary_md.clone(),
                    max_ripple: r.max_ripple,
                    granularity: r.granularity,
                })
                .collect();

            let ctx = episodic_renderer::WeekContext {
                now,
                tz_name: tz_str.clone(),
                week_key: cw.week_key.clone(),
                week_start: extract_date_only(&cw.period_start.to_rfc3339()),
                week_end: extract_date_only(&cw.period_end_exclusive.to_rfc3339()),
            };

            let episodic_md = episodic_renderer::render_episodic_md(
                &ctx,
                &renderer_events,
                &rw_renderer,
                &rm_renderer,
                &bg_renderer,
            );

            current_memory.episodic = Some(episodic_md.clone());
            rendered_episodic = Some(episodic_md);
        }

        // Call 3: Memory Update (semantic + prospective)
        let mut final_output = None;
        let total_chunks = session_chunks.len();

        for (index, sessions_text) in session_chunks.into_iter().enumerate() {
            let input = memory_update::build_sleep_input_from_parts(
                agent_id,
                current_memory.clone(),
                sessions_text,
                source_chats_json.to_string(),
                context_tokens,
                0,
            )?;
            let system_prompt = memory_update::build_sleep_system_prompt(&input);
            let (output, in_tok, out_tok) = memory_update::send_sleep_request(
                &provider,
                agent_id,
                &system_prompt,
                index + 1,
                total_chunks,
            )
            .await?;

            input_tokens = input_tokens.saturating_add(in_tok);
            output_tokens = output_tokens.saturating_add(out_tok);
            current_memory =
                memory_content_from_output(&output, current_memory.episodic.as_deref());
            final_output = Some(output);
        }

        let mut output = final_output.ok_or_else(|| {
            SleepBatchError::Internal("sleep batch produced no output".to_string())
        })?;

        if let Some(ref md) = rendered_episodic {
            output.episodic = md.clone();
        }

        // Write memory files
        write_memory_files(&agents_dir, agent_id, &output)?;

        // Archive sessions
        let groups_dir = state.config.groups_dir();
        let secrets = crate::tools::collect_config_secrets(&state.config);
        for session in sessions {
            if let Err(e) = archive_and_clear_session(&db, &groups_dir, session, &secrets) {
                warn!(
                    agent_id = %agent_id,
                    chat_id = session.chat_id,
                    error = %e,
                    "failed to archive/clear session (continuing)"
                );
            }
        }

        // Save AFTER snapshots
        save_output_snapshots(&db, &run_id, agent_id, &output).await?;

        // Log LLM usage
        if input_tokens > 0 || output_tokens > 0 {
            let provider_name = provider.provider_name().to_string();
            let model_name = provider.model_name().to_string();
            crate::runtime::metrics::inc_llm_tokens_total("input", &provider_name, input_tokens);
            crate::runtime::metrics::inc_llm_tokens_total("output", &provider_name, output_tokens);
            let db_for_usage = Arc::clone(&db);
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

        // Update run success
        let run_id_owned = run_id.clone();
        let source_chats = source_chats_json.to_string();
        call_blocking(Arc::clone(&db), move |db| {
            db.update_sleep_run_success(
                &run_id_owned,
                &source_chats,
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

fn memory_content_from_output(
    output: &memory_update::SleepBatchOutput,
    existing_episodic: Option<&str>,
) -> MemoryContent {
    MemoryContent {
        episodic: existing_episodic
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                if output.episodic.is_empty() {
                    None
                } else {
                    Some(output.episodic.clone())
                }
            }),
        semantic: Some(output.semantic.clone()).filter(|s| !s.is_empty()),
        prospective: Some(output.prospective.clone()).filter(|s| !s.is_empty()),
    }
}

async fn save_aggregate_snapshots(
    db: &Arc<Database>,
    run_id: &str,
    agent_id: &str,
    memory: Option<&MemoryContent>,
    is_after: Option<bool>,
) -> Result<(), SleepBatchError> {
    let Some(content) = memory else {
        return Ok(());
    };

    let entries: Vec<(MemoryFile, String)> = [
        (MemoryFile::Episodic, &content.episodic),
        (MemoryFile::Semantic, &content.semantic),
        (MemoryFile::Prospective, &content.prospective),
    ]
    .into_iter()
    .filter_map(|(file, maybe)| maybe.as_ref().map(|c| (file, c.clone())))
    .collect();

    for (file, file_content) in entries {
        match is_after {
            Some(true) => {
                let run = run_id.to_string();
                let agent = agent_id.to_string();
                call_blocking(Arc::clone(db), move |db| {
                    db.update_memory_snapshot_after(&run, &agent, file, &file_content)
                })
                .await?;
            }
            _ => {
                let run = run_id.to_string();
                let agent = agent_id.to_string();
                let before = file_content.clone();
                let after = file_content.clone();
                call_blocking(Arc::clone(db), move |db| {
                    db.create_memory_snapshot(&run, &agent, file, &before, &after)
                })
                .await?;
            }
        }
    }

    Ok(())
}

async fn save_output_snapshots(
    db: &Arc<Database>,
    run_id: &str,
    agent_id: &str,
    output: &memory_update::SleepBatchOutput,
) -> Result<(), SleepBatchError> {
    let content = memory_content_from_output(output, None);
    save_aggregate_snapshots(db, run_id, agent_id, Some(&content), Some(true)).await
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
    output: &memory_update::SleepBatchOutput,
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
        std::fs::write(tmp_dir.join("episodic.md"), &output.episodic)
            .map_err(|e| SleepBatchError::Io(format!("failed to write episodic.md: {e}")))?;
        std::fs::write(tmp_dir.join("semantic.md"), &output.semantic)
            .map_err(|e| SleepBatchError::Io(format!("failed to write semantic.md: {e}")))?;
        std::fs::write(tmp_dir.join("prospective.md"), &output.prospective)
            .map_err(|e| SleepBatchError::Io(format!("failed to write prospective.md: {e}")))?;
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
        for i in 1..=6 {
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
        let llm = Arc::new(MockLlmProvider::new());
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

        let llm = Arc::new(MockLlmProvider::with_response(serde_json::json!({
            "semantic": "# Semantic\n\n- fact",
            "prospective": "# Prospective\n\n- todo"
        })));
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
        let llm = Arc::new(MockLlmProvider::new());
        let state = build_test_state_with_llm(db, dir.path(), llm);

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Success);
    }

    #[tokio::test]
    async fn run_sleep_batch_marks_failed_on_error() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let llm = Arc::new(MockLlmProvider::with_response(
            serde_json::json!({"invalid": true}),
        ));
        let state = build_test_state_with_llm(db, dir.path(), llm);

        let err = run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect_err("should fail");
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
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
        let llm = Arc::new(MockLlmProvider::new());
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
        let llm = Arc::new(MockLlmProvider::with_response(serde_json::json!({
            "semantic": "s",
            "prospective": "p"
        })));
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
    fn write_memory_files_writes_all_three_files() {
        let dir = tempfile::tempdir().unwrap();
        let output = memory_update::SleepBatchOutput {
            episodic: "ep".to_string(),
            semantic: "sem".to_string(),
            prospective: "pro".to_string(),
        };
        write_memory_files(dir.path(), "agent", &output).expect("write");

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
        let output = memory_update::SleepBatchOutput {
            episodic: String::new(),
            semantic: "s".to_string(),
            prospective: String::new(),
        };
        write_memory_files(dir.path(), "agent", &output).expect("write");

        let memory_dir = dir.path().join("agent").join("memory");
        assert!(memory_dir.exists());
    }

    #[test]
    fn write_memory_files_rejects_unsafe_agent_id() {
        let dir = tempfile::tempdir().unwrap();
        let output = memory_update::SleepBatchOutput {
            episodic: String::new(),
            semantic: String::new(),
            prospective: String::new(),
        };
        let err = write_memory_files(dir.path(), "../etc", &output).expect_err("should reject");
        assert!(matches!(err, SleepBatchError::UnsafeAgentId(_)));
    }

    #[test]
    fn write_memory_files_preserves_existing_on_write_error() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent").join("memory");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("semantic.md"), "old").unwrap();

        // This should succeed — we're writing valid content
        let output = memory_update::SleepBatchOutput {
            episodic: String::new(),
            semantic: "new".to_string(),
            prospective: String::new(),
        };
        write_memory_files(dir.path(), "agent", &output).expect("write");
    }

    #[test]
    fn write_memory_files_recovers_backup_on_start() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let backup_dir = agent_dir.join("memory.backup-test");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("semantic.md"), "recovered").unwrap();

        let output = memory_update::SleepBatchOutput {
            episodic: String::new(),
            semantic: "new".to_string(),
            prospective: String::new(),
        };
        write_memory_files(dir.path(), "agent", &output).expect("write");
    }

    #[test]
    fn write_memory_files_cleans_tmp_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let tmp_dir = agent_dir.join("memory.tmp-stale");
        std::fs::create_dir_all(&tmp_dir).unwrap();

        let output = memory_update::SleepBatchOutput {
            episodic: String::new(),
            semantic: "s".to_string(),
            prospective: String::new(),
        };
        write_memory_files(dir.path(), "agent", &output).expect("write");
        assert!(!tmp_dir.exists());
    }

    #[test]
    fn write_memory_files_documents_rename_limit() {
        // Verify the function handles concurrent writes gracefully
        let dir = tempfile::tempdir().unwrap();
        let output = memory_update::SleepBatchOutput {
            episodic: String::new(),
            semantic: "s".to_string(),
            prospective: String::new(),
        };
        write_memory_files(dir.path(), "agent", &output).expect("first write");
        write_memory_files(dir.path(), "agent", &output).expect("second write");
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

        let extract_response = r#"{"events":[]}"#.to_string();
        let call2_response = r#"{"rollups":[]}"#.to_string();
        let bad_response = r#"{"not":"valid keys"}"#.to_string();
        let good_response = serde_json::json!({
            "semantic": "retried",
            "prospective": "retried"
        })
        .to_string();
        let provider = SequentialMockProvider::new(vec![
            extract_response,
            call2_response,
            bad_response,
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
        let bad2 = r#"{"still_bad":2}"#.to_string();
        let call2_response = r#"{"rollups":[]}"#.to_string();
        let provider = SequentialMockProvider::new(vec![bad1, call2_response, bad2]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        let err = run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect_err("should fail");
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));

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
        let call2_response = r#"{"rollups":[]}"#.to_string();
        let memory_response = serde_json::json!({
            "semantic": "",
            "prospective": ""
        })
        .to_string();

        let provider = SequentialMockWithUsage::new(vec![
            (events_response, 50, 50),
            (call2_response, 50, 50),
            (memory_response, 50, 50),
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

        let call2_response = r#"{"rollups":[]}"#.to_string();
        let memory_response = serde_json::json!({
            "semantic": "updated",
            "prospective": "updated"
        })
        .to_string();

        let provider = SequentialMockProvider::new(vec![
            r#"{"not_events":[]}"#.to_string(),
            r#"{"not_events":[]}"#.to_string(),
            call2_response,
            memory_response,
        ]);
        let state = build_test_state_with_llm(db, dir.path(), Arc::new(provider));

        run_sleep_batch(&state, Some("test-agent"), SleepRunTrigger::Manual)
            .await
            .expect("batch should continue despite extract failure");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs[0].status, SleepRunStatus::Success);

        let events = state
            .db
            .list_episode_events_by_run(&runs[0].id)
            .expect("events");
        assert!(events.is_empty());
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
}

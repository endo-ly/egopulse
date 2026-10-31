//! Agent screen API endpoints: effective profile, prompt files, memory change
//! history, and the skill catalog.

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use super::agents::resolved_agent_id;
use super::{ApiError, ApiResult, WebState, api_error, bad_request, internal_error, not_found};
use crate::agent_loop::prompt::{PromptFileKind, PromptFileLayer, PromptFileScope};
use crate::config::{AgentId, Config, LlmSettingSource};
use crate::skills::{
    SkillFileError, SkillManager, SkillMetadata, list_skill_files, read_skill_file,
};

const DEFAULT_HISTORY_LIMIT: usize = 20;
const MAX_HISTORY_LIMIT: usize = 100;

#[derive(Serialize)]
struct LlmRow {
    /// `None` for channels without a profile.
    channel: Option<String>,
    provider: String,
    model: String,
    provider_source: &'static str,
    model_source: &'static str,
}

#[derive(Serialize)]
struct PromptSource {
    scope: &'static str,
    path: String,
    chars: usize,
}

#[derive(Serialize)]
struct LayerBody {
    scope: &'static str,
    path: String,
    content: Option<String>,
    applied: bool,
}

#[derive(Serialize)]
struct SkillSummary {
    name: String,
    description: String,
    source: &'static str,
}

#[derive(Serialize)]
struct SkillDetail {
    #[serde(flatten)]
    summary: SkillSummary,
    /// Skill directory relative to the state root.
    path: String,
    platforms: Vec<String>,
    deps: Vec<String>,
    required_env: Vec<String>,
    instructions: String,
    files: Vec<SkillFileEntry>,
}

#[derive(Serialize)]
struct SkillFileEntry {
    path: String,
    bytes: u64,
}

#[derive(Deserialize)]
pub(super) struct HistoryQuery {
    limit: Option<usize>,
}

/// Returns the agent's effective LLM settings, bindings, and prompt sources.
pub(super) async fn get_agent_profile(
    State(state): State<WebState>,
    UrlPath(agent_id): UrlPath<String>,
) -> ApiResult {
    let agent_id =
        resolved_agent_id(&state, &agent_id).ok_or_else(|| not_found("agent_not_found"))?;
    let snapshot = state.app_state.config_manager.current_blocking();
    let config = &snapshot.config;
    let agent = config
        .agents
        .get(&agent_id)
        .ok_or_else(|| not_found("agent_not_found"))?;

    let mut profile_channels: Vec<&String> = agent.profiles.keys().collect();
    profile_channels.sort();
    let mut llm = vec![resolve_llm_row(config, &agent_id, None)?];
    for channel in profile_channels {
        llm.push(resolve_llm_row(config, &agent_id, Some(channel))?);
    }

    let app_state = Arc::clone(&state.app_state);
    let id = agent_id.as_str().to_string();
    let prompt_sources = blocking(move || -> Result<serde_json::Value, ApiError> {
        let state_root = Path::new(&app_state.config.state_root);
        let applied_sources = |kind| -> Result<Vec<PromptSource>, ApiError> {
            let layers = app_state
                .soul_agents
                .prompt_file_layers(&id, kind)
                .ok_or_else(|| not_found("agent_not_found"))?;
            Ok(layers
                .into_iter()
                .filter(|layer| layer.applied)
                .map(|layer| PromptSource {
                    scope: scope_name(layer.scope),
                    path: display_path(state_root, &layer.path),
                    chars: layer.content.map_or(0, |content| content.chars().count()),
                })
                .collect())
        };
        let memory = app_state.memory_loader.load_bundle(&id).map_err(|error| {
            tracing::warn!(%error, agent_id = %id, "failed to load memory bundle");
            internal_error()
        })?;
        let chars = |text: &str| text.chars().count();
        // Only the location and presence of SECRET.md are exposed here; its
        // content is fetched separately on explicit request.
        let secret_path = display_path(
            state_root,
            &app_state
                .soul_agents
                .prompt_file_layers(&id, PromptFileKind::Secret)
                .and_then(|layers| layers.into_iter().next())
                .ok_or_else(|| not_found("agent_not_found"))?
                .path,
        );
        Ok(serde_json::json!({
            "soul": applied_sources(PromptFileKind::Soul)?.into_iter().next(),
            "rules": applied_sources(PromptFileKind::Rules)?,
            "secret": {
                "path": secret_path,
                "exists": !applied_sources(PromptFileKind::Secret)?.is_empty(),
            },
            "memory": {
                "episodic": chars(&memory.episodic),
                "semantic": chars(&memory.semantic),
                "prospective": chars(&memory.prospective),
            },
            "skills": app_state.skills.discover_skills().len(),
        }))
    })
    .await??;

    Ok(Json(serde_json::json!({
        "ok": true,
        "profile": {
            "id": agent_id.as_str(),
            "label": agent.label,
            "is_default": agent_id == config.default_agent,
            "reasoning_effort": agent.reasoning_effort,
            "llm": llm,
            "bindings": {
                "discord_bot": agent.discord_bot.as_ref().map(ToString::to_string),
                "telegram_bot": agent.telegram_bot.as_ref().map(ToString::to_string),
            },
            "prompt_sources": prompt_sources,
        }
    })))
}

/// Returns every file layer of one prompt file kind (`soul` / `rules` / `secret`).
pub(super) async fn get_agent_prompt_file(
    State(state): State<WebState>,
    UrlPath((agent_id, kind)): UrlPath<(String, String)>,
) -> ApiResult {
    let agent_id =
        resolved_agent_id(&state, &agent_id).ok_or_else(|| not_found("agent_not_found"))?;
    let kind =
        parse_prompt_file_kind(&kind).ok_or_else(|| bad_request("invalid_prompt_file_kind"))?;

    let app_state = Arc::clone(&state.app_state);
    let layers = blocking(move || {
        app_state
            .soul_agents
            .prompt_file_layers(agent_id.as_str(), kind)
            .map(|layers| {
                let state_root = Path::new(&app_state.config.state_root);
                layers
                    .into_iter()
                    .map(|layer| layer_body(state_root, layer))
                    .collect::<Vec<_>>()
            })
    })
    .await?
    .ok_or_else(|| not_found("agent_not_found"))?;

    Ok(Json(serde_json::json!({ "ok": true, "layers": layers })))
}

/// Returns published changes to the agent's memory files, newest first.
pub(super) async fn get_agent_memory_history(
    State(state): State<WebState>,
    UrlPath(agent_id): UrlPath<String>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult {
    let agent_id =
        resolved_agent_id(&state, &agent_id).ok_or_else(|| not_found("agent_not_found"))?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .clamp(1, MAX_HISTORY_LIMIT);

    let db = Arc::clone(&state.app_state.db);
    let changes = blocking(move || db.list_memory_changes(agent_id.as_str(), limit))
        .await?
        .map_err(|error| {
            tracing::warn!(%error, "failed to list memory changes");
            internal_error()
        })?;

    let changes: Vec<_> = changes
        .into_iter()
        .map(|change| {
            serde_json::json!({
                "run_id": change.run_id,
                "file": change.file.to_string(),
                "created_at": change.created_at,
                "content_before": change.content_before,
                "content_after": change.content_after,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "ok": true, "changes": changes })))
}

/// Lists the skills available to every agent.
pub(super) async fn list_skills(State(state): State<WebState>) -> ApiResult {
    let skills = Arc::clone(&state.app_state.skills);
    let summaries = blocking(move || {
        skills
            .discover_skills()
            .iter()
            .map(|meta| skill_summary(&skills, meta))
            .collect::<Vec<_>>()
    })
    .await?;
    Ok(Json(serde_json::json!({ "ok": true, "skills": summaries })))
}

/// Returns one skill's frontmatter, SKILL.md instructions, and bundled files.
pub(super) async fn get_skill(
    State(state): State<WebState>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult {
    let app_state = Arc::clone(&state.app_state);
    let skill = blocking(move || -> Result<SkillDetail, ApiError> {
        let skills = &app_state.skills;
        let loaded = skills.load_skill_checked(&name).map_err(|error| {
            tracing::debug!(skill = %name, %error, "skill is not loadable");
            not_found("skill_not_found")
        })?;
        let files = list_skill_files(&loaded.metadata.dir_path).map_err(|error| {
            tracing::warn!(%error, skill = %name, "failed to list skill files");
            internal_error()
        })?;
        let meta = loaded.metadata;
        Ok(SkillDetail {
            summary: skill_summary(skills, &meta),
            path: display_path(Path::new(&app_state.config.state_root), &meta.dir_path),
            platforms: meta.platforms,
            deps: meta.deps,
            required_env: meta.required_env,
            instructions: loaded.instructions,
            files: files
                .into_iter()
                .map(|file| SkillFileEntry {
                    path: file.path,
                    bytes: file.bytes,
                })
                .collect(),
        })
    })
    .await??;

    Ok(Json(serde_json::json!({ "ok": true, "skill": skill })))
}

/// Returns the text content of a file bundled with a skill.
pub(super) async fn get_skill_file(
    State(state): State<WebState>,
    UrlPath((name, file)): UrlPath<(String, String)>,
) -> ApiResult {
    let skills = Arc::clone(&state.app_state.skills);
    let content = blocking(move || -> Result<String, ApiError> {
        let meta = skills
            .discover_skills()
            .into_iter()
            .find(|meta| meta.name == name)
            .ok_or_else(|| not_found("skill_not_found"))?;
        read_skill_file(&meta.dir_path, &file).map_err(|error| match error {
            SkillFileError::NotFound => not_found("skill_file_not_found"),
            SkillFileError::TooLarge => {
                api_error(StatusCode::PAYLOAD_TOO_LARGE, "skill_file_too_large")
            }
            SkillFileError::NotText => {
                api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "skill_file_not_text")
            }
            SkillFileError::Io(error) => {
                tracing::warn!(%error, skill = %name, "failed to read skill file");
                internal_error()
            }
        })
    })
    .await??;

    Ok(Json(serde_json::json!({ "ok": true, "content": content })))
}

fn resolve_llm_row(
    config: &Config,
    agent_id: &AgentId,
    channel: Option<&String>,
) -> Result<LlmRow, ApiError> {
    let (resolved, origin) = config
        .resolve_llm_origin_for_agent(agent_id, channel.map(String::as_str))
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %agent_id, "failed to resolve agent llm");
            internal_error()
        })?;
    Ok(LlmRow {
        channel: channel.cloned(),
        provider: resolved.provider,
        model: resolved.model,
        provider_source: source_name(origin.provider),
        model_source: source_name(origin.model),
    })
}

fn skill_summary(skills: &SkillManager, meta: &SkillMetadata) -> SkillSummary {
    SkillSummary {
        name: meta.name.clone(),
        description: meta.description.clone(),
        source: if skills.is_builtin(meta) {
            "builtin"
        } else {
            "workspace"
        },
    }
}

fn layer_body(state_root: &Path, layer: PromptFileLayer) -> LayerBody {
    LayerBody {
        scope: scope_name(layer.scope),
        path: display_path(state_root, &layer.path),
        content: layer.content,
        applied: layer.applied,
    }
}

fn parse_prompt_file_kind(kind: &str) -> Option<PromptFileKind> {
    match kind {
        "soul" => Some(PromptFileKind::Soul),
        "rules" => Some(PromptFileKind::Rules),
        "secret" => Some(PromptFileKind::Secret),
        _ => None,
    }
}

fn scope_name(scope: PromptFileScope) -> &'static str {
    match scope {
        PromptFileScope::Global => "global",
        PromptFileScope::Agent => "agent",
    }
}

fn source_name(source: LlmSettingSource) -> &'static str {
    match source {
        LlmSettingSource::Profile => "profile",
        LlmSettingSource::Agent => "agent",
        LlmSettingSource::GlobalDefault => "global_default",
        LlmSettingSource::ProviderDefault => "provider_default",
    }
}

/// Shows paths under the state root relative to it, so responses do not leak
/// the host's absolute layout.
fn display_path(state_root: &Path, path: &Path) -> String {
    path.strip_prefix(state_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

async fn blocking<T, F>(task: F) -> Result<T, ApiError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(task).await.map_err(|error| {
        tracing::warn!(%error, "agent profile task panicked");
        internal_error()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::config::{AgentConfig, AgentProfileConfig};
    use crate::storage::Database;

    fn web_state(dir: &tempfile::TempDir) -> WebState {
        let state_root = dir.path().to_string_lossy().to_string();
        let mut config = crate::test_util::test_config(&state_root);
        config.agents.insert(
            AgentId::new("lyre"),
            AgentConfig {
                label: "Lyre".to_string(),
                model: Some("custom-model".to_string()),
                reasoning_effort: Some("medium".to_string()),
                profiles: HashMap::from([(
                    "voice".to_string(),
                    AgentProfileConfig {
                        provider: None,
                        model: Some("gpt-4.1-mini".to_string()),
                    },
                )]),
                ..Default::default()
            },
        );
        let app_state = crate::test_util::build_state_with_config(config, None, None, None, None);
        WebState {
            app_state: Arc::new(app_state),
            config_path: None,
            run_hub: super::super::RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn write(dir: &tempfile::TempDir, relative: &str, content: &str) {
        let path = dir.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, content).expect("write");
    }

    fn insert_change(db: &Database, run_id: &str, status: &str, before: &str, after: &str) {
        let conn = db.get_conn().expect("conn");
        conn.execute(
            "INSERT INTO sleep_runs (id, agent_id, status, trigger_type, started_at)
             VALUES (?1, 'lyre', ?2, 'scheduled', '2026-10-30T03:00:00+00:00')",
            rusqlite::params![run_id, status],
        )
        .expect("insert run");
        conn.execute(
            "INSERT INTO memory_snapshots
                (id, run_id, agent_id, file, content_before, content_after, created_at)
             VALUES (?1, ?1, 'lyre', 'semantic', ?2, ?3, '2026-10-30T03:05:00+00:00')",
            rusqlite::params![run_id, before, after],
        )
        .expect("insert snapshot");
    }

    fn status_of(result: ApiResult) -> StatusCode {
        result.expect_err("expected error").0
    }

    #[tokio::test]
    async fn profile_reports_effective_llm_and_prompt_sources() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);
        write(&dir, "SOUL.md", "Global soul");
        write(&dir, "agents/lyre/AGENTS.md", "Lyre rules");
        write(&dir, "agents/lyre/SECRET.md", "secret");
        write(&dir, "agents/lyre/memory/semantic.md", "abc");

        // Act
        let body = get_agent_profile(State(state), UrlPath("lyre".to_string()))
            .await
            .expect("profile")
            .0;

        // Assert
        let profile = &body["profile"];
        assert_eq!(profile["label"], "Lyre");
        assert_eq!(profile["is_default"], false);
        assert_eq!(profile["reasoning_effort"], "medium");
        assert_eq!(
            profile["llm"],
            serde_json::json!([
                {
                    "channel": null,
                    "provider": "openai",
                    "model": "custom-model",
                    "provider_source": "global_default",
                    "model_source": "agent",
                },
                {
                    "channel": "voice",
                    "provider": "openai",
                    "model": "gpt-4.1-mini",
                    "provider_source": "global_default",
                    "model_source": "profile",
                },
            ])
        );
        let sources = &profile["prompt_sources"];
        assert_eq!(
            sources["soul"],
            serde_json::json!({"scope": "global", "path": "SOUL.md", "chars": 11})
        );
        assert_eq!(
            sources["rules"],
            serde_json::json!([{"scope": "agent", "path": "agents/lyre/AGENTS.md", "chars": 10}])
        );
        assert_eq!(
            sources["secret"],
            serde_json::json!({"path": "agents/lyre/SECRET.md", "exists": true})
        );
        assert_eq!(sources["memory"]["semantic"], 3);
        assert_eq!(sources["memory"]["episodic"], 0);
    }

    #[tokio::test]
    async fn profile_returns_404_for_unknown_agent() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);

        // Act
        let result = get_agent_profile(State(state), UrlPath("ghost".to_string())).await;

        // Assert
        assert_eq!(status_of(result), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn prompt_file_returns_layers_with_state_relative_paths() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);
        write(&dir, "agents/lyre/SOUL.md", "Lyre soul");
        write(&dir, "SOUL.md", "Global soul");

        // Act
        let body = get_agent_prompt_file(
            State(state),
            UrlPath(("lyre".to_string(), "soul".to_string())),
        )
        .await
        .expect("layers")
        .0;

        // Assert
        assert_eq!(
            body["layers"],
            serde_json::json!([
                {"scope": "agent", "path": "agents/lyre/SOUL.md", "content": "Lyre soul", "applied": true},
                {"scope": "global", "path": "SOUL.md", "content": "Global soul", "applied": false},
            ])
        );
    }

    #[tokio::test]
    async fn prompt_file_rejects_unknown_kind_and_agent() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);

        // Act
        let bad_kind = get_agent_prompt_file(
            State(state.clone()),
            UrlPath(("lyre".to_string(), "pulse".to_string())),
        )
        .await;
        let bad_agent = get_agent_prompt_file(
            State(state),
            UrlPath(("ghost".to_string(), "soul".to_string())),
        )
        .await;

        // Assert
        assert_eq!(status_of(bad_kind), StatusCode::BAD_REQUEST);
        assert_eq!(status_of(bad_agent), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn memory_history_returns_published_changes_only() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);
        insert_change(&state.app_state.db, "published", "success", "a", "b");
        insert_change(&state.app_state.db, "pending", "running", "b", "c");

        // Act
        let body = get_agent_memory_history(
            State(state),
            UrlPath("lyre".to_string()),
            Query(HistoryQuery { limit: None }),
        )
        .await
        .expect("history")
        .0;

        // Assert
        assert_eq!(
            body["changes"],
            serde_json::json!([{
                "run_id": "published",
                "file": "semantic",
                "created_at": "2026-10-30T03:05:00+00:00",
                "content_before": "a",
                "content_after": "b",
            }])
        );
    }

    #[tokio::test]
    async fn memory_history_rejects_unknown_agent() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);

        // Act
        let result = get_agent_memory_history(
            State(state),
            UrlPath("ghost".to_string()),
            Query(HistoryQuery { limit: None }),
        )
        .await;

        // Assert
        assert_eq!(status_of(result), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn skills_are_listed_and_loaded_with_frontmatter_and_files() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);
        write(
            &dir,
            "workspace/skills/notes/SKILL.md",
            "---\nname: notes\ndescription: Take notes\nrequired_env: NOTES_TOKEN\n---\nWrite things down.\n",
        );
        write(
            &dir,
            "workspace/skills/notes/references/style.md",
            "# Style",
        );

        // Act
        let list = list_skills(State(state.clone())).await.expect("list").0;
        let detail = get_skill(State(state.clone()), UrlPath("notes".to_string()))
            .await
            .expect("detail")
            .0;
        let missing = get_skill(State(state), UrlPath("ghost".to_string())).await;

        // Assert
        assert_eq!(
            list["skills"],
            serde_json::json!([{"name": "notes", "description": "Take notes", "source": "workspace"}])
        );
        let skill = &detail["skill"];
        assert_eq!(skill["path"], "workspace/skills/notes");
        assert_eq!(skill["required_env"], serde_json::json!(["NOTES_TOKEN"]));
        assert_eq!(skill["instructions"], "Write things down.");
        assert_eq!(
            skill["files"],
            serde_json::json!([{"path": "references/style.md", "bytes": 7}])
        );
        assert_eq!(status_of(missing), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn skill_file_is_served_only_inside_the_skill_dir() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);
        write(
            &dir,
            "workspace/skills/notes/SKILL.md",
            "---\nname: notes\ndescription: Take notes\n---\nBody.\n",
        );
        write(
            &dir,
            "workspace/skills/notes/references/style.md",
            "# Style",
        );
        write(&dir, "SOUL.md", "Global soul");

        // Act
        let file = get_skill_file(
            State(state.clone()),
            UrlPath(("notes".to_string(), "references/style.md".to_string())),
        )
        .await
        .expect("file")
        .0;
        let escaped = get_skill_file(
            State(state),
            UrlPath(("notes".to_string(), "../../../SOUL.md".to_string())),
        )
        .await;

        // Assert
        assert_eq!(file["content"], "# Style");
        assert_eq!(status_of(escaped), StatusCode::NOT_FOUND);
    }
}

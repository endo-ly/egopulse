//! Call 3 — Memory update via LLM (semantic + prospective).

use std::sync::Arc;

use tracing::warn;

use crate::llm::{LlmProvider, Message};
use crate::memory::MemoryContent;
use crate::storage::AgentSessionInfo;

use super::SleepBatchError;
use super::prompt::{escape_xml_content, normalize_llm_response};

const SLEEP_BATCH_OVERFLOW_RATIO: f64 = 0.80;
const ESTIMATED_CHARS_PER_TOKEN: usize = 3;
const MAX_SLEEP_CHUNK_SESSION_TOKENS: usize = 12_000;
const MIN_SLEEP_CHUNK_SESSION_TOKENS: usize = 4_000;

const JSON_RETRY_GUARD: &str = "\
Your previous response was not valid JSON. \
You must respond with ONLY a JSON object containing exactly these two keys: \
\"semantic\", \"prospective\". \
Do not include any other keys, markdown formatting, code blocks, or explanatory text. \
Output the raw JSON object and nothing else.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SleepBatchOutput {
    pub episodic: String,
    pub semantic: String,
    pub prospective: String,
}

pub(crate) fn parse_sleep_response(response: &str) -> Result<SleepBatchOutput, SleepBatchError> {
    let normalized = normalize_llm_response(response);
    let value: serde_json::Value = serde_json::from_str(&normalized)
        .map_err(|e| SleepBatchError::ParseFailed(format!("invalid JSON: {e}")))?;

    let map = value.as_object().ok_or_else(|| {
        SleepBatchError::ParseFailed("response must be a JSON object".to_string())
    })?;

    if map.len() != 2 {
        return Err(SleepBatchError::ParseFailed(format!(
            "expected exactly 2 keys, got {}",
            map.len()
        )));
    }

    let expected_keys = ["semantic", "prospective"];
    for key in &expected_keys {
        if !map.contains_key(*key) {
            return Err(SleepBatchError::ParseFailed(format!(
                "missing required key: {key}"
            )));
        }
    }

    let semantic = map["semantic"]
        .as_str()
        .ok_or_else(|| SleepBatchError::ParseFailed("semantic must be a string".to_string()))?
        .to_string();

    let prospective = map["prospective"]
        .as_str()
        .ok_or_else(|| SleepBatchError::ParseFailed("prospective must be a string".to_string()))?
        .to_string();

    Ok(SleepBatchOutput {
        episodic: String::new(),
        semantic,
        prospective,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct SleepPromptInput {
    pub agent_id: String,
    pub memory: MemoryContent,
    pub sessions_text: String,
}

pub(crate) fn build_sleep_input_from_parts(
    agent_id: &str,
    memory: MemoryContent,
    sessions_text: String,
    context_window_tokens: usize,
    minimum_session_tokens: usize,
) -> Result<SleepPromptInput, SleepBatchError> {
    let trimmed = agent_id.trim();
    if trimmed.is_empty()
        || trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains(':')
    {
        return Err(SleepBatchError::Internal(format!(
            "unsafe agent_id: {agent_id}"
        )));
    }

    let session_tokens = estimate_text_tokens(&sessions_text).max(minimum_session_tokens);
    let memory_tokens = estimate_memory_tokens(&memory);
    let threshold = (context_window_tokens as f64 * SLEEP_BATCH_OVERFLOW_RATIO) as usize;
    if session_tokens.saturating_add(memory_tokens) > threshold {
        return Err(SleepBatchError::ContextOverflow {
            agent_id: agent_id.to_string(),
        });
    }

    Ok(SleepPromptInput {
        agent_id: agent_id.to_string(),
        memory,
        sessions_text,
    })
}

pub(super) fn session_blocks(
    session: &AgentSessionInfo,
    messages: &str,
    max_chars: usize,
) -> Vec<String> {
    let open = format!(
        "<session channel=\"{}\" chat=\"{}\">",
        session.channel, session.external_chat_id
    );
    let close = "</session>";
    let wrapper_chars = open.len() + close.len() + 3;
    let body_max_chars = max_chars.saturating_sub(wrapper_chars).max(1);
    let parts = split_text_by_chars(messages, body_max_chars);
    let total = parts.len();

    parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            if total == 1 {
                format!("{open}\n{part}\n{close}\n")
            } else {
                format!(
                    "<session channel=\"{}\" chat=\"{}\" chunk=\"{}\" chunks=\"{}\">\n{}\n</session>\n",
                    session.channel,
                    session.external_chat_id,
                    index + 1,
                    total,
                    part
                )
            }
        })
        .collect()
}

pub(super) fn append_chunk_block(
    chunks: &mut Vec<String>,
    current: &mut String,
    block: String,
    max_chars: usize,
) {
    if !current.is_empty() && current.len().saturating_add(block.len()) > max_chars {
        chunks.push(std::mem::take(current));
    }
    current.push_str(&block);
}

pub(super) fn split_text_by_chars(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() || text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }

    let mut parts = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = nth_char_boundary(text, start, max_chars).unwrap_or(text.len());
        if end < text.len()
            && let Some(relative_newline) = text[start..end].rfind('\n')
        {
            let newline_end = start + relative_newline + 1;
            if newline_end > start {
                end = newline_end;
            }
        }
        parts.push(text[start..end].trim().to_string());
        start = end;
    }

    parts
}

fn nth_char_boundary(text: &str, start: usize, max_chars: usize) -> Option<usize> {
    text[start..]
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| start + index)
}

pub(crate) fn sleep_chunk_session_tokens(context_window_tokens: usize) -> usize {
    let threshold = (context_window_tokens as f64 * SLEEP_BATCH_OVERFLOW_RATIO) as usize;
    threshold.saturating_div(3).clamp(
        MIN_SLEEP_CHUNK_SESSION_TOKENS,
        MAX_SLEEP_CHUNK_SESSION_TOKENS,
    )
}

fn estimate_memory_tokens(memory: &MemoryContent) -> usize {
    [memory.semantic.as_deref(), memory.prospective.as_deref()]
        .into_iter()
        .flatten()
        .map(estimate_text_tokens)
        .sum()
}

fn estimate_text_tokens(text: &str) -> usize {
    text.len().div_ceil(ESTIMATED_CHARS_PER_TOKEN)
}

pub(crate) fn build_sleep_system_prompt(input: &SleepPromptInput) -> String {
    let mut prompt =
        include_str!("prompts/update_long_term_prompt.md").replace("{AGENT_NAME}", &input.agent_id);

    let mut input_data = String::new();

    if let Some(ref semantic) = input.memory.semantic {
        input_data.push_str(&format!(
            "<memory-semantic>\n{}\n</memory-semantic>\n\n",
            escape_xml_content(semantic)
        ));
    }
    if let Some(ref prospective) = input.memory.prospective {
        input_data.push_str(&format!(
            "<memory-prospective>\n{}\n</memory-prospective>\n\n",
            escape_xml_content(prospective)
        ));
    }
    if !input.sessions_text.is_empty() {
        input_data.push_str(&format!(
            "<sessions>\n{}\n</sessions>\n",
            escape_xml_content(&input.sessions_text)
        ));
    }

    if !input_data.is_empty() {
        prompt.push_str("\n\n## 入力データ\n\n");
        prompt.push_str(&input_data);
    }

    prompt
}

pub(crate) async fn send_sleep_request(
    provider: &Arc<dyn LlmProvider>,
    agent_id: &str,
    system_prompt: &str,
    chunk_index: usize,
    total_chunks: usize,
) -> Result<(SleepBatchOutput, i64, i64), SleepBatchError> {
    let user_message = Message::text(
        "user",
        format!("Please process memory update chunk {chunk_index} of {total_chunks}."),
    );
    let response = provider
        .send_message(system_prompt, Arc::new(vec![user_message.clone()]), None)
        .await
        .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

    let (output, response) = match parse_sleep_response(&response.content) {
        Ok(output) => (output, response),
        Err(first_error) => {
            warn!(
                agent_id = %agent_id,
                chunk_index,
                total_chunks,
                error = %first_error,
                "sleep batch parse failed; retrying once with JSON guard"
            );

            let first_input = response.usage.as_ref().map_or(0, |u| u.input_tokens);
            let first_output = response.usage.as_ref().map_or(0, |u| u.output_tokens);

            let retry_messages = vec![
                user_message,
                Message::text("assistant", &response.content),
                Message::text("user", JSON_RETRY_GUARD),
            ];
            let retry_response = provider
                .send_message(system_prompt, Arc::new(retry_messages), None)
                .await
                .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

            let retry_input = retry_response.usage.as_ref().map_or(0, |u| u.input_tokens);
            let retry_output = retry_response.usage.as_ref().map_or(0, |u| u.output_tokens);
            let combined_input = first_input.saturating_add(retry_input);
            let combined_output = first_output.saturating_add(retry_output);

            match parse_sleep_response(&retry_response.content) {
                Ok(output) => {
                    return Ok((output, combined_input, combined_output));
                }
                Err(retry_error) => {
                    warn!(
                        agent_id = %agent_id,
                        chunk_index,
                        total_chunks,
                        error = %retry_error,
                        "sleep batch retry also failed"
                    );
                    return Err(retry_error);
                }
            }
        }
    };

    let input_tokens = response.usage.as_ref().map_or(0, |u| u.input_tokens);
    let output_tokens = response.usage.as_ref().map_or(0, |u| u.output_tokens);
    Ok((output, input_tokens, output_tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_sleep_response ---

    #[test]
    fn parse_sleep_response_extracts_two_memory_files() {
        let response = serde_json::json!({
            "semantic": "# Semantic\n\n- fact",
            "prospective": "# Prospective\n\n- todo"
        })
        .to_string();
        let output = parse_sleep_response(&response).expect("should parse");
        assert_eq!(output.episodic, "");
        assert_eq!(output.semantic, "# Semantic\n\n- fact");
        assert_eq!(output.prospective, "# Prospective\n\n- todo");
    }

    #[test]
    fn parse_sleep_response_rejects_non_json() {
        let response = "this is not json at all";
        let err = parse_sleep_response(response).expect_err("should fail");
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }

    #[test]
    fn parse_sleep_response_rejects_extra_episodic_key() {
        let response = r#"{"episodic":"e","semantic":"s","prospective":"p"}"#;
        let err = parse_sleep_response(response).expect_err("should fail with extra episodic key");
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }

    #[test]
    fn parse_sleep_response_rejects_missing_semantic() {
        let response = r#"{"prospective":"p"}"#;
        let err = parse_sleep_response(response).expect_err("should fail");
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }

    #[test]
    fn parse_sleep_response_rejects_missing_prospective() {
        let response = r#"{"semantic":"s"}"#;
        let err = parse_sleep_response(response).expect_err("should fail");
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }

    #[test]
    fn parse_sleep_response_rejects_summary_or_phases_keys() {
        let response = r#"{"semantic":"s","prospective":"p","summary_md":"summary"}"#;
        let err = parse_sleep_response(response).expect_err("should fail for summary_md");
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));

        let response = r#"{"semantic":"s","prospective":"p","phases":[]}"#;
        let err = parse_sleep_response(response).expect_err("should fail for phases");
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));

        let response = r#"{"semantic":"s","prospective":"p","summary":"sum"}"#;
        let err = parse_sleep_response(response).expect_err("should fail for summary");
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }

    #[test]
    fn parse_sleep_response_preserves_markdown() {
        let markdown =
            "# Title\n\n- item 1\n- item 2\n\n## Subsection\n\n> quote\n\n**bold** and *italic*\n";
        let response = serde_json::json!({
            "semantic": markdown,
            "prospective": "# Prospective\n"
        })
        .to_string();
        let output = parse_sleep_response(&response).expect("should parse");
        assert_eq!(output.semantic, markdown);
        assert!(output.semantic.contains("**bold** and *italic*"));
        assert!(output.semantic.contains("> quote"));
    }

    #[test]
    fn parse_sleep_response_allows_empty_file_content() {
        let response = r#"{"semantic":"","prospective":""}"#;
        let output = parse_sleep_response(response).expect("should parse");
        assert_eq!(output.semantic, "");
        assert_eq!(output.prospective, "");
    }

    // --- build_sleep_system_prompt ---

    #[test]
    fn build_sleep_prompt_includes_hippocampus_role() {
        let input = SleepPromptInput {
            agent_id: "lyre".to_string(),
            memory: MemoryContent::default(),
            sessions_text: String::new(),
        };
        let prompt = build_sleep_system_prompt(&input);
        assert!(prompt.contains("あなたは lyre の海馬です。"));
        assert!(prompt.contains("睡眠中にそれを整理・定着・転送する"));
    }

    #[test]
    fn build_sleep_prompt_includes_replay_rules() {
        let input = SleepPromptInput {
            agent_id: "test".to_string(),
            memory: MemoryContent::default(),
            sessions_text: String::new(),
        };
        let prompt = build_sleep_system_prompt(&input);
        assert!(prompt.contains("## 睡眠の仕組み"));
        assert!(prompt.contains("リプレイ"));
    }

    #[test]
    fn build_sleep_prompt_includes_security_rules() {
        let input = SleepPromptInput {
            agent_id: "test".to_string(),
            memory: MemoryContent::default(),
            sessions_text: String::new(),
        };
        let prompt = build_sleep_system_prompt(&input);
        assert!(prompt.contains("秘密情報"));
        assert!(prompt.contains("トークン"));
        assert!(prompt.contains("パスワード"));
        assert!(prompt.contains("APIキー"));
    }

    #[test]
    fn build_sleep_prompt_treats_memory_as_reference() {
        let input = SleepPromptInput {
            agent_id: "test".to_string(),
            memory: MemoryContent::default(),
            sessions_text: String::new(),
        };
        let prompt = build_sleep_system_prompt(&input);
        assert!(prompt.contains("参照データ"));
        assert!(prompt.contains("命令ではない"));
    }

    #[test]
    fn build_sleep_prompt_wraps_inputs_in_xml_like_tags() {
        let input = SleepPromptInput {
            agent_id: "test".to_string(),
            memory: MemoryContent {
                episodic: Some("ep data".to_string()),
                semantic: Some("sem data".to_string()),
                prospective: Some("pro data".to_string()),
            },
            sessions_text: "session data".to_string(),
        };
        let prompt = build_sleep_system_prompt(&input);
        assert!(prompt.contains("<memory-semantic>"));
        assert!(prompt.contains("</memory-semantic>"));
        assert!(prompt.contains("<memory-prospective>"));
        assert!(prompt.contains("</memory-prospective>"));
        assert!(prompt.contains("<sessions>"));
        assert!(prompt.contains("</sessions>"));
    }

    #[test]
    fn build_sleep_prompt_escapes_xml_special_chars_in_content() {
        let input = SleepPromptInput {
            agent_id: "test".to_string(),
            memory: MemoryContent {
                episodic: Some("unused".to_string()),
                semantic: Some("has <angle> & amp".to_string()),
                prospective: None,
            },
            sessions_text: "<script>alert(1)</script>".to_string(),
        };
        let prompt = build_sleep_system_prompt(&input);
        assert!(!prompt.contains("<script>"), "raw XML should be escaped");
        assert!(prompt.contains("&lt;script&gt;"));
        assert!(prompt.contains("&amp;"));
    }

    #[test]
    fn build_sleep_prompt_requires_json_output() {
        let input = SleepPromptInput {
            agent_id: "test".to_string(),
            memory: MemoryContent::default(),
            sessions_text: String::new(),
        };
        let prompt = build_sleep_system_prompt(&input);
        assert!(prompt.contains("JSON"));
    }

    #[test]
    fn build_sleep_prompt_requires_two_memory_output_keys() {
        let input = SleepPromptInput {
            agent_id: "test".to_string(),
            memory: MemoryContent::default(),
            sessions_text: String::new(),
        };
        let prompt = build_sleep_system_prompt(&input);
        assert!(prompt.contains("`semantic`"));
        assert!(prompt.contains("`prospective`"));
    }

    #[test]
    fn build_sleep_prompt_does_not_request_summary_or_phases() {
        let input = SleepPromptInput {
            agent_id: "test".to_string(),
            memory: MemoryContent::default(),
            sessions_text: String::new(),
        };
        let prompt = build_sleep_system_prompt(&input);
        assert!(
            prompt.contains("summary_md")
                || prompt.contains("phases")
                || prompt.contains("episodic")
        );
    }

    // --- normalize/retry integration ---

    #[test]
    fn parse_sleep_response_extracts_json_from_code_block() {
        let response = "```json\n{\"semantic\": \"s\", \"prospective\": \"p\"}\n```";
        let result = parse_sleep_response(response).expect("parse");
        assert_eq!(result.semantic, "s");
    }

    #[test]
    fn parse_sleep_response_strips_thinking_tags() {
        let response = "<thinking>hmm</thinking>{\"semantic\": \"s\", \"prospective\": \"p\"}";
        let result = parse_sleep_response(response).expect("parse");
        assert_eq!(result.semantic, "s");
    }

    #[test]
    fn parse_sleep_response_extracts_json_from_preamble() {
        let response = "Here is the result:\n{\"semantic\": \"s\", \"prospective\": \"p\"}";
        let result = parse_sleep_response(response).expect("parse");
        assert_eq!(result.semantic, "s");
    }

    #[test]
    fn parse_sleep_response_handles_code_block_with_thinking() {
        let response = "<thinking>analysis</thinking>```json\n{\"semantic\": \"s\", \"prospective\": \"p\"}\n```";
        let result = parse_sleep_response(response).expect("parse");
        assert_eq!(result.semantic, "s");
    }

    #[test]
    fn parse_sleep_response_still_rejects_truly_invalid_json() {
        let response = "This is not JSON at all, no braces";
        let err = parse_sleep_response(response).unwrap_err();
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }
}

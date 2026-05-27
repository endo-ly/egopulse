//! LLM response normalization and JSON extraction utilities.

/// Maximum characters of raw LLM response to include in error messages and logs.
const RAW_RESPONSE_PREVIEW_CHARS: usize = 300;

/// Normalizes a raw LLM response into a string that is more likely to parse as JSON.
///
/// Applies in order:
/// 1. Strips `<thinking>` / `<thought>` / `<reasoning>` tag blocks.
/// 2. Extracts JSON from markdown code blocks (```` ```json ... ``` ````).
/// 3. Extracts the outermost `{ … }` span to remove preamble text.
pub(crate) fn normalize_llm_response(raw: &str) -> String {
    let stripped = crate::agent_loop::formatting::strip_thinking(raw);

    if let Some(json) = extract_json_from_code_block(&stripped) {
        return json;
    }

    extract_json_object_span(&stripped).unwrap_or(stripped)
}

pub(crate) fn extract_json_from_code_block(text: &str) -> Option<String> {
    let marker = "```json";
    let start = text.find(marker)?;
    let content_start = start + marker.len();
    let end = text[content_start..].find("```")?;
    Some(text[content_start..content_start + end].trim().to_string())
}

pub(crate) fn extract_json_object_span(text: &str) -> Option<String> {
    let first = text.find('{')?;
    let last = text.rfind('}')?;
    if first < last {
        Some(text[first..=last].to_string())
    } else {
        None
    }
}

pub(crate) fn preview_raw_response(raw: &str) -> String {
    let truncated: String = raw.chars().take(RAW_RESPONSE_PREVIEW_CHARS).collect();
    if raw.chars().count() > RAW_RESPONSE_PREVIEW_CHARS {
        format!("{truncated}...")
    } else {
        truncated
    }
}

/// Escapes XML special characters in content to prevent injection.
pub(crate) fn escape_xml_content(content: &str) -> String {
    content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_llm_response_extracts_from_json_code_block() {
        let input = "```json\n{\"key\": \"value\"}\n```";
        let result = normalize_llm_response(input);
        assert_eq!(result, "{\"key\": \"value\"}");
    }

    #[test]
    fn normalize_llm_response_extracts_brace_span_when_no_code_block() {
        let input = "Some preamble {\"key\": \"value\"} trailing text";
        let result = normalize_llm_response(input);
        assert_eq!(result, "{\"key\": \"value\"}");
    }

    #[test]
    fn normalize_llm_response_returns_as_is_when_no_json_structure() {
        let input = "just plain text";
        let result = normalize_llm_response(input);
        assert_eq!(result, "just plain text");
    }
}

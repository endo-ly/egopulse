//! テキスト操作ユーティリティ — 切り詰め・正規化・ファジーマッチ・編集適用。

use similar::TextDiff;

// ---------------------------------------------------------------------------
// データ構造
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<&'static str>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditSpec {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppliedEditsResult {
    pub base_content: String,
    pub new_content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedEdit {
    pub edit_index: usize,
    pub match_index: usize,
    pub match_length: usize,
    pub new_text: String,
}

// ---------------------------------------------------------------------------
// フォーマット
// ---------------------------------------------------------------------------

pub(crate) fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// ---------------------------------------------------------------------------
// 切り詰め
// ---------------------------------------------------------------------------

pub(crate) fn truncate_head(content: &str, max_lines: usize, max_bytes: usize) -> TruncationResult {
    let total_bytes = content.len();
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let first_line_bytes = lines.first().map(|line| line.len()).unwrap_or(0);
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some("bytes"),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut selected = Vec::new();
    let mut bytes = 0usize;
    let mut truncated_by = Some("lines");
    for (index, line) in lines.iter().enumerate() {
        if index >= max_lines {
            truncated_by = Some("lines");
            break;
        }
        let line_bytes = line.len() + usize::from(index > 0);
        if bytes + line_bytes > max_bytes {
            truncated_by = Some("bytes");
            break;
        }
        selected.push(*line);
        bytes += line_bytes;
    }

    let output = selected.join("\n");
    TruncationResult {
        content: output.clone(),
        truncated: true,
        truncated_by,
        total_lines,
        total_bytes,
        output_lines: selected.len(),
        output_bytes: output.len(),
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

pub(crate) fn truncate_string_to_bytes_from_end(value: &str, max_bytes: usize) -> String {
    let bytes = value.as_bytes();
    if bytes.len() <= max_bytes {
        return value.to_string();
    }
    // UTF-8 境界まで前方にシフトしてマルチバイト文字の切断を防ぐ
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).to_string()
}

pub(crate) fn truncate_tail(content: &str, max_lines: usize, max_bytes: usize) -> TruncationResult {
    let total_bytes = content.len();
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let mut selected = Vec::new();
    let mut bytes = 0usize;
    let mut truncated_by = Some("lines");
    let mut last_line_partial = false;

    for line in lines.iter().rev() {
        if selected.len() >= max_lines {
            truncated_by = Some("lines");
            break;
        }
        let line_bytes = line.len() + usize::from(!selected.is_empty());
        if bytes + line_bytes > max_bytes {
            truncated_by = Some("bytes");
            if selected.is_empty() {
                selected.push(truncate_string_to_bytes_from_end(line, max_bytes));
                last_line_partial = true;
            }
            break;
        }
        selected.push((*line).to_string());
        bytes += line_bytes;
    }
    selected.reverse();
    let output = selected.join("\n");
    TruncationResult {
        content: output.clone(),
        truncated: true,
        truncated_by,
        total_lines,
        total_bytes,
        output_lines: selected.len(),
        output_bytes: output.len(),
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

// ---------------------------------------------------------------------------
// シェルユーティリティ
// ---------------------------------------------------------------------------

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

// ---------------------------------------------------------------------------
// テキスト正規化
// ---------------------------------------------------------------------------

pub(crate) fn normalize_newlines(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

pub(crate) fn normalize_for_fuzzy_match(value: &str) -> String {
    normalize_newlines(value)
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(normalize_fuzzy_char)
        .collect()
}

pub(crate) fn fuzzy_byte_pos_to_original_byte_pos(original: &str, fuzzy_byte_pos: usize) -> usize {
    let fuzzy = normalize_for_fuzzy_match(original);
    let fuzzy_char_pos = fuzzy[..fuzzy_byte_pos].chars().count();
    original
        .char_indices()
        .nth(fuzzy_char_pos)
        .map(|(pos, _)| pos)
        .unwrap_or(original.len())
}

pub(crate) fn normalize_fuzzy_char(value: char) -> char {
    match value {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
        | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
        | '\u{3000}' => ' ',
        _ => value,
    }
}

pub(crate) fn detect_line_ending(content: &str) -> &'static str {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (crlf_idx, lf_idx) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}

pub(crate) fn restore_line_endings(content: &str, ending: &str) -> String {
    if ending == "\r\n" {
        content.replace('\n', "\r\n")
    } else {
        content.to_string()
    }
}

pub(crate) fn strip_bom(content: &str) -> (&str, &str) {
    if let Some(rest) = content.strip_prefix('\u{feff}') {
        ("\u{feff}", rest)
    } else {
        ("", content)
    }
}

pub(crate) fn fuzzy_find_text(content: &str, old_text: &str) -> Option<(usize, usize, bool)> {
    if let Some(index) = content.find(old_text) {
        return Some((index, old_text.len(), false));
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    fuzzy_content
        .find(&fuzzy_old_text)
        .map(|index| (index, fuzzy_old_text.len(), true))
}

pub(crate) fn count_occurrences(content: &str, needle: &str) -> usize {
    let normalized_content = normalize_for_fuzzy_match(content);
    let normalized_needle = normalize_for_fuzzy_match(needle);
    if normalized_needle.is_empty() {
        return 0;
    }
    normalized_content.match_indices(&normalized_needle).count()
}

// ---------------------------------------------------------------------------
// エラーメッセージ生成
// ---------------------------------------------------------------------------

pub(crate) fn get_not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        )
    }
}

pub(crate) fn get_duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> String {
    if total_edits == 1 {
        format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        )
    }
}

pub(crate) fn get_empty_old_text_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
) -> String {
    if total_edits == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

pub(crate) fn get_no_change_error(path: &str, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        )
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

// ---------------------------------------------------------------------------
// 編集適用
// ---------------------------------------------------------------------------

pub(crate) fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[EditSpec],
    path: &str,
) -> Result<AppliedEditsResult, String> {
    let normalized_edits = normalize_edits(edits);
    validate_non_empty_old_texts(&normalized_edits, path)?;

    let mut base_content = normalized_content.to_string();
    let mut matched_edits = Vec::with_capacity(normalized_edits.len());
    // 編集適用位置を補正するための累積オフセット。fuzzy 置換で元と正規化後の
    // バイト長が異なる場合に差分を蓄積し、後続編集の開始位置を調整する。
    let mut cumulative_offset: isize = 0;

    for (index, edit) in normalized_edits.iter().enumerate() {
        let matched = match_normalized_edit(
            normalized_content,
            &mut base_content,
            edit,
            index,
            normalized_edits.len(),
            path,
            &mut cumulative_offset,
        )?;
        matched_edits.push(matched);
    }

    ensure_non_overlapping_matches(&mut matched_edits, path)?;
    let new_content = apply_matched_edits(&base_content, &matched_edits);

    if new_content == base_content {
        return Err(get_no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

fn normalize_edits(edits: &[EditSpec]) -> Vec<EditSpec> {
    edits
        .iter()
        .map(|edit| EditSpec {
            old_text: normalize_newlines(&edit.old_text),
            new_text: normalize_newlines(&edit.new_text),
        })
        .collect()
}

fn validate_non_empty_old_texts(edits: &[EditSpec], path: &str) -> Result<(), String> {
    for (index, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(get_empty_old_text_error(path, index, edits.len()));
        }
    }
    Ok(())
}

fn match_normalized_edit(
    normalized_content: &str,
    base_content: &mut String,
    edit: &EditSpec,
    index: usize,
    total_edits: usize,
    path: &str,
    cumulative_offset: &mut isize,
) -> Result<MatchedEdit, String> {
    let Some((fuzzy_pos, fuzzy_len, used_fuzzy)) =
        fuzzy_find_text(normalized_content, &edit.old_text)
    else {
        return Err(get_not_found_error(path, index, total_edits));
    };

    let (span_start, span_end, match_length) =
        resolve_match_span(normalized_content, edit, fuzzy_pos, fuzzy_len, used_fuzzy);
    let adjusted_start = (span_start as isize + *cumulative_offset) as usize;
    let adjusted_end = (span_end as isize + *cumulative_offset) as usize;

    if used_fuzzy {
        *cumulative_offset += normalize_matched_span(base_content, adjusted_start, adjusted_end)
            - (span_end - span_start) as isize;
    }

    let occurrences = count_occurrences(base_content, &edit.old_text);
    if occurrences > 1 {
        return Err(get_duplicate_error(path, index, total_edits, occurrences));
    }

    Ok(MatchedEdit {
        edit_index: index,
        match_index: adjusted_start,
        match_length,
        new_text: edit.new_text.clone(),
    })
}

fn resolve_match_span(
    normalized_content: &str,
    edit: &EditSpec,
    fuzzy_pos: usize,
    fuzzy_len: usize,
    used_fuzzy: bool,
) -> (usize, usize, usize) {
    if !used_fuzzy {
        return (fuzzy_pos, fuzzy_pos + fuzzy_len, fuzzy_len);
    }

    let orig_start = fuzzy_byte_pos_to_original_byte_pos(normalized_content, fuzzy_pos);
    let fuzzy_old_text = normalize_for_fuzzy_match(&edit.old_text);
    let char_count = fuzzy_old_text.chars().count();
    let orig_end = normalized_content[orig_start..]
        .char_indices()
        .nth(char_count)
        .map(|(pos, _)| orig_start + pos)
        .unwrap_or(normalized_content.len());
    (orig_start, orig_end, fuzzy_old_text.len())
}

fn normalize_matched_span(
    base_content: &mut String,
    adjusted_start: usize,
    adjusted_end: usize,
) -> isize {
    let original_span = &base_content[adjusted_start..adjusted_end];
    let normalized_span = normalize_for_fuzzy_match(original_span);
    let normalized_len = normalized_span.len() as isize;
    *base_content = format!(
        "{}{}{}",
        &base_content[..adjusted_start],
        normalized_span,
        &base_content[adjusted_end..]
    );
    normalized_len
}

fn ensure_non_overlapping_matches(
    matched_edits: &mut [MatchedEdit],
    path: &str,
) -> Result<(), String> {
    matched_edits.sort_by_key(|edit| edit.match_index);
    for pair in matched_edits.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index, path
            ));
        }
    }
    Ok(())
}

fn apply_matched_edits(base_content: &str, matched_edits: &[MatchedEdit]) -> String {
    let mut new_content = base_content.to_string();
    for edit in matched_edits.iter().rev() {
        new_content = format!(
            "{}{}{}",
            &new_content[..edit.match_index],
            edit.new_text,
            &new_content[edit.match_index + edit.match_length..]
        );
    }
    new_content
}

// ---------------------------------------------------------------------------
// Diff 生成
// ---------------------------------------------------------------------------

pub(crate) fn generate_diff_string(path: &str, base_content: &str, new_content: &str) -> String {
    let diff = TextDiff::from_lines(base_content, new_content);
    diff.unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

pub(crate) fn first_changed_line(base_content: &str, new_content: &str) -> Option<usize> {
    let base_lines = base_content.split('\n').collect::<Vec<_>>();
    let new_lines = new_content.split('\n').collect::<Vec<_>>();
    let max_len = base_lines.len().max(new_lines.len());
    for index in 0..max_len {
        if base_lines.get(index) != new_lines.get(index) {
            return Some(index + 1);
        }
    }
    None
}

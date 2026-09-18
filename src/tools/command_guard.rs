//! シェルコマンドのセキュリティガード。
//!
//! AI エージェントによる環境変数ダンプやシークレット窃取を防止するため、
//! 実行前にコマンド文字列を検証し、危険なパターンをブロックする。

/// ブロック対象のコマンド名（単語境界で照合）。
const BLOCKED_COMMANDS: &[&str] = &["env", "printenv"];

/// `/proc/` 配下へのアクセスを検出するプレフィックス。
const PROC_PREFIX: &str = "/proc/";

/// `/proc/self/environ`, `/proc/self/mem` 等、プロセス内部情報へのアクセスをブロックする。
///
/// `/proc/self/*`（environ, mem, maps, fd, cmdline 等）および
/// `/proc/<pid>/*` を包括的に検出する。
fn check_blocked_patterns(command: &str) -> Result<(), String> {
    let mut start = 0usize;
    while let Some(offset) = command[start..].find(PROC_PREFIX) {
        let abs_offset = start + offset;
        let after = &command[abs_offset + PROC_PREFIX.len()..];
        // "/proc/" の後に "self/" または "<digits>/" が続けばブロック
        let is_self = after.starts_with("self/");
        let is_pid = after
            .split('/')
            .next()
            .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty());
        if is_self || is_pid {
            return Err(
                "Access denied: command references /proc/*/..., which exposes process internals."
                    .to_string(),
            );
        }
        start = abs_offset + 1;
        if start >= command.len() {
            break;
        }
    }
    Ok(())
}

/// シェル経由でブロック対象コマンドを実行するパターンを検出する。
const SHELL_EXEC_PREFIXES: &[&str] = &["bash -c", "sh -c", "dash -c", "zsh -c", "ksh -c", "eval "];

/// `env`・`printenv` の実行をブロックする。
///
/// 直接実行に加え、`bash -c 'env'` や `eval 'printenv'` 等
/// シェル経由のバイパスも検出する。
fn check_blocked_commands(command: &str) -> Result<(), String> {
    for segment in split_command_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(first_word) = trimmed.split_whitespace().next() else {
            continue;
        };
        for blocked in BLOCKED_COMMANDS {
            if first_word == *blocked {
                return Err(format!(
                    "Access denied: '{blocked}' is blocked to prevent environment variable leakage. \
                     Use 'echo $VAR_NAME' to read a specific variable."
                ));
            }
        }
        if SHELL_EXEC_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
            for blocked in BLOCKED_COMMANDS {
                if contains_blocked_command_token(trimmed, blocked) {
                    return Err(format!(
                        "Access denied: '{blocked}' detected in shell execution context."
                    ));
                }
            }
        }
    }
    Ok(())
}

/// コマンドがセキュリティポリシーに違反するか検査する。
pub(crate) fn check_command(command: &str) -> Result<(), String> {
    check_blocked_commands(command)?;
    check_blocked_patterns(command)?;
    check_set_without_options(command)?;
    Ok(())
}

/// 引数なしの `set`（シェル変数・関数の全ダンプ）をブロックする。
///
/// `set -e` や `set -o pipefail` のようなオプション付きは許可する。
fn check_set_without_options(command: &str) -> Result<(), String> {
    for segment in split_command_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        let words: Vec<&str> = trimmed.split_whitespace().collect();
        if words.first() != Some(&"set") {
            continue;
        }
        let next_is_option = words
            .get(1)
            .is_some_and(|w| w.starts_with('-') || w.starts_with('+'));
        if !next_is_option {
            return Err(
                "Access denied: bare 'set' is blocked to prevent shell variable leakage. \
                 Use 'set -e', 'set -o pipefail', etc. for option configuration."
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// コマンド文字列を簡易トークン化する。
///
/// シェルの完全なパーサーではなく、一般的なケースで十分な精度を確保する。
/// パイプ `|`、セミコロン `;`、`&&`、`||` で区切られた各セグメントの
/// 最初の単語をコマンド名として抽出する。
#[cfg(test)]
fn tokenize(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();

    for segment in split_command_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        for word in trimmed.split_whitespace() {
            let w = word.to_string();
            if !w.is_empty() {
                tokens.push(w);
            }
        }
    }

    tokens
}

/// コマンド文字列をパイプ・セミコロン・論理演算子で分割する。
fn split_command_segments(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let chars: Vec<(usize, char)> = command.char_indices().collect();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut i = 0usize;

    while i < chars.len() {
        let (byte_pos, ch) = chars[i];
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if !in_single_quote && !in_double_quote {
            if ch == ';' || ch == '\n' {
                if byte_pos > start {
                    segments.push(&command[start..byte_pos]);
                }
                start = byte_pos + 1;
            } else if ch == '|' {
                if byte_pos > start {
                    segments.push(&command[start..byte_pos]);
                }
                start = byte_pos + 1;
                if i + 1 < chars.len() && chars[i + 1].1 == '|' {
                    i += 1;
                    start = chars[i].0 + 1;
                }
            } else if ch == '&' && i + 1 < chars.len() && chars[i + 1].1 == '&' {
                if byte_pos > start {
                    segments.push(&command[start..byte_pos]);
                }
                i += 1;
                start = chars[i].0 + 1;
            }
        }
        i += 1;
    }

    if start < command.len() {
        segments.push(&command[start..]);
    }

    segments
}

fn contains_blocked_command_token(segment: &str, blocked: &str) -> bool {
    segment
        .split(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    ';' | '|' | '&' | '(' | ')' | '\'' | '"' | '`' | '<' | '>'
                )
        })
        .filter(|token| !token.is_empty())
        .any(|token| {
            let normalized =
                token.trim_matches(|ch: char| matches!(ch, '\'' | '"' | '`' | ',' | ';'));
            if normalized.is_empty() {
                return false;
            }
            let base = normalized.rsplit('/').next().unwrap_or(normalized);
            base == blocked
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_guard_preserves_allowed_and_blocked_security_cases() {
        let blocked_commands = [
            "env",
            "env | grep API",
            "env | sort",
            "echo hello; env",
            "echo hello && env",
            "echo hello || env",
            "echo ok\nenv",
            "printenv",
            "printenv API_KEY",
            "cat file | printenv",
            "cat /proc/self/environ",
            "cat /proc/1/environ",
            "strings /proc/self/environ | grep KEY",
            "cat /proc/self/mem",
            "cat /proc/self/maps",
            "cat /proc/self/fd/3",
            "cat /proc/self/cmdline",
            "cat /proc/42/mem",
            "cat /proc/123/maps",
            "set",
            "set  ",
            "echo hi; set",
            "echo ok\nset",
            "bash -c 'env'",
            "bash -c '/usr/bin/env | sort'",
            "eval \"printenv\"",
            "env | grep -E '(MODEL|MODEL_NAME|LLM|OPENAI|API)' | sort",
        ];
        for command in blocked_commands {
            assert!(check_command(command).is_err(), "should block: {command}");
        }

        let allowed_commands = [
            "echo hello",
            "ls -la",
            "cat file.txt | grep pattern",
            "cargo build --release",
            "set -e",
            "set -o pipefail",
            "set -euxo pipefail",
            "set +x",
            "echo 'environment'",
            "echo \"the env variable\"",
            "bash -c 'echo event'",
            "bash -c 'echo printenvy'",
            "echo $HOME",
            "echo $OPENAI_API_KEY",
            "printf '%s' \"$API_KEY\"",
        ];
        for command in allowed_commands {
            assert!(check_command(command).is_ok(), "should allow: {command}");
        }
    }

    #[test]
    fn tokenizer_splits_shell_operators() {
        let cases = [
            (
                "echo hello | grep world",
                vec!["echo", "hello", "grep", "world"],
            ),
            (
                "echo a ; echo b ; echo c",
                vec!["echo", "a", "echo", "b", "echo", "c"],
            ),
            (
                "echo a && echo b || echo c",
                vec!["echo", "a", "echo", "b", "echo", "c"],
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(tokenize(input), expected);
        }
    }
}

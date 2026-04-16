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

/// `env`・`printenv` の実行をブロックする。
///
/// パイプやセミコロンで繋がれた場合も検知するため、
/// コマンド文字列内のすべてのトークンを検査する。
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
        let next_is_option = words.get(1).is_some_and(|w| w.starts_with('-'));
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
    let chars: Vec<char> = command.chars().collect();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if !in_single_quote && !in_double_quote {
            if ch == ';' {
                if i > start {
                    segments.push(&command[start..i]);
                }
                start = i + 1;
            }
            // | はパイプ区切り（|| も処理）
            else if ch == '|' {
                if i > start {
                    segments.push(&command[start..i]);
                }
                start = i + 1;
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    i += 1;
                    start = i + 1;
                }
            } else if ch == '&' && i + 1 < chars.len() && chars[i + 1] == '&' {
                if i > start {
                    segments.push(&command[start..i]);
                }
                i += 1;
                start = i + 1;
            }
        }
        i += 1;
    }

    if start < command.len() {
        segments.push(&command[start..]);
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_normal_commands() {
        assert!(check_command("echo hello").is_ok());
        assert!(check_command("ls -la").is_ok());
        assert!(check_command("cat file.txt | grep pattern").is_ok());
        assert!(check_command("cargo build --release").is_ok());
    }

    #[test]
    fn blocks_env_command() {
        assert!(check_command("env").is_err());
        assert!(check_command("env | grep API").is_err());
        assert!(check_command("env | sort").is_err());
        assert!(check_command("echo hello; env").is_err());
        assert!(check_command("echo hello && env").is_err());
        assert!(check_command("echo hello || env").is_err());
    }

    #[test]
    fn blocks_printenv() {
        assert!(check_command("printenv").is_err());
        assert!(check_command("printenv API_KEY").is_err());
        assert!(check_command("cat file | printenv").is_err());
    }

    #[test]
    fn blocks_proc_self_environ() {
        assert!(check_command("cat /proc/self/environ").is_err());
        assert!(check_command("cat /proc/1/environ").is_err());
        assert!(check_command("strings /proc/self/environ | grep KEY").is_err());
    }

    #[test]
    fn blocks_proc_self_mem() {
        assert!(check_command("cat /proc/self/mem").is_err());
        assert!(check_command("cat /proc/self/maps").is_err());
        assert!(check_command("cat /proc/self/fd/3").is_err());
        assert!(check_command("cat /proc/self/cmdline").is_err());
    }

    #[test]
    fn blocks_proc_pid_access() {
        assert!(check_command("cat /proc/42/mem").is_err());
        assert!(check_command("cat /proc/123/maps").is_err());
    }

    #[test]
    fn blocks_bare_set() {
        assert!(check_command("set").is_err());
        assert!(check_command("set  ").is_err());
        assert!(check_command("echo hi; set").is_err());
    }

    #[test]
    fn allows_set_with_options() {
        assert!(check_command("set -e").is_ok());
        assert!(check_command("set -o pipefail").is_ok());
        assert!(check_command("set -euxo pipefail").is_ok());
    }

    #[test]
    fn allows_env_in_quotes() {
        assert!(check_command("echo 'environment'").is_ok());
        assert!(check_command("echo \"the env variable\"").is_ok());
    }

    #[test]
    fn allows_echo_specific_var() {
        assert!(check_command("echo $HOME").is_ok());
        assert!(check_command("echo $OPENAI_API_KEY").is_ok());
        assert!(check_command("printf '%s' \"$API_KEY\"").is_ok());
    }

    #[test]
    fn tokenize_splits_pipes() {
        let tokens = tokenize("echo hello | grep world");
        assert_eq!(tokens, vec!["echo", "hello", "grep", "world"]);
    }

    #[test]
    fn tokenize_splits_semicolons() {
        let tokens = tokenize("echo a ; echo b ; echo c");
        assert_eq!(tokens, vec!["echo", "a", "echo", "b", "echo", "c"]);
    }

    #[test]
    fn tokenize_splits_logical_operators() {
        let tokens = tokenize("echo a && echo b || echo c");
        assert_eq!(tokens, vec!["echo", "a", "echo", "b", "echo", "c"]);
    }

    #[test]
    fn blocks_original_attack_commands() {
        assert!(check_command("env | grep -E '(MODEL|MODEL_NAME|LLM|OPENAI|API)' | sort").is_err());
        assert!(check_command("cat /proc/self/environ").is_err());
    }
}

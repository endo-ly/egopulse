//! SecretRef 型と解決ロジック。
//!
//! YAML 内の任意の文字列フィールドを `{ source: env, id: VAR }` や `{ source: exec, command: "..." }` で
//! 外部シークレット参照として記述できるようにする。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use secrecy::SecretString;
use serde::Deserialize;

use crate::error::ConfigError;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "source", rename_all = "lowercase")]
pub(crate) enum SecretSource {
    Env {
        #[serde(default)]
        id: Option<String>,
    },
    Exec {
        #[serde(default)]
        command: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(crate) enum StringOrRef {
    Literal(String),
    Ref(SecretSource),
}

#[derive(Clone, Debug)]
pub enum ResolvedValue {
    Literal(String),
    EnvRef { value: String, id: String },
    ExecRef { value: String, command: String },
}

impl ResolvedValue {
    pub(crate) fn value(&self) -> &str {
        match self {
            Self::Literal(v) | Self::EnvRef { value: v, .. } | Self::ExecRef { value: v, .. } => v,
        }
    }

    pub(crate) fn to_secret_string(&self) -> SecretString {
        SecretString::new(self.value().to_string().into_boxed_str())
    }

    pub(crate) fn to_yaml_value(&self) -> serde_yml::Value {
        match self {
            Self::Literal(v) => serde_yml::Value::String(v.clone()),
            Self::EnvRef { id, .. } => {
                let mut mapping = serde_yml::Mapping::new();
                mapping.insert(
                    serde_yml::Value::String("source".into()),
                    serde_yml::Value::String("env".into()),
                );
                mapping.insert(
                    serde_yml::Value::String("id".into()),
                    serde_yml::Value::String(id.clone()),
                );
                serde_yml::Value::Mapping(mapping)
            }
            Self::ExecRef { command, .. } => {
                let mut mapping = serde_yml::Mapping::new();
                mapping.insert(
                    serde_yml::Value::String("source".into()),
                    serde_yml::Value::String("exec".into()),
                );
                mapping.insert(
                    serde_yml::Value::String("command".into()),
                    serde_yml::Value::String(command.clone()),
                );
                serde_yml::Value::Mapping(mapping)
            }
        }
    }
}

pub(crate) fn resolve_string_or_ref(
    value: Option<StringOrRef>,
    dotenv: &HashMap<String, String>,
) -> Result<Option<ResolvedValue>, ConfigError> {
    let Some(inner) = value else { return Ok(None) };

    match inner {
        StringOrRef::Literal(s) => {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(ResolvedValue::Literal(trimmed)))
            }
        }
        StringOrRef::Ref(source) => resolve_secret_ref(source, dotenv).map(Some),
    }
}

fn resolve_secret_ref(
    source: SecretSource,
    dotenv: &HashMap<String, String>,
) -> Result<ResolvedValue, ConfigError> {
    match source {
        SecretSource::Env { id: Some(id) } => {
            let id = id.trim().to_string();
            if id.is_empty() {
                return Err(ConfigError::SecretRefUnresolved {
                    reference: "env source with empty id".into(),
                });
            }
            if let Ok(val) = std::env::var(&id) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    return Ok(ResolvedValue::EnvRef { value: trimmed, id });
                }
            }
            if let Some(val) = dotenv.get(&id) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    return Ok(ResolvedValue::EnvRef { value: trimmed, id });
                }
            }
            Err(ConfigError::SecretRefUnresolved {
                reference: format!("env:{id}"),
            })
        }
        SecretSource::Env { id: None } => Err(ConfigError::SecretRefUnresolved {
            reference: "env source missing id".into(),
        }),
        SecretSource::Exec {
            command: Some(command),
        } => {
            let command = command.trim().to_string();
            if command.is_empty() {
                return Err(ConfigError::SecretRefUnresolved {
                    reference: "exec source with empty command".into(),
                });
            }
            run_exec_command(&command)
        }
        SecretSource::Exec { command: None } => Err(ConfigError::SecretRefUnresolved {
            reference: "exec source missing command".into(),
        }),
    }
}

fn run_exec_command(command: &str) -> Result<ResolvedValue, ConfigError> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|e| ConfigError::SecretRefExecFailed {
            command: command.to_string(),
            detail: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConfigError::SecretRefExecFailed {
            command: command.to_string(),
            detail: format!("exit {}: {stderr}", output.status),
        });
    }

    let value = String::from_utf8(output.stdout)
        .map_err(|e| ConfigError::SecretRefExecFailed {
            command: command.to_string(),
            detail: e.to_string(),
        })?
        .trim()
        .to_string();

    if value.is_empty() {
        return Err(ConfigError::SecretRefExecFailed {
            command: command.to_string(),
            detail: "command produced empty output".into(),
        });
    }

    Ok(ResolvedValue::ExecRef {
        value,
        command: command.to_string(),
    })
}

pub(crate) fn read_dotenv(path: &Path) -> HashMap<String, String> {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };

    parse_dotenv_contents(&contents)
}

fn parse_dotenv_contents(contents: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if !key.is_empty() {
                map.insert(key, value);
            }
        }
    }
    map
}

pub(crate) fn dotenv_path(config_dir: &Path) -> std::path::PathBuf {
    config_dir.join(".env")
}

pub(crate) fn save_dotenv(path: &Path, entries: &[(String, String)]) -> Result<(), ConfigError> {
    let mut existing = read_dotenv(path);

    for (key, value) in entries {
        existing.insert(key.clone(), value.clone());
    }

    let parent = path.parent().ok_or_else(|| ConfigError::ConfigNotFound {
        path: path.to_path_buf(),
    })?;
    fs::create_dir_all(parent).map_err(|source| ConfigError::ConfigReadFailed {
        path: parent.to_path_buf(),
        source,
    })?;

    let mut lines: Vec<String> = existing
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    lines.sort();

    let content = format!("{}\n", lines.join("\n"));

    let mut opts = fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|source| ConfigError::ConfigReadFailed {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(content.as_bytes())
        .map_err(|source| ConfigError::ConfigReadFailed {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvVarGuard;
    use serial_test::serial;
    use std::io::Write as IoWrite;

    #[test]
    #[serial]
    fn secret_ref_env_resolves_from_process_env() {
        let _guard = EnvVarGuard::set("EGOPULSE_TEST_SECRET_1", "from-process");
        let dotenv = HashMap::new();

        let source = SecretSource::Env {
            id: Some("EGOPULSE_TEST_SECRET_1".into()),
        };
        let result = resolve_secret_ref(source, &dotenv).expect("resolve");
        assert_eq!(result.value(), "from-process");
    }

    #[test]
    #[serial]
    fn secret_ref_env_resolves_from_dotenv() {
        let _guard = EnvVarGuard::set("EGOPULSE_TEST_SECRET_2_DOTENV", "");

        let mut dotenv = HashMap::new();
        dotenv.insert(
            "EGOPULSE_TEST_SECRET_2_DOTENV".to_string(),
            "from-dotenv".to_string(),
        );

        let source = SecretSource::Env {
            id: Some("EGOPULSE_TEST_SECRET_2_DOTENV".into()),
        };
        let result = resolve_secret_ref(source, &dotenv).expect("resolve from dotenv");
        assert_eq!(result.value(), "from-dotenv");
    }

    #[test]
    #[serial]
    fn secret_ref_env_prefers_process_env_over_dotenv() {
        let _guard = EnvVarGuard::set("EGOPULSE_TEST_SECRET_3", "from-process");
        let mut dotenv = HashMap::new();
        dotenv.insert(
            "EGOPULSE_TEST_SECRET_3".to_string(),
            "from-dotenv".to_string(),
        );

        let source = SecretSource::Env {
            id: Some("EGOPULSE_TEST_SECRET_3".into()),
        };
        let result = resolve_secret_ref(source, &dotenv).expect("resolve");
        assert_eq!(result.value(), "from-process");
    }

    #[test]
    #[serial]
    fn secret_ref_env_unresolved_returns_error() {
        let _guard = EnvVarGuard::set("EGOPULSE_TEST_SECRET_4_MISSING", "");
        let dotenv = HashMap::new();

        let source = SecretSource::Env {
            id: Some("EGOPULSE_TEST_SECRET_4_MISSING".into()),
        };
        let err = resolve_secret_ref(source, &dotenv).expect_err("should fail");
        assert!(
            matches!(err, ConfigError::SecretRefUnresolved { .. }),
            "expected SecretRefUnresolved, got {err:?}"
        );
    }

    #[test]
    #[serial]
    fn secret_ref_exec_captures_stdout() {
        let dotenv = HashMap::new();
        let source = SecretSource::Exec {
            command: Some("echo hello-world".into()),
        };
        let result = resolve_secret_ref(source, &dotenv).expect("resolve exec");
        assert_eq!(result.value(), "hello-world");
    }

    #[test]
    #[serial]
    fn secret_ref_exec_failure_returns_error() {
        let dotenv = HashMap::new();
        let source = SecretSource::Exec {
            command: Some("false".into()),
        };
        let err = resolve_secret_ref(source, &dotenv).expect_err("should fail");
        assert!(
            matches!(err, ConfigError::SecretRefExecFailed { .. }),
            "expected SecretRefExecFailed, got {err:?}"
        );
    }

    #[test]
    #[serial]
    fn secret_ref_literal_value_unchanged() {
        let dotenv = HashMap::new();
        let result =
            resolve_string_or_ref(Some(StringOrRef::Literal("my-api-key".into())), &dotenv)
                .expect("resolve")
                .expect("some");
        assert_eq!(result.value(), "my-api-key");
    }

    #[test]
    fn resolved_value_to_yaml_restores_env_ref() {
        let rv = ResolvedValue::EnvRef {
            value: "secret".into(),
            id: "OPENAI_API_KEY".into(),
        };
        let yaml_val = rv.to_yaml_value();

        let mapping = yaml_val.as_mapping().expect("mapping");
        assert_eq!(
            mapping.get(serde_yml::Value::String("source".into())),
            Some(&serde_yml::Value::String("env".into()))
        );
        assert_eq!(
            mapping.get(serde_yml::Value::String("id".into())),
            Some(&serde_yml::Value::String("OPENAI_API_KEY".into()))
        );
    }

    #[test]
    fn resolved_value_to_yaml_restores_exec_ref() {
        let rv = ResolvedValue::ExecRef {
            value: "secret".into(),
            command: "pass show discord/token".into(),
        };
        let yaml_val = rv.to_yaml_value();

        let mapping = yaml_val.as_mapping().expect("mapping");
        assert_eq!(
            mapping.get(serde_yml::Value::String("source".into())),
            Some(&serde_yml::Value::String("exec".into()))
        );
        assert_eq!(
            mapping.get(serde_yml::Value::String("command".into())),
            Some(&serde_yml::Value::String("pass show discord/token".into()))
        );
    }

    #[test]
    fn read_dotenv_parses_key_value_pairs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");
        let mut f = fs::File::create(&path).expect("create");
        write!(f, "KEY1=value1\n# comment\n\nKEY2=value2\n").expect("write");

        let map = read_dotenv(&path);
        assert_eq!(map.get("KEY1").map(|s| s.as_str()), Some("value1"));
        assert_eq!(map.get("KEY2").map(|s| s.as_str()), Some("value2"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn save_dotenv_preserves_unrelated_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");

        save_dotenv(&path, &[("EXISTING_KEY".into(), "existing-value".into())])
            .expect("save first");

        save_dotenv(&path, &[("NEW_KEY".into(), "new-value".into())]).expect("save second");

        let map = read_dotenv(&path);
        assert_eq!(
            map.get("EXISTING_KEY").map(|s| s.as_str()),
            Some("existing-value")
        );
        assert_eq!(map.get("NEW_KEY").map(|s| s.as_str()), Some("new-value"));
    }

    #[test]
    fn save_dotenv_creates_with_0600_permissions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");

        save_dotenv(&path, &[("TEST_KEY".into(), "test-value".into())]).expect("save");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(&path).expect("metadata");
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        }
    }
}

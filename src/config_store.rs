//! 設定 YAML の排他更新と安全な永続化を扱うモジュール。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use serde_yml::{Mapping, Value};

use crate::error::EgoPulseError;

static CONFIG_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub fn read_yaml(path: &Path) -> Result<Value, EgoPulseError> {
    if !path.exists() {
        return Ok(Value::Mapping(Mapping::new()));
    }

    let raw =
        fs::read_to_string(path).map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    let parsed: Value =
        serde_yml::from_str(&raw).map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    Ok(match parsed {
        Value::Mapping(_) => parsed,
        _ => Value::Mapping(Mapping::new()),
    })
}

pub fn update_yaml<T, F>(path: &Path, update: F) -> Result<T, EgoPulseError>
where
    F: FnOnce(&mut Value) -> Result<T, EgoPulseError>,
{
    let _guard = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| EgoPulseError::Internal("config write lock poisoned".to_string()))?;
    let mut root = read_yaml(path)?;
    let result = update(&mut root)?;
    write_yaml_atomically(path, &root)?;
    Ok(result)
}

pub fn write_yaml_atomically(path: &Path, root: &Value) -> Result<(), EgoPulseError> {
    let parent = path
        .parent()
        .ok_or_else(|| EgoPulseError::Internal("config path has no parent".to_string()))?;
    fs::create_dir_all(parent).map_err(|error| EgoPulseError::Internal(error.to_string()))?;

    let yaml =
        serde_yml::to_string(root).map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    let temp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("egopulse.config.yaml"),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let mut temp_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    temp_file
        .write_all(yaml.as_bytes())
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    temp_file
        .flush()
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    temp_file
        .sync_all()
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    drop(temp_file);

    fs::rename(&temp_path, path).map_err(|error| EgoPulseError::Internal(error.to_string()))?;

    if let Ok(dir) = OpenOptions::new().read(true).open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

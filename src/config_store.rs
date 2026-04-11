//! 設定 YAML の排他更新と安全な永続化を扱うモジュール。
//!
//! このモジュールは旧式の `serde_yml::Value` ベースの操作を提供する。
//! 新規の保存処理では `Config::save_yaml()` を使用すること。

#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use fs2::FileExt;
use serde_yml::Value;

use crate::error::EgoPulseError;

static CONFIG_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub fn read_yaml(path: &Path) -> Result<Value, EgoPulseError> {
    let _guard = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| EgoPulseError::Internal("config write lock poisoned".to_string()))?;
    let _lock_file = acquire_config_lock(path)?;
    read_yaml_unlocked(path)
}

pub fn update_yaml<T, F>(path: &Path, update: F) -> Result<T, EgoPulseError>
where
    F: FnOnce(&mut Value) -> Result<T, EgoPulseError>,
{
    let _guard = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| EgoPulseError::Internal("config write lock poisoned".to_string()))?;
    let _lock_file = acquire_config_lock(path)?;
    let mut root = read_yaml_unlocked(path)?;
    let result = update(&mut root)?;
    write_yaml_atomically(path, &root)?;
    Ok(result)
}

fn read_yaml_unlocked(path: &Path) -> Result<Value, EgoPulseError> {
    if !path.exists() {
        return Ok(serde_yml::Value::Mapping(Default::default()));
    }

    let raw =
        fs::read_to_string(path).map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    let parsed: Value =
        serde_yml::from_str(&raw).map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    match parsed {
        Value::Mapping(_) => Ok(parsed),
        _ => Err(EgoPulseError::Internal(format!(
            "config root is not a mapping: {}",
            path.display()
        ))),
    }
}

fn acquire_config_lock(path: &Path) -> Result<File, EgoPulseError> {
    let lock_path = lock_file_path(path)?;
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    }

    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    lock_file
        .lock_exclusive()
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    Ok(lock_file)
}

fn lock_file_path(path: &Path) -> Result<PathBuf, EgoPulseError> {
    let parent = path
        .parent()
        .ok_or_else(|| EgoPulseError::Internal("config path has no parent".to_string()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("egopulse.config.yaml");
    Ok(parent.join(format!(".{file_name}.lock")))
}

fn write_yaml_atomically(path: &Path, root: &Value) -> Result<(), EgoPulseError> {
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

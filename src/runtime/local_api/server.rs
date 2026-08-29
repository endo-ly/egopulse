//! Unix socket listener and connection lifecycle for the local runtime API.

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

use crate::error::EgoPulseError;
use crate::runtime::{AppState, Criticality, TaskKind, TaskSpec};

use super::service;
use super::socket_path;

pub(crate) fn start(state: &Arc<AppState>) -> Result<(), EgoPulseError> {
    let path = socket_path(std::path::Path::new(&state.config.state_root));
    let listener = bind_listener(&path)?;
    let shutdown = state.supervisor.shutdown_started_token();
    let supervisor = Arc::clone(&state.supervisor);
    let runtime = Arc::clone(state);
    supervisor.spawn_long_lived(
        TaskSpec::new(TaskKind::LocalApi, "local-api", Criticality::Critical),
        serve(listener, path, runtime, shutdown),
    );
    Ok(())
}

fn bind_listener(path: &std::path::Path) -> Result<UnixListener, EgoPulseError> {
    let parent = path.parent().ok_or_else(|| {
        EgoPulseError::RuntimeLocalApi("runtime socket has no parent directory".to_string())
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        EgoPulseError::RuntimeLocalApi(format!(
            "failed to create runtime socket directory {}: {error}",
            parent.display()
        ))
    })?;

    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_socket() {
            std::fs::remove_file(path).map_err(|error| {
                EgoPulseError::RuntimeLocalApi(format!(
                    "failed to remove stale runtime socket {}: {error}",
                    path.display()
                ))
            })?;
        } else {
            return Err(EgoPulseError::RuntimeLocalApi(format!(
                "runtime socket path is occupied by a non-socket entry: {}",
                path.display()
            )));
        }
    }

    let listener = UnixListener::bind(path).map_err(|error| {
        EgoPulseError::RuntimeLocalApi(format!(
            "failed to bind runtime socket {}: {error}",
            path.display()
        ))
    })?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|error| {
        EgoPulseError::RuntimeLocalApi(format!(
            "failed to secure runtime socket {}: {error}",
            path.display()
        ))
    })?;
    Ok(listener)
}

async fn serve(
    listener: UnixListener,
    path: PathBuf,
    state: Arc<AppState>,
    shutdown: CancellationToken,
) -> Result<(), EgoPulseError> {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|error| {
                    EgoPulseError::RuntimeLocalApi(format!("runtime socket accept failed: {error}"))
                })?;
                let connection_state = Arc::clone(&state);
                let connection_shutdown = shutdown.clone();
                tokio::spawn(async move {
                    if let Err(error) =
                        service::handle_connection(stream, connection_state, connection_shutdown).await
                    {
                        tracing::debug!(error = %error, "local runtime API connection ended with an error");
                    }
                });
            }
        }
    }

    cleanup_socket(&path);
    Ok(())
}

fn cleanup_socket(path: &std::path::Path) {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            if let Err(error) = std::fs::remove_file(path) {
                tracing::warn!(socket = %path.display(), %error, "failed to remove runtime socket");
            }
        }
        Ok(_) => {
            tracing::warn!(socket = %path.display(), "runtime socket path changed during shutdown; refusing to remove non-socket entry")
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            tracing::warn!(socket = %path.display(), %error, "failed to inspect runtime socket during shutdown");
        }
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::net::UnixStream;

    use super::{bind_listener, socket_path, start};

    #[test]
    fn regular_file_at_socket_path_is_not_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("egopulse.sock");
        std::fs::write(&path, "keep").expect("write marker");

        let result = bind_listener(&path);

        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(path).expect("marker remains"),
            "keep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_at_socket_path_is_not_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        let path = dir.path().join("egopulse.sock");
        std::fs::write(&target, "keep").expect("write target");
        std::os::unix::fs::symlink(&target, &path).expect("create symlink");

        let result = bind_listener(&path);

        assert!(result.is_err());
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("link remains")
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_socket_is_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("egopulse.sock");
        let stale = std::os::unix::net::UnixListener::bind(&path).expect("bind stale socket");
        drop(stale);

        let listener = bind_listener(&path).expect("replace stale socket");

        drop(listener);
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_permissions_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("egopulse.sock");

        let listener = bind_listener(&path).expect("bind socket");

        assert_eq!(
            std::fs::metadata(&path)
                .expect("socket metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        drop(listener);
    }

    #[tokio::test]
    async fn idle_connection_does_not_delay_runtime_shutdown() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        config.channels.clear();
        let state = Arc::new(crate::test_util::build_state_with_config(
            config, None, None, None, None,
        ));
        start(&state).expect("start local API");
        let path = socket_path(std::path::Path::new(&state.config.state_root));
        let _connection = UnixStream::connect(&path).await.expect("connect socket");

        // Act
        let shutdown =
            tokio::time::timeout(Duration::from_secs(1), state.supervisor.shutdown()).await;

        // Assert
        assert!(shutdown.is_ok(), "idle connection must not block shutdown");
        assert!(!path.exists(), "runtime socket must be cleaned up");
        assert!(
            UnixStream::connect(&path).await.is_err(),
            "shutdown must reject new local API connections"
        );
    }
}

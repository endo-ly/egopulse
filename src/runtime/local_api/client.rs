//! Client for the runtime's local Unix socket API.

use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, ReadHalf};
use tokio::net::UnixStream;

use crate::error::EgoPulseError;

use super::PROTOCOL_VERSION;
use super::protocol::{
    CommandOutcome, Request, RequestEnvelope, Response, ResponseEnvelope, SessionReference,
    SessionSummary, SessionView,
};

#[derive(Clone, Debug)]
pub(crate) struct LocalRuntimeClient {
    socket_path: PathBuf,
}

impl LocalRuntimeClient {
    pub(crate) async fn connect(socket_path: PathBuf) -> Result<Self, EgoPulseError> {
        let client = Self { socket_path };
        let info = client.runtime_info().await?;
        if info.protocol_version != PROTOCOL_VERSION {
            return Err(EgoPulseError::RuntimeProtocolMismatch {
                expected: PROTOCOL_VERSION,
                actual: info.protocol_version,
            });
        }
        tracing::debug!(
            version = %info.egopulse_version,
            "connected to local EgoPulse runtime"
        );
        Ok(client)
    }

    pub(crate) async fn runtime_info(&self) -> Result<RuntimeInfo, EgoPulseError> {
        let mut lines = self.request(Request::RuntimeInfo).await?;
        match read_response(&mut lines).await? {
            Response::RuntimeInfo {
                protocol_version,
                egopulse_version,
            } => Ok(RuntimeInfo {
                protocol_version,
                egopulse_version,
            }),
            other => Err(unexpected_response("runtime_info", other)),
        }
    }

    pub(crate) async fn list_sessions(&self) -> Result<Vec<SessionSummary>, EgoPulseError> {
        let mut lines = self.request(Request::ListSessions).await?;
        match read_response(&mut lines).await? {
            Response::Sessions { sessions } => Ok(sessions),
            other => Err(unexpected_response("list_sessions", other)),
        }
    }

    pub(crate) async fn open_session(
        &self,
        session: SessionReference,
    ) -> Result<SessionView, EgoPulseError> {
        let mut lines = self.request(Request::OpenSession { session }).await?;
        match read_response(&mut lines).await? {
            Response::Session { session } => Ok(session),
            other => Err(unexpected_response("open_session", other)),
        }
    }

    pub(crate) async fn execute_command(
        &self,
        session: SessionReference,
        text: String,
    ) -> Result<CommandResult, EgoPulseError> {
        let mut lines = self
            .request(Request::ExecuteCommand {
                session,
                text,
                sender_id: Some("local_user".to_string()),
            })
            .await?;
        match read_response(&mut lines).await? {
            Response::CommandFinished {
                outcome,
                effective_provider,
                effective_model,
            } => Ok(CommandResult {
                outcome,
                effective_provider,
                effective_model,
            }),
            other => Err(unexpected_response("execute_command", other)),
        }
    }

    pub(crate) async fn stage_followup(
        &self,
        session: SessionReference,
        input: String,
    ) -> Result<super::protocol::FollowupOutcome, EgoPulseError> {
        let mut lines = self
            .request(Request::StageFollowup { session, input })
            .await?;
        match read_response(&mut lines).await? {
            Response::FollowupFinished { outcome } => Ok(outcome),
            other => Err(unexpected_response("stage_followup", other)),
        }
    }

    pub(crate) async fn execute_turn<F>(
        &self,
        session: SessionReference,
        prompt: String,
        on_event: F,
    ) -> Result<String, EgoPulseError>
    where
        F: Fn(super::protocol::TurnEvent) + Send + Sync + 'static,
    {
        let mut lines = self
            .request(Request::ExecuteTurn { session, prompt })
            .await?;
        loop {
            match read_response(&mut lines).await? {
                Response::TurnEvent { event } => on_event(event),
                Response::TurnFinished { response } => return Ok(response),
                other => return Err(unexpected_response("execute_turn", other)),
            }
        }
    }

    async fn request(
        &self,
        request: Request,
    ) -> Result<Lines<BufReader<ReadHalf<UnixStream>>>, EgoPulseError> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|error| map_connect_error(&self.socket_path, error))?;
        let (read_half, mut write_half) = tokio::io::split(stream);
        let envelope = serde_json::to_vec(&RequestEnvelope::new(request))
            .map_err(|error| EgoPulseError::RuntimeLocalApi(error.to_string()))?;
        write_half
            .write_all(&envelope)
            .await
            .map_err(|error| EgoPulseError::RuntimeLocalApi(error.to_string()))?;
        write_half
            .write_all(b"\n")
            .await
            .map_err(|error| EgoPulseError::RuntimeLocalApi(error.to_string()))?;
        write_half
            .flush()
            .await
            .map_err(|error| EgoPulseError::RuntimeLocalApi(error.to_string()))?;
        Ok(BufReader::new(read_half).lines())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeInfo {
    pub(crate) protocol_version: u32,
    pub(crate) egopulse_version: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CommandResult {
    pub(crate) outcome: CommandOutcome,
    pub(crate) effective_provider: String,
    pub(crate) effective_model: String,
}

async fn read_response(
    lines: &mut Lines<BufReader<ReadHalf<UnixStream>>>,
) -> Result<Response, EgoPulseError> {
    let line = lines
        .next_line()
        .await
        .map_err(|error| EgoPulseError::RuntimeLocalApi(error.to_string()))?
        .ok_or_else(|| {
            EgoPulseError::RuntimeLocalApi("runtime closed the connection".to_string())
        })?;
    let envelope: ResponseEnvelope = serde_json::from_str(&line)
        .map_err(|error| EgoPulseError::RuntimeLocalApi(format!("invalid response: {error}")))?;
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err(EgoPulseError::RuntimeProtocolMismatch {
            expected: PROTOCOL_VERSION,
            actual: envelope.protocol_version,
        });
    }
    match envelope.response {
        Response::ProtocolMismatch { expected, actual } => {
            Err(EgoPulseError::RuntimeProtocolMismatch { expected, actual })
        }
        Response::Error { message } => Err(EgoPulseError::RuntimeLocalApi(message)),
        response => Ok(response),
    }
}

fn map_connect_error(path: &std::path::Path, error: std::io::Error) -> EgoPulseError {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            tracing::debug!(socket = %path.display(), error = %error, "runtime socket unavailable");
            EgoPulseError::RuntimeUnavailable
        }
        _ => EgoPulseError::RuntimeLocalApi(format!(
            "failed to connect to {}: {error}",
            path.display()
        )),
    }
}

fn unexpected_response(operation: &str, response: Response) -> EgoPulseError {
    EgoPulseError::RuntimeLocalApi(format!("unexpected response for {operation}: {response:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::local_api::protocol::{Response, ResponseEnvelope};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn missing_socket_reports_runtime_unavailable() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing.sock");

        // Act
        let error = LocalRuntimeClient::connect(path)
            .await
            .expect_err("must fail");

        // Assert
        assert!(matches!(error, EgoPulseError::RuntimeUnavailable));
    }

    #[tokio::test]
    async fn protocol_mismatch_is_rejected_during_connect() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("egopulse.sock");
        let listener = UnixListener::bind(&path).expect("bind socket");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            lines.next_line().await.expect("read request");
            let response = ResponseEnvelope {
                protocol_version: PROTOCOL_VERSION,
                response: Response::ProtocolMismatch {
                    expected: PROTOCOL_VERSION,
                    actual: PROTOCOL_VERSION + 1,
                },
            };
            let mut bytes = serde_json::to_vec(&response).expect("encode response");
            bytes.push(b'\n');
            write_half.write_all(&bytes).await.expect("write response");
        });

        // Act
        let error = LocalRuntimeClient::connect(path)
            .await
            .expect_err("must reject");
        server.await.expect("join server");

        // Assert
        assert!(matches!(
            error,
            EgoPulseError::RuntimeProtocolMismatch {
                expected: PROTOCOL_VERSION,
                actual: value,
            } if value == PROTOCOL_VERSION + 1
        ));
    }
}

//! Private IPC over Unix domain sockets. Never TCP.
//!
//! Framing: one JSON object per line (NDJSON), request then response.

use std::path::Path;
use std::sync::Arc;

use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::protocol::InternalProtocolVersion;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::paths::set_mode_0600;
use crate::{InternalRequest, InternalResponse, Runtime};

/// Bind the private Unix listener for the desktop-owned runtime.
pub async fn bind_private_listener(socket_path: &Path) -> LcuResult<UnixListener> {
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }
    let listener = UnixListener::bind(socket_path).map_err(|e| {
        LcuError::coded(
            ErrorCode::InternalError,
            format!("bind private socket {}: {e}", socket_path.display()),
        )
    })?;
    set_mode_0600(socket_path)?;
    Ok(listener)
}

/// Serve requests until the process is cancelled. One connection = one request/response for M0.
pub async fn serve_forever(runtime: Arc<Runtime>, listener: UnixListener) -> LcuResult<()> {
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("accept: {e}")))?;
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(runtime, stream).await {
                tracing::warn!(error = %err, "private ipc connection failed");
            }
        });
    }
}

async fn handle_connection(runtime: Arc<Runtime>, stream: UnixStream) -> LcuResult<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("read request: {e}")))?
    else {
        return Ok(());
    };

    let request: InternalRequest = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            // The Chrome native-messaging host pushes fire-and-forget event
            // frames ({type:...}, no method) to announce control-state changes.
            // They are not InternalRequests; answering them is impossible and
            // they must not spam InvalidRequest warnings. The host closes the
            // connection right after writing, so no response is expected.
            if line.contains("\"type\"") && !line.contains("\"method\"") {
                tracing::debug!(line_len = line.len(), "ignoring non-request event frame");
                return Ok(());
            }
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                format!("bad request json: {e}"),
            ));
        }
    };
    let response = runtime.handle_internal(request);
    let mut out = serde_json::to_string(&response).map_err(|e| {
        LcuError::coded(ErrorCode::InternalError, format!("serialize response: {e}"))
    })?;
    out.push('\n');
    writer
        .write_all(out.as_bytes())
        .await
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("write response: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("flush response: {e}")))?;
    Ok(())
}

/// Client helper used by `lcu` CLI against the desktop-owned socket.
pub async fn call_runtime(
    socket_path: &Path,
    request: InternalRequest,
) -> LcuResult<InternalResponse> {
    // Reject protocol skew early on the client for clearer errors.
    if let InternalRequest::Ping { protocol_version } = &request {
        let remote = InternalProtocolVersion(*protocol_version);
        if !InternalProtocolVersion::CURRENT.is_compatible(remote) {
            return Ok(InternalResponse::Error {
                code: ErrorCode::ProtocolMismatch,
                message: format!(
                    "client protocol {}, incompatible with request {protocol_version}",
                    InternalProtocolVersion::CURRENT.0
                ),
            });
        }
    }

    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        LcuError::coded(
            ErrorCode::RuntimeUnavailable,
            format!(
                "runtime socket {} unavailable: {e}; start lcu-desktop first",
                socket_path.display()
            ),
        )
    })?;

    let (reader, mut writer) = stream.into_split();
    let mut payload = serde_json::to_string(&request).map_err(|e| {
        LcuError::coded(ErrorCode::InternalError, format!("serialize request: {e}"))
    })?;
    payload.push('\n');
    writer
        .write_all(payload.as_bytes())
        .await
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("write request: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("flush request: {e}")))?;

    let mut lines = BufReader::new(reader).lines();
    let line = lines
        .next_line()
        .await
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("read response: {e}")))?
        .ok_or_else(|| {
            LcuError::coded(ErrorCode::RuntimeUnavailable, "runtime closed connection")
        })?;
    serde_json::from_str(&line)
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("parse response: {e}")))
}

/// Synchronous wrapper for the CLI binary.
pub fn call_runtime_blocking(
    socket_path: &Path,
    request: InternalRequest,
) -> LcuResult<InternalResponse> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("tokio: {e}")))?;
    rt.block_on(call_runtime(socket_path, request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::RuntimePaths;
    use lcu_platform::NullBackend;
    use tempfile::tempdir;

    #[tokio::test]
    async fn roundtrip_over_unix_socket() {
        let dir = tempdir().unwrap();
        let paths = RuntimePaths::from_root(dir.path());
        paths.ensure_layout().unwrap();
        let runtime =
            Arc::new(Runtime::new(paths.clone(), Arc::new(NullBackend)).unwrap());
        // prepare() creates a placeholder file; remove before bind.
        let _ = std::fs::remove_file(&paths.socket);
        let listener = bind_private_listener(&paths.socket).await.unwrap();
        let server = Arc::clone(&runtime);
        tokio::spawn(async move {
            let _ = serve_forever(server, listener).await;
        });
        // tiny yield for listener
        tokio::task::yield_now().await;
        let resp = call_runtime(
            &paths.socket,
            InternalRequest::Ping {
                protocol_version: InternalProtocolVersion::CURRENT.0,
            },
        )
        .await
        .unwrap();
        match resp {
            InternalResponse::Pong { protocol_version } => {
                assert_eq!(protocol_version, InternalProtocolVersion::CURRENT.0);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

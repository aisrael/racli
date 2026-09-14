//! Wire-protocol client for `racli server` over a Unix socket (`GetVersion`, `Search`, `FindDefinition`, `FindReferences`).

use std::path::Path;
use std::time::Duration;

use prost::Message;
use tokio::net::UnixStream;

use crate::proto::racli::FindDefinitionRequest;
use crate::proto::racli::FindDefinitionResponse;
use crate::proto::racli::FindReferencesRequest;
use crate::proto::racli::FindReferencesResponse;
use crate::proto::racli::GetVersionRequest;
use crate::proto::racli::GetVersionResponse;
use crate::proto::racli::SearchRequest;
use crate::proto::racli::SearchResponse;
use crate::racli_session::RacliRpcError;
use crate::wire;
use crate::wire::Method;
use crate::wire::Status;
use crate::wire::WireError;

/// Failures connecting to `racli server` or completing a round trip of the wire protocol.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Failed to connect to the Unix socket.
    #[error("failed to connect to {path}")]
    Connect {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Connecting to the Unix socket did not complete within the timeout.
    #[error("timed out connecting to {0}")]
    ConnectTimedOut(std::path::PathBuf),
    /// The request/response round trip did not complete within the timeout.
    #[error("timed out waiting for a response")]
    TimedOut,
    /// Failed framing or parsing a wire message.
    #[error(transparent)]
    Wire(#[from] WireError),
    /// The server returned an application-level error.
    #[error(transparent)]
    Rpc(#[from] RacliRpcError),
}

async fn connect(socket_path: &Path, connect_timeout: Duration) -> Result<UnixStream, ClientError> {
    tokio::time::timeout(connect_timeout, UnixStream::connect(socket_path))
        .await
        .map_err(|_| ClientError::ConnectTimedOut(socket_path.to_path_buf()))?
        .map_err(|source| ClientError::Connect {
            path: socket_path.to_path_buf(),
            source,
        })
}

async fn roundtrip<Req: Message, Resp: Message + Default>(
    stream: &mut UnixStream,
    method: Method,
    request: &Req,
) -> Result<Resp, ClientError> {
    wire::write_frame(stream, method as u8, &request.encode_to_vec()).await?;
    let (tag, payload) = wire::read_frame(stream).await?;
    match Status::try_from(tag)? {
        Status::Ok => Ok(Resp::decode(&payload[..]).map_err(WireError::from)?),
        Status::InvalidArgument => Err(ClientError::Rpc(RacliRpcError::InvalidArgument(
            String::from_utf8_lossy(&payload).into_owned(),
        ))),
        Status::Internal => Err(ClientError::Rpc(RacliRpcError::Internal(
            String::from_utf8_lossy(&payload).into_owned(),
        ))),
    }
}

/// Calls `GetVersion` on the server at `socket_path` with 10s connect and request timeouts.
pub async fn get_version(socket_path: &Path) -> Result<GetVersionResponse, ClientError> {
    let mut stream = connect(socket_path, Duration::from_secs(10)).await?;
    tokio::time::timeout(
        Duration::from_secs(10),
        roundtrip(&mut stream, Method::GetVersion, &GetVersionRequest {}),
    )
    .await
    .map_err(|_| ClientError::TimedOut)?
}

/// Calls `Search` on the server at `socket_path` with 10s connect and 60s per-request timeout (LSP `workspace/symbol` can be slow).
pub async fn search(
    socket_path: &Path,
    query: impl AsRef<str>,
) -> Result<SearchResponse, ClientError> {
    let mut stream = connect(socket_path, Duration::from_secs(10)).await?;
    let request = SearchRequest {
        query: query.as_ref().to_string(),
    };
    tokio::time::timeout(
        Duration::from_secs(60),
        roundtrip(&mut stream, Method::Search, &request),
    )
    .await
    .map_err(|_| ClientError::TimedOut)?
}

/// Calls `FindDefinition` on the server at `socket_path` with 10s connect and 60s per-request timeout.
pub async fn find_definition(
    socket_path: &Path,
    file_path: impl AsRef<str>,
    line: u32,
    character: u32,
) -> Result<FindDefinitionResponse, ClientError> {
    let mut stream = connect(socket_path, Duration::from_secs(10)).await?;
    let request = FindDefinitionRequest {
        file_path: file_path.as_ref().to_string(),
        line,
        character,
    };
    tokio::time::timeout(
        Duration::from_secs(60),
        roundtrip(&mut stream, Method::FindDefinition, &request),
    )
    .await
    .map_err(|_| ClientError::TimedOut)?
}

/// Calls `FindReferences` on the server at `socket_path` with 10s connect and 60s per-request timeout.
pub async fn find_references(
    socket_path: &Path,
    file_path: impl AsRef<str>,
    line: u32,
    character: u32,
) -> Result<FindReferencesResponse, ClientError> {
    let mut stream = connect(socket_path, Duration::from_secs(10)).await?;
    let request = FindReferencesRequest {
        file_path: file_path.as_ref().to_string(),
        line,
        character,
    };
    tokio::time::timeout(
        Duration::from_secs(60),
        roundtrip(&mut stream, Method::FindReferences, &request),
    )
    .await
    .map_err(|_| ClientError::TimedOut)?
}

//! gRPC client for `racli server` over a Unix socket (`GetVersion` today).

use std::path::Path;
use std::time::Duration;

use tonic::transport::Endpoint;

use crate::proto::racli::{GetVersionRequest, racli_client::RacliClient};

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `GetVersion`.
#[derive(Debug, thiserror::Error)]
pub enum ClientVersionError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Calls `GetVersion` on the server at `socket_path` with 10s connect and request timeouts.
pub async fn get_server_version(socket_path: &Path) -> Result<String, ClientVersionError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
    let resp = client.get_version(GetVersionRequest {}).await?;

    Ok(resp.into_inner().version)
}

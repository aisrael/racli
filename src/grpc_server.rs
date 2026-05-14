//! gRPC server (`tonic`) bound to a Unix domain socket for `racli server`.

use std::future::Future;
use std::path::{Path, PathBuf};

use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;
use tracing_subscriber::EnvFilter;

use crate::proto::racli::racli_server::{Racli, RacliServer};
use crate::proto::racli::{GetVersionRequest, GetVersionResponse};
use crate::server::Core;
use tonic::{Request, Response, Status};

/// Name of the env var that sets the max level for `racli::*` only (e.g. `trace`, `debug`, `off`).
pub const RACLI_SERVER_LOG_LEVEL_ENV: &str = "RACLI_SERVER_LOG_LEVEL";

/// Builds the server stderr filter: non-`racli` targets capped at `info`, plus `racli` level from env or default.
fn racli_server_env_filter() -> EnvFilter {
    let racli_level = std::env::var(RACLI_SERVER_LOG_LEVEL_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "debug".to_string());

    let combined = format!("info,racli={racli_level}");
    EnvFilter::try_new(&combined).unwrap_or_else(|_| EnvFilter::new("info,racli=debug"))
}

/// Installs a `tracing-subscriber` stderr logger once; [`RACLI_SERVER_LOG_LEVEL_ENV`] only adjusts `racli::*`.
fn init_grpc_server_tracing() {
    let filter = racli_server_env_filter();

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Errors from binding, serving, or cleaning up the gRPC Unix socket server.
#[derive(Debug, thiserror::Error)]
pub enum GrpcServerError {
    /// The Unix socket path could not be bound (often permissions or a stale socket file).
    #[error("failed to bind Unix socket at {path}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// tonic failed while driving the HTTP/2 stack over the Unix listener.
    #[error("failed serving gRPC transport")]
    Serve(#[from] tonic::transport::Error),
}

/// tonic service implementation that forwards RPCs to [`crate::server::Core`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RacliGrpc {
    core: Core,
}

#[tonic::async_trait]
impl Racli for RacliGrpc {
    /// Returns [`Core::version`](crate::server::Core::version) as the protobuf `version` field.
    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        tracing::debug!(rpc = "Racli.GetVersion", "gRPC endpoint invoked");
        let version = self.core.version();
        tracing::debug!(rpc = "Racli.GetVersion", %version, "returning GetVersion response");
        Ok(Response::new(GetVersionResponse { version }))
    }
}

/// Waits for SIGINT or SIGTERM (falls back to Ctrl+C only if SIGTERM cannot be registered).
async fn unix_shutdown_signals() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}

/// Serves gRPC on `socket_path` until SIGINT or SIGTERM, then deletes the bound pathname.
pub async fn run_grpc_unix_socket_interactive<P: AsRef<Path>>(
    socket_path: P,
) -> Result<(), GrpcServerError> {
    let shutdown = async {
        unix_shutdown_signals().await;
    };
    run_grpc_unix_socket_until_shutdown(socket_path, shutdown).await
}

/// Serves Racli gRPC on `socket_path` until `shutdown` completes, removes the socket file, and returns.
/// Prefer this in tests with an oneshot `shutdown`; use [`run_grpc_unix_socket_interactive`] for signal-driven CLI shutdown.
pub async fn run_grpc_unix_socket_until_shutdown<P: AsRef<Path>>(
    socket_path: P,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), GrpcServerError> {
    init_grpc_server_tracing();

    let socket_path = socket_path.as_ref();
    let path_buf = socket_path.to_path_buf();
    let _ = std::fs::remove_file(socket_path);

    let uds =
        tokio::net::UnixListener::bind(socket_path).map_err(|source| GrpcServerError::Bind {
            path: path_buf.clone(),
            source,
        })?;

    let incoming = UnixListenerStream::new(uds);
    let svc = RacliGrpc::default();

    Server::builder()
        .add_service(RacliServer::new(svc))
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await?;

    let _ = std::fs::remove_file(&path_buf);
    Ok(())
}

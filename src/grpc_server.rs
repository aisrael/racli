//! gRPC server (`tonic`) bound to a Unix domain socket for `racli server`.

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;
use tracing_subscriber::EnvFilter;

use crate::proto::racli::GetVersionRequest;
use crate::proto::racli::GetVersionResponse;
use crate::proto::racli::LspServerInfo;
use crate::proto::racli::SearchRequest;
use crate::proto::racli::SearchResponse;
use crate::proto::racli::WorkspaceSymbolResponse as ProtoWorkspaceSymbolResponse;
use crate::proto::racli::racli_server::Racli;
use crate::proto::racli::racli_server::RacliServer;
use crate::rust_analyzer::RustAnalyzerSession;
use crate::server::Core;
use tonic::Request;
use tonic::Response;
use tonic::Status;

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
    /// Failed to read the process working directory for the LSP workspace root.
    #[error("failed to read current working directory")]
    CurrentDir {
        #[source]
        source: std::io::Error,
    },
    /// Failed to spawn or speak LSP with the `rust-analyzer` child process.
    #[error(transparent)]
    RustAnalyzer(#[from] crate::rust_analyzer::RustAnalyzerError),
    /// tonic failed while driving the HTTP/2 stack over the Unix listener.
    #[error("failed serving gRPC transport")]
    Serve(#[from] tonic::transport::Error),
}

/// tonic service implementation that forwards RPCs to [`crate::server::Core`] and LSP metadata from rust-analyzer.
#[derive(Clone)]
pub struct RacliGrpc {
    core: Core,
    lsp_server_info: LspServerInfo,
    rust_analyzer: Arc<Mutex<RustAnalyzerSession>>,
}

#[tonic::async_trait]
impl Racli for RacliGrpc {
    /// Returns [`Core::version`](crate::server::Core::version) and rust-analyzer [`LspServerInfo`] from initialize.
    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        tracing::debug!(rpc = "Racli.GetVersion", "gRPC endpoint invoked");
        let version = self.core.version();
        let lsp_server_info = self.lsp_server_info.clone();
        tracing::debug!(rpc = "Racli.GetVersion", %version, "returning GetVersion response");
        Ok(Response::new(GetVersionResponse {
            version,
            lsp_server_info: Some(lsp_server_info),
        }))
    }

    /// Runs LSP `workspace/symbol` and returns a protobuf mirror of [`lsp_types::WorkspaceSymbolResponse`].
    async fn search(
        &self,
        request: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        let query = request.into_inner().query;
        tracing::debug!(rpc = "Racli.Search", %query, "gRPC endpoint invoked");
        let mut ra = self.rust_analyzer.lock().await;
        let value = self
            .core
            .search(&mut ra, query)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        drop(ra);

        let ws: ProtoWorkspaceSymbolResponse = if value.is_null() {
            ProtoWorkspaceSymbolResponse { payload: None }
        } else {
            let lsp_resp: lsp_types::WorkspaceSymbolResponse =
                serde_json::from_value(value).map_err(|e| Status::internal(e.to_string()))?;
            crate::lsp_map::workspace_symbol_response_to_proto(lsp_resp)
        };

        Ok(Response::new(SearchResponse {
            workspace_symbol_response: Some(ws),
        }))
    }
}

/// Waits for SIGINT or SIGTERM on the same [`signal`](tokio::signal::unix::signal) API (reliable with tonic shutdown).
async fn unix_shutdown_signals() {
    use tokio::signal::unix::SignalKind;
    use tokio::signal::unix::signal;

    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            let _ = sigint.recv().await;
            return;
        }
    };

    tokio::select! {
        _ = sigint.recv() => {}
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

    let cwd = std::env::current_dir().map_err(|source| GrpcServerError::CurrentDir { source })?;
    let rust_analyzer = match RustAnalyzerSession::spawn(&cwd).await {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&path_buf);
            return Err(e.into());
        }
    };

    let ra = Arc::new(Mutex::new(rust_analyzer));

    let uds =
        tokio::net::UnixListener::bind(socket_path).map_err(|source| GrpcServerError::Bind {
            path: path_buf.clone(),
            source,
        })?;

    let incoming = UnixListenerStream::new(uds);
    let lsp_server_info = ra.lock().await.lsp_server_info.clone();
    let svc = RacliGrpc {
        core: Core::default(),
        lsp_server_info,
        rust_analyzer: Arc::clone(&ra),
    };

    tracing::info!(
        version = %crate::VERSION,
        socket = %path_buf.display(),
        "racli gRPC server starting"
    );

    let serve_result = Server::builder()
        .add_service(RacliServer::new(svc))
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await;

    let ra_result = match Arc::try_unwrap(ra) {
        Ok(mutex) => {
            let session = mutex.into_inner();
            session.shutdown_gracefully().await
        }
        Err(_) => {
            tracing::warn!(
                "could not unwrap rust-analyzer Arc after gRPC shutdown; relying on Drop"
            );
            Ok(())
        }
    };

    let _ = std::fs::remove_file(&path_buf);

    serve_result.map_err(GrpcServerError::Serve)?;
    ra_result?;
    Ok(())
}

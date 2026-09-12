//! gRPC server (`tonic`) bound to a Unix domain socket for `racli server`.

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::logging;
use crate::proto::racli::FindDefinitionRequest;
use crate::proto::racli::FindDefinitionResponse;
use crate::proto::racli::FindReferencesRequest;
use crate::proto::racli::FindReferencesResponse;
use crate::proto::racli::GetVersionRequest;
use crate::proto::racli::GetVersionResponse;
use crate::proto::racli::SearchRequest;
use crate::proto::racli::SearchResponse;
use crate::proto::racli::racli_server::Racli;
use crate::proto::racli::racli_server::RacliServer;
use crate::racli_live_backend::RacliBackendStartError;
use crate::racli_live_backend::RacliLiveBackend;
use crate::racli_session::RacliRpcError;
use crate::racli_session::RacliSession;
use tonic::Request;
use tonic::Response;
use tonic::Status;

/// Name of the env var that sets the max level for `racli::*` only (`0`-`5` or a level name, e.g. `debug`).
pub const RACLI_SERVER_LOG_LEVEL_ENV: &str = "RACLI_SERVER_LOG_LEVEL";

/// Name of the env var that, if set, redirects server/MCP logging to a file instead of stderr.
pub const RACLI_SERVER_LOG_FILE_ENV: &str = "RACLI_SERVER_LOG_FILE";

/// Builds the server log filter: non-`racli` targets capped at `info`, plus `racli` level from env or `info`.
fn racli_server_env_filter() -> EnvFilter {
    let racli_level = logging::resolve_level(RACLI_SERVER_LOG_LEVEL_ENV);
    let combined = format!("info,racli={racli_level}");
    EnvFilter::try_new(&combined).unwrap_or_else(|_| EnvFilter::new("info,racli=info"))
}

/// Installs a `tracing-subscriber` logger once, to [`RACLI_SERVER_LOG_FILE_ENV`] if set and openable
/// or stderr otherwise; exits the process immediately if the log file can't be opened. The returned
/// guard must be kept alive for the process lifetime so buffered file writes are flushed.
pub fn init_grpc_server_tracing() -> Option<WorkerGuard> {
    let filter = racli_server_env_filter();
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true);

    match std::env::var_os(RACLI_SERVER_LOG_FILE_ENV).filter(|s| !s.is_empty()) {
        Some(path) => {
            let file = match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(e) => {
                    eprintln!(
                        "error: unable to open {RACLI_SERVER_LOG_FILE_ENV} {path:?} for writing: {e}"
                    );
                    std::process::exit(1);
                }
            };
            let (writer, guard) = tracing_appender::non_blocking(file);
            let _ = builder.with_writer(writer).try_init();
            Some(guard)
        }
        None => {
            let _ = builder.with_writer(std::io::stderr).try_init();
            None
        }
    }
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

impl From<RacliBackendStartError> for GrpcServerError {
    fn from(value: RacliBackendStartError) -> Self {
        match value {
            RacliBackendStartError::RustAnalyzer(e) => Self::RustAnalyzer(e),
        }
    }
}

fn racli_rpc_error_to_status(err: RacliRpcError) -> Status {
    match err {
        RacliRpcError::InvalidArgument(msg) => Status::invalid_argument(msg),
        RacliRpcError::Internal(msg) => Status::internal(msg),
    }
}

/// tonic service implementation that forwards RPCs to [`RacliSession`].
#[derive(Clone)]
pub struct RacliGrpc {
    session: Arc<RacliSession>,
}

#[tonic::async_trait]
impl Racli for RacliGrpc {
    /// Returns [`crate::VERSION`] and rust-analyzer [`LspServerInfo`] from initialize.
    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        tracing::debug!(rpc = "Racli.GetVersion", "gRPC endpoint invoked");
        let resp = self.session.get_version();
        tracing::debug!(rpc = "Racli.GetVersion", %resp.version, "returning GetVersion response");
        Ok(Response::new(resp))
    }

    /// Runs LSP `workspace/symbol` and returns a protobuf mirror of [`lsp_types::WorkspaceSymbolResponse`].
    async fn search(
        &self,
        request: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        let query = request.into_inner().query;
        tracing::debug!(rpc = "Racli.Search", %query, "gRPC endpoint invoked");
        self.session
            .search(query)
            .await
            .map(Response::new)
            .map_err(racli_rpc_error_to_status)
    }

    /// Runs LSP `textDocument/definition` and returns flattened definition locations.
    async fn find_definition(
        &self,
        request: Request<FindDefinitionRequest>,
    ) -> Result<Response<FindDefinitionResponse>, Status> {
        let inner = request.into_inner();
        tracing::debug!(
            rpc = "Racli.FindDefinition",
            file_path = %inner.file_path,
            line = inner.line,
            character = inner.character,
            "gRPC endpoint invoked"
        );
        self.session
            .find_definition(inner.file_path, inner.line, inner.character)
            .await
            .map(Response::new)
            .map_err(racli_rpc_error_to_status)
    }

    /// Runs LSP `textDocument/references` and returns flattened reference locations (declaration included).
    async fn find_references(
        &self,
        request: Request<FindReferencesRequest>,
    ) -> Result<Response<FindReferencesResponse>, Status> {
        let inner = request.into_inner();
        tracing::debug!(
            rpc = "Racli.FindReferences",
            file_path = %inner.file_path,
            line = inner.line,
            character = inner.character,
            "gRPC endpoint invoked"
        );
        self.session
            .find_references(inner.file_path, inner.line, inner.character)
            .await
            .map(Response::new)
            .map_err(racli_rpc_error_to_status)
    }
}

/// Registers SIGINT/SIGTERM handlers immediately (via the same [`signal`](tokio::signal::unix::signal)
/// API tonic's shutdown relies on) and returns a future that resolves once either fires.
///
/// Must be called *before* any slow startup work (e.g. spawning and initializing rust-analyzer):
/// a signal received before the handler is registered falls back to the OS default disposition
/// (immediate termination, skipping rust-analyzer's graceful LSP shutdown entirely). Registering
/// early and awaiting late is safe — tokio records a pending signal via an atomic flag regardless
/// of whether the returned future is being polled yet.
pub(crate) fn install_unix_shutdown_signals() -> impl Future<Output = ()> + Send + 'static {
    use tokio::signal::unix::SignalKind;
    use tokio::signal::unix::signal;

    let sigint = signal(SignalKind::interrupt());
    let sigterm = signal(SignalKind::terminate());

    async move {
        match (sigint, sigterm) {
            (Ok(mut sigint), Ok(mut sigterm)) => {
                tokio::select! {
                    _ = sigint.recv() => {}
                    _ = sigterm.recv() => {}
                }
            }
            (Ok(mut sigint), Err(_)) => {
                let _ = sigint.recv().await;
            }
            (Err(_), Ok(mut sigterm)) => {
                let _ = sigterm.recv().await;
            }
            (Err(_), Err(_)) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
}

/// Serves gRPC on `socket_path` until SIGINT or SIGTERM, then deletes the bound pathname.
pub async fn run_grpc_unix_socket_interactive<P: AsRef<Path>>(
    socket_path: P,
) -> Result<(), GrpcServerError> {
    // Install the signal handlers before any of the (potentially slow) startup work inside
    // `run_grpc_unix_socket_until_shutdown` (spawning and initializing rust-analyzer), so a
    // Ctrl+C during startup is caught instead of killing the process outright.
    let shutdown = install_unix_shutdown_signals();
    run_grpc_unix_socket_until_shutdown(socket_path, shutdown).await
}

/// Serves Racli gRPC on `socket_path` until `shutdown` completes, removes the socket file, and returns.
/// Prefer this in tests with an oneshot `shutdown`; use [`run_grpc_unix_socket_interactive`] for signal-driven CLI shutdown.
pub async fn run_grpc_unix_socket_until_shutdown<P: AsRef<Path>>(
    socket_path: P,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), GrpcServerError> {
    let _log_guard = init_grpc_server_tracing();

    let socket_path = socket_path.as_ref();
    let path_buf = socket_path.to_path_buf();
    let _ = std::fs::remove_file(socket_path);

    let cwd = std::env::current_dir().map_err(|source| GrpcServerError::CurrentDir { source })?;
    let backend = match RacliLiveBackend::start(cwd).await {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&path_buf);
            return Err(e.into());
        }
    };

    let uds =
        tokio::net::UnixListener::bind(socket_path).map_err(|source| GrpcServerError::Bind {
            path: path_buf.clone(),
            source,
        })?;

    let incoming = UnixListenerStream::new(uds);
    let svc = RacliGrpc {
        session: backend.session().clone(),
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

    let ra_result = backend.shutdown().await;

    let _ = std::fs::remove_file(&path_buf);

    serve_result.map_err(GrpcServerError::Serve)?;
    ra_result?;
    Ok(())
}

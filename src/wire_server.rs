//! Custom binary-protocol server bound to a Unix domain socket for `racli server`.

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use prost::Message;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::task::JoinSet;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::logging;
use crate::proto::racli::FindDefinitionRequest;
use crate::proto::racli::FindReferencesRequest;
use crate::proto::racli::GetVersionRequest;
use crate::proto::racli::SearchRequest;
use crate::racli_live_backend::RacliBackendStartError;
use crate::racli_live_backend::RacliLiveBackend;
use crate::racli_session::RacliRpcError;
use crate::racli_session::RacliSession;
use crate::wire;
use crate::wire::Method;
use crate::wire::Status;
use crate::wire::WireError;

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
pub fn init_server_tracing() -> Option<WorkerGuard> {
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

/// Errors from binding, serving, or cleaning up the wire-protocol Unix socket server.
#[derive(Debug, thiserror::Error)]
pub enum WireServerError {
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
    /// Failed accepting a connection on the Unix listener.
    #[error("failed accepting connections on the Unix listener")]
    Accept(#[source] std::io::Error),
}

impl From<RacliBackendStartError> for WireServerError {
    fn from(value: RacliBackendStartError) -> Self {
        match value {
            RacliBackendStartError::RustAnalyzer(e) => Self::RustAnalyzer(e),
        }
    }
}

fn racli_rpc_error_to_status(err: RacliRpcError) -> (Status, String) {
    match err {
        RacliRpcError::InvalidArgument(msg) => (Status::InvalidArgument, msg),
        RacliRpcError::Internal(msg) => (Status::Internal, msg),
    }
}

/// Reads one request frame from `stream`, dispatches it to `session`, writes one response frame,
/// then returns (dropping `stream` closes the connection).
async fn handle_connection(session: Arc<RacliSession>, mut stream: UnixStream) {
    if let Err(e) = handle_connection_inner(&session, &mut stream).await {
        tracing::warn!(error = %e, "wire connection failed");
    }
}

async fn handle_connection_inner(
    session: &RacliSession,
    stream: &mut UnixStream,
) -> Result<(), WireError> {
    let (tag, payload) = wire::read_frame(stream).await?;
    let method = match Method::try_from(tag) {
        Ok(method) => method,
        Err(e) => {
            let (status, msg) = (Status::Internal, e.to_string());
            wire::write_frame(stream, status as u8, msg.as_bytes()).await?;
            return Ok(());
        }
    };

    let (status, response_bytes) = match method {
        Method::GetVersion => {
            tracing::debug!(rpc = "GetVersion", "wire endpoint invoked");
            match GetVersionRequest::decode(&payload[..]) {
                Ok(_req) => {
                    let resp = session.get_version();
                    tracing::debug!(rpc = "GetVersion", %resp.version, "returning GetVersion response");
                    (Status::Ok, resp.encode_to_vec())
                }
                Err(e) => (Status::Internal, e.to_string().into_bytes()),
            }
        }
        Method::Search => match SearchRequest::decode(&payload[..]) {
            Ok(req) => {
                tracing::debug!(rpc = "Search", query = %req.query, "wire endpoint invoked");
                match session.search(req.query).await {
                    Ok(resp) => (Status::Ok, resp.encode_to_vec()),
                    Err(e) => {
                        let (status, msg) = racli_rpc_error_to_status(e);
                        (status, msg.into_bytes())
                    }
                }
            }
            Err(e) => (Status::Internal, e.to_string().into_bytes()),
        },
        Method::FindDefinition => match FindDefinitionRequest::decode(&payload[..]) {
            Ok(req) => {
                tracing::debug!(
                    rpc = "FindDefinition",
                    file_path = %req.file_path,
                    line = req.line,
                    character = req.character,
                    "wire endpoint invoked"
                );
                match session
                    .find_definition(req.file_path, req.line, req.character)
                    .await
                {
                    Ok(resp) => (Status::Ok, resp.encode_to_vec()),
                    Err(e) => {
                        let (status, msg) = racli_rpc_error_to_status(e);
                        (status, msg.into_bytes())
                    }
                }
            }
            Err(e) => (Status::Internal, e.to_string().into_bytes()),
        },
        Method::FindReferences => match FindReferencesRequest::decode(&payload[..]) {
            Ok(req) => {
                tracing::debug!(
                    rpc = "FindReferences",
                    file_path = %req.file_path,
                    line = req.line,
                    character = req.character,
                    "wire endpoint invoked"
                );
                match session
                    .find_references(req.file_path, req.line, req.character)
                    .await
                {
                    Ok(resp) => (Status::Ok, resp.encode_to_vec()),
                    Err(e) => {
                        let (status, msg) = racli_rpc_error_to_status(e);
                        (status, msg.into_bytes())
                    }
                }
            }
            Err(e) => (Status::Internal, e.to_string().into_bytes()),
        },
    };

    wire::write_frame(stream, status as u8, &response_bytes).await
}

/// Registers SIGINT/SIGTERM handlers immediately and returns a future that resolves once either fires.
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

/// Serves the wire protocol on `socket_path` until SIGINT or SIGTERM, then deletes the bound pathname.
pub async fn run_wire_unix_socket_interactive<P: AsRef<Path>>(
    socket_path: P,
) -> Result<(), WireServerError> {
    // Install the signal handlers before any of the (potentially slow) startup work inside
    // `run_wire_unix_socket_until_shutdown` (spawning and initializing rust-analyzer), so a
    // Ctrl+C during startup is caught instead of killing the process outright.
    let shutdown = install_unix_shutdown_signals();
    run_wire_unix_socket_until_shutdown(socket_path, shutdown).await
}

/// Serves the wire protocol on `socket_path` until `shutdown` completes, removes the socket file, and returns.
/// Prefer this in tests with an oneshot `shutdown`; use [`run_wire_unix_socket_interactive`] for signal-driven CLI shutdown.
pub async fn run_wire_unix_socket_until_shutdown<P: AsRef<Path>>(
    socket_path: P,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), WireServerError> {
    let _log_guard = init_server_tracing();

    let socket_path = socket_path.as_ref();
    let path_buf = socket_path.to_path_buf();
    let _ = std::fs::remove_file(socket_path);

    let cwd = std::env::current_dir().map_err(|source| WireServerError::CurrentDir { source })?;
    let backend = match RacliLiveBackend::start(cwd).await {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&path_buf);
            return Err(e.into());
        }
    };

    let listener = UnixListener::bind(socket_path).map_err(|source| WireServerError::Bind {
        path: path_buf.clone(),
        source,
    })?;

    tracing::info!(
        version = %crate::VERSION,
        socket = %path_buf.display(),
        "racli wire-protocol server starting"
    );

    let session = backend.session().clone();
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);

    let accept_err = loop {
        tokio::select! {
            accept_res = listener.accept() => {
                match accept_res {
                    Ok((stream, _addr)) => {
                        let session = session.clone();
                        connections.spawn(handle_connection(session, stream));
                    }
                    Err(e) => break Some(e),
                }
            }
            _ = &mut shutdown => break None,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    };

    // Stop accepting new connections; let any already in flight finish.
    while connections.join_next().await.is_some() {}

    let ra_result = backend.shutdown().await;

    let _ = std::fs::remove_file(&path_buf);

    if let Some(e) = accept_err {
        return Err(WireServerError::Accept(e));
    }
    ra_result?;
    Ok(())
}

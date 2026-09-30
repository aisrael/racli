//! `racli tee`: serves gRPC like `racli server` and proxies an editor's stdio LSP session through that socket.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;
use tonic::Code;
use tonic::Status;
use tonic::transport::Channel;
use tonic::transport::Endpoint;

use crate::effective_unix_socket_path;
use crate::grpc_server::GrpcServerError;
use crate::grpc_server::install_unix_shutdown_signals;
use crate::grpc_server::run_grpc_unix_socket_until_shutdown;
use crate::lsp_client::transport::TransportError;
use crate::lsp_client::transport::read_framed_body;
use crate::lsp_client::transport::write_framed;
use crate::proto::racli::LspEventsRequest;
use crate::proto::racli::LspInitializeRequest;
use crate::proto::racli::LspNotifyRequest;
use crate::proto::racli::LspRequestRequest;
use crate::proto::racli::lsp_request_response::Outcome;
use crate::proto::racli::racli_client::RacliClient;
use crate::rust_analyzer::DEFAULT_SYMBOL_SEARCH_LIMIT;

/// Delay between attempts to connect to the gRPC socket while rust-analyzer is still starting.
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// JSON-RPC `InvalidParams` error code.
const INVALID_PARAMS: i32 = -32602;

/// JSON-RPC `InternalError` error code.
const INTERNAL_ERROR: i32 = -32603;

/// Arguments for `racli tee`; `--version` exits 0 because editors (e.g. VS Code's `rust-analyzer.server.path`) probe the server binary with it.
#[derive(Parser)]
#[command(version = crate::VERSION)]
pub struct TeeArgs {
    /// Maximum number of results rust-analyzer returns per `workspace/symbol` query.
    #[arg(long, default_value_t = DEFAULT_SYMBOL_SEARCH_LIMIT, value_parser = clap::value_parser!(u32).range(1..))]
    pub symbol_search_limit: u32,
}

/// Failures running the gRPC server or the stdio LSP proxy.
#[derive(Debug, thiserror::Error)]
pub enum TeeError {
    /// The in-process gRPC server failed (rust-analyzer startup, socket bind, or serving).
    #[error(transparent)]
    Grpc(#[from] GrpcServerError),
    /// The gRPC server task panicked.
    #[error("racli server task failed")]
    ServerTask(#[from] tokio::task::JoinError),
    /// The gRPC endpoint URI for the socket path was invalid.
    #[error("invalid gRPC endpoint for {path}")]
    Endpoint {
        path: PathBuf,
        #[source]
        source: tonic::transport::Error,
    },
    /// Reading from stdin or writing to stdout failed.
    #[error("LSP stdio transport failed")]
    Stdio(#[from] TransportError),
}

/// Runs the gRPC server on the Unix socket and proxies stdin/stdout LSP through it until the editor
/// sends `exit`, closes stdin, or SIGINT/SIGTERM arrives.
pub async fn run_tee(args: TeeArgs) -> Result<(), TeeError> {
    let signals = install_unix_shutdown_signals();
    let socket_path = effective_unix_socket_path();
    // The server also removes it, but doing it here guarantees our first connect attempt can't
    // reach another process's socket before our server task has run.
    let _ = std::fs::remove_file(&socket_path);

    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let mut server = tokio::spawn({
        let socket_path = socket_path.clone();
        async move {
            run_grpc_unix_socket_until_shutdown(socket_path, args.symbol_search_limit, async {
                tokio::select! {
                    () = signals => {}
                    _ = stop_rx => {}
                }
            })
            .await
        }
    });

    let proxied = tokio::select! {
        proxied = proxy(&socket_path) => proxied,
        // The server only stops on its own after a signal or a failure; either way we are done.
        served = &mut server => return Ok(served??),
    };
    let _ = stop_tx.send(());
    let served = server.await;
    proxied?;
    served??;
    Ok(())
}

/// Connects to the gRPC socket (waiting for the server to bind it) and proxies stdio through it.
async fn proxy(socket_path: &Path) -> Result<(), TeeError> {
    let endpoint =
        Endpoint::try_from(format!("unix://{}", socket_path.display())).map_err(|source| {
            TeeError::Endpoint {
                path: socket_path.to_path_buf(),
                source,
            }
        })?;
    let channel = loop {
        match endpoint.connect().await {
            Ok(channel) => break channel,
            Err(e) => {
                tracing::trace!(error = %e, "racli socket not ready; retrying");
                tokio::time::sleep(CONNECT_RETRY_DELAY).await;
            }
        }
    };
    tracing::info!(socket = %socket_path.display(), "racli tee connected; proxying stdio LSP");
    proxy_stdio(RacliClient::new(channel)).await
}

/// What the stdin loop does after handling one editor message.
enum Step {
    /// Write this message to the editor.
    Reply(Value),
    /// Nothing to send back.
    Nothing,
    /// The editor sent `exit`.
    Exit,
}

/// Reads editor messages one at a time (preserving their order) and forwards them over gRPC; a
/// single writer task owns stdout so replies and server notifications never interleave mid-frame.
async fn proxy_stdio(mut client: RacliClient<Channel>) -> Result<(), TeeError> {
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(write_stdout(out_rx));
    let mut events: Option<JoinHandle<()>> = None;
    let mut stdin = BufReader::new(tokio::io::stdin());

    let result = loop {
        let body = match read_framed_body(&mut stdin).await {
            Ok(body) => body,
            Err(TransportError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                tracing::info!("editor closed stdin; stopping");
                break Ok(());
            }
            Err(e) => break Err(TeeError::Stdio(e)),
        };
        let msg: Value = match serde_json::from_slice(&body) {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(error = %e, "dropping malformed LSP message from editor");
                continue;
            }
        };
        let is_initialize = msg.get("method").and_then(Value::as_str) == Some("initialize");
        match handle_message(&mut client, msg).await {
            Step::Reply(reply) => {
                let _ = out_tx.send(reply);
            }
            Step::Nothing => {}
            Step::Exit => {
                tracing::info!("editor sent exit; stopping");
                break Ok(());
            }
        }
        // Server notifications may only follow the `initialize` response, which is queued above.
        if is_initialize && events.is_none() {
            events = Some(tokio::spawn(forward_events(client.clone(), out_tx.clone())));
        }
    };

    if let Some(events) = events {
        events.abort();
        let _ = events.await;
    }
    drop(out_tx);
    let written = writer.await?;
    result?;
    written?;
    Ok(())
}

/// Handles one editor message: lifecycle messages locally, everything else via gRPC.
async fn handle_message(client: &mut RacliClient<Channel>, msg: Value) -> Step {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();
    let params_json = msg.get("params").map(Value::to_string).unwrap_or_default();

    match (method, id) {
        (Some("initialize"), Some(id)) => {
            let result = client
                .lsp_initialize(LspInitializeRequest {})
                .await
                .map(|r| r.into_inner().result_json);
            Step::Reply(match result {
                Ok(result_json) => json_result(id, &result_json),
                Err(status) => status_error(id, &status),
            })
        }
        // rust-analyzer is shared, so the editor's shutdown is acknowledged locally.
        (Some("shutdown"), Some(id)) => Step::Reply(result(id, Value::Null)),
        (Some(method), Some(id)) => {
            let response = client
                .lsp_request(LspRequestRequest {
                    method: method.to_string(),
                    params_json,
                })
                .await
                .map(|r| r.into_inner().outcome);
            Step::Reply(match response {
                Ok(Some(Outcome::ResultJson(result_json))) => json_result(id, &result_json),
                Ok(Some(Outcome::Error(err))) => {
                    let data = err
                        .data_json
                        .and_then(|d| serde_json::from_str::<Value>(&d).ok());
                    error(id, err.code, &err.message, data)
                }
                Ok(None) => error(id, INTERNAL_ERROR, "empty LspRequest response", None),
                Err(status) => status_error(id, &status),
            })
        }
        (Some("exit"), None) => Step::Exit,
        // racli already sent `initialized`; queued requests can't be cancelled.
        (Some("initialized" | "$/cancelRequest"), None) => Step::Nothing,
        (Some(method), None) => {
            if let Err(status) = client
                .lsp_notify(LspNotifyRequest {
                    method: method.to_string(),
                    params_json,
                })
                .await
            {
                tracing::warn!(method, %status, "forwarding LSP notification failed");
            }
            Step::Nothing
        }
        (None, _) => {
            tracing::debug!(
                "dropping response from editor (server-to-client requests are not forwarded)"
            );
            Step::Nothing
        }
    }
}

/// Streams `Racli.LspEvents` to the editor as LSP notifications until the stream ends.
async fn forward_events(mut client: RacliClient<Channel>, out_tx: mpsc::UnboundedSender<Value>) {
    let mut stream = match client.lsp_events(LspEventsRequest {}).await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            tracing::warn!(%status, "subscribing to LSP events failed");
            return;
        }
    };
    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => {
                let params: Value = serde_json::from_str(&event.params_json).unwrap_or(Value::Null);
                let notification =
                    json!({"jsonrpc": "2.0", "method": event.method, "params": params});
                if out_tx.send(notification).is_err() {
                    return;
                }
            }
            Err(status) => {
                tracing::warn!(%status, "LSP event stream ended with an error");
                return;
            }
        }
    }
}

/// Writes each queued message to stdout as an LSP frame.
async fn write_stdout(mut rx: mpsc::UnboundedReceiver<Value>) -> Result<(), TransportError> {
    let mut stdout = tokio::io::stdout();
    while let Some(msg) = rx.recv().await {
        write_framed(&mut stdout, &msg.to_string()).await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// Builds a JSON-RPC success response.
fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Builds a success response from a JSON string, or an internal error if it doesn't parse.
fn json_result(id: Value, result_json: &str) -> Value {
    match serde_json::from_str(result_json) {
        Ok(value) => result(id, value),
        Err(e) => error(
            id,
            INTERNAL_ERROR,
            &format!("invalid result JSON: {e}"),
            None,
        ),
    }
}

/// Builds a JSON-RPC error response.
fn error(id: Value, code: i32, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

/// Maps a gRPC failure to a JSON-RPC error response.
fn status_error(id: Value, status: &Status) -> Value {
    let code = match status.code() {
        Code::InvalidArgument => INVALID_PARAMS,
        _ => INTERNAL_ERROR,
    };
    error(id, code, status.message(), None)
}

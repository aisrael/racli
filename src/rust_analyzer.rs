//! Spawns `rust-analyzer` as an LSP stdio child, initializes the workspace from a root path, and shuts down with LSP `shutdown` / `exit`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use url::Url;

use crate::proto::racli::LspServerInfo;

/// Puts the child in its own process group so a terminal Ctrl+C (`SIGINT`) does not also kill rust-analyzer before LSP shutdown.
#[cfg(unix)]
fn configure_command_isolated_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `pre_exec` runs in the child after fork and before exec; `setpgid(0,0)` is the
    // standard pattern to isolate the child from the parent's terminal process group.
    unsafe {
        cmd.as_std_mut().pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_command_isolated_process_group(_cmd: &mut Command) {}

/// Failures spawning `rust-analyzer`, during LSP stdio I/O, or when JSON-RPC returns an error.
#[derive(Debug, thiserror::Error)]
pub enum RustAnalyzerError {
    /// The workspace directory could not be turned into a `file://` URI.
    #[error("invalid workspace directory for file URL")]
    InvalidWorkspaceUrl,
    /// Failed to start the `rust-analyzer` process.
    #[error("failed to spawn rust-analyzer")]
    Spawn(#[source] std::io::Error),
    /// Stdin/stdout on the child process failed.
    #[error("rust-analyzer I/O error")]
    Io(#[source] std::io::Error),
    /// The LSP `Content-Length` header was missing or invalid.
    #[error("invalid LSP frame: {0}")]
    Framing(String),
    /// JSON parse/build error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The reader task exited before a response arrived (often child crash or EOF).
    #[error("rust-analyzer closed stdout before a response")]
    ResponseDropped,
    /// JSON-RPC `error` object in a response.
    #[error("rust-analyzer: {0}")]
    Rpc(String),
}

/// Owns a running `rust-analyzer` child with a background task that routes JSON-RPC responses to pending requests.
pub struct RustAnalyzerSession {
    child: Child,
    stdin: ChildStdin,
    next_id: std::sync::atomic::AtomicU64,
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    reader: Option<tokio::task::JoinHandle<Result<(), RustAnalyzerError>>>,
    /// OS process id captured right after spawn (still logged after `wait` when [`Child::id`] is unset).
    child_pid: Option<u32>,
    /// Set when [`RustAnalyzerSession::shutdown_gracefully`] finishes so [`Drop`] does not log an abnormal teardown.
    shutdown_complete: bool,
    /// `serverInfo` from the LSP [`initialize`](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#initialize) result.
    pub lsp_server_info: LspServerInfo,
}

impl RustAnalyzerSession {
    /// Spawns `rust-analyzer` in `workspace_root`, sends `initialize` and `initialized`, and returns a live session.
    pub async fn spawn(workspace_root: &Path) -> Result<Self, RustAnalyzerError> {
        let root_uri = workspace_uri(workspace_root)?;

        let mut cmd = Command::new("rust-analyzer");
        cmd.current_dir(workspace_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        configure_command_isolated_process_group(&mut cmd);
        let mut child = cmd.spawn().map_err(RustAnalyzerError::Spawn)?;

        tracing::info!(
            pid = ?child.id(),
            workspace = %workspace_root.display(),
            "starting rust-analyzer child process"
        );

        let child_pid = child.id();

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RustAnalyzerError::Framing("missing child stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RustAnalyzerError::Framing("missing child stdout".into()))?;

        let pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = Arc::clone(&pending);

        let reader = tokio::spawn(async move {
            stdout_reader_loop(stdout, pending_reader).await
        });

        let mut session = RustAnalyzerSession {
            child,
            stdin,
            next_id: std::sync::atomic::AtomicU64::new(1),
            pending,
            reader: Some(reader),
            shutdown_complete: false,
            child_pid,
            lsp_server_info: LspServerInfo::default(),
        };

        let folder_name = workspace_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".into());
        let init_result = session
            .request(
                "initialize",
                json!({
                    "processId": serde_json::Value::Null,
                    "rootUri": root_uri.clone(),
                    "capabilities": {},
                    "clientInfo": { "name": "racli", "version": env!("CARGO_PKG_VERSION") },
                    "workspaceFolders": [{
                        "uri": root_uri,
                        "name": folder_name
                    }]
                }),
            )
            .await?;

        session.lsp_server_info = lsp_server_info_from_initialize_result(&init_result);

        session
            .send_notification("initialized", json!({}))
            .await?;

        tracing::info!(
            pid = ?session.child_pid,
            lsp_name = %session.lsp_server_info.name,
            lsp_version = %session.lsp_server_info.version,
            "rust-analyzer LSP initialized"
        );

        Ok(session)
    }

    /// Sends LSP `shutdown` and `exit` (best-effort with timeouts), waits for the process, and joins the stdout reader.
    pub async fn shutdown_gracefully(mut self) -> Result<(), RustAnalyzerError> {
        tracing::info!(
            pid = ?self.child_pid,
            "stopping rust-analyzer child process"
        );

        match tokio::time::timeout(
            Duration::from_secs(8),
            self.request("shutdown", Value::Null),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::warn!(
                    pid = ?self.child_pid,
                    error = %e,
                    "LSP shutdown failed; continuing teardown"
                );
            }
            Err(_) => {
                tracing::warn!(
                    pid = ?self.child_pid,
                    "LSP shutdown timed out; continuing teardown"
                );
            }
        }

        if let Err(e) = self.send_notification("exit", Value::Null).await {
            tracing::warn!(
                pid = ?self.child_pid,
                error = %e,
                "LSP exit notification failed; continuing teardown"
            );
        }

        let wait = self.child.wait();
        match tokio::time::timeout(Duration::from_secs(10), wait).await {
            Ok(Ok(status)) => {
                tracing::info!(
                    pid = ?self.child_pid,
                    ?status,
                    "rust-analyzer child process exited"
                );
            }
            Ok(Err(e)) => return Err(RustAnalyzerError::Io(e)),
            Err(_) => {
                tracing::warn!(
                    pid = ?self.child_pid,
                    "rust-analyzer child did not exit in time; killing"
                );
                self.child.kill().await.map_err(RustAnalyzerError::Io)?;
            }
        }

        if let Some(h) = self.reader.take() {
            h.abort();
            let _ = h.await;
        }

        self.shutdown_complete = true;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, RustAnalyzerError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut guard = self.pending.lock().await;
            guard.insert(id, tx);
        }

        write_lsp_message(&mut self.stdin, &msg).await?;

        let response = rx
            .await
            .map_err(|_| RustAnalyzerError::ResponseDropped)?;

        if let Some(err) = response.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown RPC error");
            return Err(RustAnalyzerError::Rpc(msg.to_string()));
        }

        Ok(response
            .get("result")
            .cloned()
            .unwrap_or(Value::Null))
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<(), RustAnalyzerError> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        write_lsp_message(&mut self.stdin, &msg).await
    }
}

impl Drop for RustAnalyzerSession {
    fn drop(&mut self) {
        if !self.shutdown_complete {
            tracing::warn!(
                pid = ?self.child_pid,
                "rust-analyzer child session dropped without graceful shutdown"
            );
        }
        if let Some(h) = self.reader.as_ref() {
            h.abort();
        }
        let _ = self.child.start_kill();
    }
}

fn workspace_uri(root: &Path) -> Result<String, RustAnalyzerError> {
    Url::from_directory_path(root)
        .map(|u| u.to_string())
        .map_err(|()| RustAnalyzerError::InvalidWorkspaceUrl)
}

/// Maps LSP `InitializeResult.serverInfo` JSON into [`LspServerInfo`].
fn lsp_server_info_from_initialize_result(result: &Value) -> LspServerInfo {
    let Some(obj) = result.get("serverInfo").and_then(|v| v.as_object()) else {
        return LspServerInfo::default();
    };
    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let version = obj
        .get("version")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        })
        .unwrap_or_default();
    LspServerInfo { name, version }
}

async fn read_lsp_message<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, RustAnalyzerError> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(RustAnalyzerError::Io)?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        let line_trim = line.trim_end_matches(['\r', '\n']);
        let prefix = "Content-Length:";
        if let Some(rest) = line_trim.strip_prefix(prefix) {
            let len: usize = rest
                .trim()
                .parse()
                .map_err(|_| RustAnalyzerError::Framing("invalid Content-Length".into()))?;
            content_length = Some(len);
        }
    }
    let len = content_length.ok_or_else(|| {
        RustAnalyzerError::Framing("missing Content-Length header".into())
    })?;
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(RustAnalyzerError::Io)?;
    Ok(body)
}

async fn write_lsp_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &Value,
) -> Result<(), RustAnalyzerError> {
    let body = serde_json::to_vec(msg)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer
        .write_all(header.as_bytes())
        .await
        .map_err(RustAnalyzerError::Io)?;
    writer
        .write_all(&body)
        .await
        .map_err(RustAnalyzerError::Io)?;
    writer.flush().await.map_err(RustAnalyzerError::Io)?;
    Ok(())
}

async fn stdout_reader_loop(
    stdout: impl tokio::io::AsyncRead + Unpin,
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
) -> Result<(), RustAnalyzerError> {
    let mut reader = BufReader::new(stdout);
    loop {
        let body = match read_lsp_message(&mut reader).await {
            Ok(b) => b,
            Err(RustAnalyzerError::Io(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        };

        let v: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "skipping non-JSON LSP body");
                continue;
            }
        };

        let Some(id_val) = v.get("id") else {
            tracing::trace!(%v, "LSP notification from server");
            continue;
        };

        let is_response = v.get("result").is_some() || v.get("error").is_some();
        if !is_response {
            continue;
        }

        let Some(id) = rpc_id(id_val) else {
            continue;
        };

        let Some(tx) = pending.lock().await.remove(&id) else {
            tracing::trace!(%id, "no waiter for response id");
            continue;
        };
        let _ = tx.send(v);
    }
}

fn rpc_id(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
}

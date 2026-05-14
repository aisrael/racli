//! Spawns `rust-analyzer` as an LSP stdio child, initializes the workspace from a root path, and shuts down with LSP `shutdown` / `exit`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lsp_types::ClientCapabilities;
use lsp_types::ClientInfo;
use lsp_types::DidChangeWatchedFilesClientCapabilities;
use lsp_types::DidChangeWatchedFilesParams;
use lsp_types::GotoDefinitionParams;
use lsp_types::InitializeParams;
use lsp_types::PartialResultParams;
use lsp_types::Position;
use lsp_types::TextDocumentIdentifier;
use lsp_types::TextDocumentPositionParams;
use lsp_types::Uri;
use lsp_types::WorkDoneProgressParams;
use lsp_types::WorkspaceClientCapabilities;
use lsp_types::WorkspaceFolder;
use lsp_types::WorkspaceSymbolParams;
use lsp_types::notification::DidChangeWatchedFiles;
use lsp_types::notification::Notification;
use lsp_types::request::GotoDefinition;
use lsp_types::request::WorkspaceSymbolRequest;
use serde_json::Value;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::Mutex;
use url::Url;

use crate::lsp_client::LspClient;
use crate::lsp_client::transport::io_transport;
use crate::proto::racli::LspServerInfo;

/// Client capabilities advertised to rust-analyzer during LSP `initialize` (includes watched-files dynamic registration).
fn racli_lsp_client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        workspace: Some(WorkspaceClientCapabilities {
            did_change_watched_files: Some(DidChangeWatchedFilesClientCapabilities {
                dynamic_registration: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

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
    /// A source file path could not be turned into a `file://` document URI.
    #[error("invalid source file path for file URL")]
    InvalidDocumentUrl,
    /// Failed to start the `rust-analyzer` process.
    #[error("failed to spawn rust-analyzer")]
    Spawn(#[source] std::io::Error),
    /// Stdin/stdout on the child process failed.
    #[error("rust-analyzer I/O error")]
    Io(#[source] std::io::Error),
    /// [`LspClient`] / JSON-RPC stack error (including transport).
    #[error(transparent)]
    Lsp(#[from] crate::lsp_client::LspError),
    /// JSON parse/build error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// JSON-RPC `error` object in a response (e.g. conflicting merged search shapes).
    #[error("rust-analyzer: {0}")]
    Rpc(String),
}

/// Owns a running `rust-analyzer` child and an [`LspClient`] over stdio.
pub struct RustAnalyzerSession {
    child: Child,
    /// Wrapped so [`RustAnalyzerSession::shutdown_gracefully`] can consume the client before waiting on [`Child`] (this type implements [`Drop`]).
    lsp: Option<LspClient>,
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
        let root_uri_str = workspace_uri(workspace_root)?;
        let root_uri: Uri = root_uri_str
            .parse()
            .map_err(|_| RustAnalyzerError::InvalidWorkspaceUrl)?;

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
            .ok_or_else(|| RustAnalyzerError::Io(io_other("missing child stdin")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("missing child stdout")))?;

        let (sender, receiver) = io_transport(stdin, stdout);
        let lsp = LspClient::new(sender, receiver);

        let folder_name = workspace_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".into());

        #[allow(deprecated)]
        // Mirrors prior JSON handshake; deprecated in favour of workspace_folders-only.
        let init_params = InitializeParams {
            process_id: None,
            root_uri: Some(root_uri.clone()),
            capabilities: racli_lsp_client_capabilities(),
            client_info: Some(ClientInfo {
                name: "racli".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: folder_name,
            }]),
            ..Default::default()
        };

        let init = lsp.initialize(init_params).await?;
        let lsp_server_info = lsp_server_info_from_server_info(init.server_info);

        lsp.initialized().await?;

        let session = RustAnalyzerSession {
            child,
            lsp: Some(lsp),
            child_pid,
            shutdown_complete: false,
            lsp_server_info,
        };

        tracing::info!(
            pid = ?session.child_pid,
            lsp_name = %session.lsp_server_info.name,
            lsp_version = %session.lsp_server_info.version,
            "rust-analyzer LSP initialized"
        );

        Ok(session)
    }

    /// Sends LSP `shutdown` and `exit` (best-effort with timeouts), waits for the process, and drops the LSP client.
    pub async fn shutdown_gracefully(mut self) -> Result<(), RustAnalyzerError> {
        tracing::info!(
            pid = ?self.child_pid,
            "stopping rust-analyzer child process"
        );

        if let Some(lsp) = self.lsp.take() {
            match tokio::time::timeout(Duration::from_secs(8), lsp.shutdown()).await {
                Ok(Ok(())) => {}
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

            if let Err(e) = lsp.exit().await {
                tracing::warn!(
                    pid = ?self.child_pid,
                    error = %e,
                    "LSP exit notification failed; continuing teardown"
                );
            }
        } else {
            tracing::warn!(
                pid = ?self.child_pid,
                "LSP client missing during shutdown handshake; tearing down child only"
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

        self.shutdown_complete = true;
        Ok(())
    }

    /// Sends LSP `workspace/symbol` with the given query and returns the JSON-RPC `result` (often an array or `null`).
    pub async fn workspace_symbol(
        &mut self,
        query: impl Into<String>,
    ) -> Result<Value, RustAnalyzerError> {
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let result = lsp
            .send_request::<WorkspaceSymbolRequest>(WorkspaceSymbolParams {
                query: query.into(),
                ..Default::default()
            })
            .await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends LSP `textDocument/definition` for `document_uri` at `line` / `character` (0-based LSP position) and returns the JSON-RPC `result` (`null` or a location payload).
    pub async fn text_document_definition(
        &mut self,
        document_uri: impl Into<String>,
        line: u32,
        character: u32,
    ) -> Result<Value, RustAnalyzerError> {
        let uri_str = document_uri.into();
        let uri: Uri = uri_str
            .parse()
            .map_err(|_| RustAnalyzerError::InvalidDocumentUrl)?;
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let result = lsp.send_request::<GotoDefinition>(params).await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends LSP `workspace/didChangeWatchedFiles` so the server can refresh state for filesystem changes.
    pub async fn notify_did_change_watched_files(
        &mut self,
        params: DidChangeWatchedFilesParams,
    ) -> Result<(), RustAnalyzerError> {
        tracing::debug!(
            lsp_method = DidChangeWatchedFiles::METHOD,
            change_count = params.changes.len(),
            changes = ?params.changes,
            "sending workspace/didChangeWatchedFiles notification to rust-analyzer"
        );
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        lsp.send_notification::<DidChangeWatchedFiles>(params)
            .await?;
        Ok(())
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
        let _ = self.child.start_kill();
    }
}

fn io_other(msg: &'static str) -> std::io::Error {
    std::io::Error::other(msg)
}

/// Shuts down the session when `ra` holds the last `Arc` strong reference (otherwise warns and returns Ok).
pub async fn shutdown_rust_analyzer_session_arc(
    ra: Arc<Mutex<RustAnalyzerSession>>,
) -> Result<(), RustAnalyzerError> {
    match Arc::try_unwrap(ra) {
        Ok(mutex) => mutex.into_inner().shutdown_gracefully().await,
        Err(_) => {
            tracing::warn!("could not unwrap rust-analyzer Arc before shutdown; relying on Drop");
            Ok(())
        }
    }
}

fn workspace_uri(root: &Path) -> Result<String, RustAnalyzerError> {
    Url::from_directory_path(root)
        .map(|u| u.to_string())
        .map_err(|()| RustAnalyzerError::InvalidWorkspaceUrl)
}

/// Builds an LSP `file://` document URI for an absolute regular file path.
pub fn document_uri_from_path(path: &Path) -> Result<String, RustAnalyzerError> {
    Url::from_file_path(path)
        .map(|u| u.to_string())
        .map_err(|()| RustAnalyzerError::InvalidDocumentUrl)
}

/// Maps LSP `InitializeResult.server_info` into [`LspServerInfo`].
fn lsp_server_info_from_server_info(server: Option<lsp_types::ServerInfo>) -> LspServerInfo {
    let Some(si) = server else {
        return LspServerInfo::default();
    };
    let version = si.version.unwrap_or_default();
    LspServerInfo {
        name: si.name,
        version,
    }
}

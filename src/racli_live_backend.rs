//! Shared rust-analyzer session, workspace file watcher, and [`RacliSession`] for `racli server` and `racli mcp`.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::racli_session::RacliSession;
use crate::rust_analyzer::RustAnalyzerError;
use crate::rust_analyzer::RustAnalyzerSession;
use crate::rust_analyzer::shutdown_rust_analyzer_session_arc;
use crate::server::Core;
use crate::workspace_file_watcher::WorkspaceFileWatcherHandle;
use crate::workspace_file_watcher::spawn_workspace_file_watcher;

/// Failures starting [`RustAnalyzerSession`] or wiring the file watcher (before gRPC bind in server mode).
#[derive(Debug, thiserror::Error)]
pub enum RacliBackendStartError {
    /// `rust-analyzer` could not be spawned or LSP initialization failed.
    #[error(transparent)]
    RustAnalyzer(#[from] RustAnalyzerError),
}

/// Live backend: LSP child, notify bridge, and shared RPC/session state.
pub struct RacliLiveBackend {
    session: Arc<RacliSession>,
    watcher: WorkspaceFileWatcherHandle,
    ra: Arc<Mutex<RustAnalyzerSession>>,
}

impl RacliLiveBackend {
    /// Spawns `rust-analyzer` under `workspace_root`, completes LSP init, starts the workspace watcher, and builds [`RacliSession`].
    pub async fn start(workspace_root: PathBuf) -> Result<Self, RacliBackendStartError> {
        let rust_analyzer = RustAnalyzerSession::spawn(&workspace_root).await?;
        let ra = Arc::new(Mutex::new(rust_analyzer));

        let watcher = spawn_workspace_file_watcher(workspace_root, Arc::clone(&ra));

        let lsp_server_info = ra.lock().await.lsp_server_info.clone();
        let session = Arc::new(RacliSession::new(
            Core::default(),
            lsp_server_info,
            Arc::clone(&ra),
        ));

        Ok(Self {
            session,
            watcher,
            ra,
        })
    }

    /// Shared session for gRPC or MCP tool handlers.
    pub fn session(&self) -> &Arc<RacliSession> {
        &self.session
    }

    /// Stops the file watcher, drops the session handle, and shuts down rust-analyzer gracefully.
    pub async fn shutdown(self) -> Result<(), RustAnalyzerError> {
        self.watcher.stop().await;
        drop(self.session);
        shutdown_rust_analyzer_session_arc(self.ra).await
    }
}

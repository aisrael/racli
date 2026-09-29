//! Shared rust-analyzer session, workspace file watcher, and [`RacliSession`] for `racli server` and `racli mcp`.

use std::path::PathBuf;
use std::sync::Arc;

use ractor::Actor;
use ractor::ActorRef;
use tokio::task::JoinHandle;

use crate::actors::backend::BackendArgs;
use crate::actors::backend::BackendMsg;
use crate::actors::backend::BackendSupervisor;
use crate::racli_session::RacliSession;
use crate::rust_analyzer::RustAnalyzerError;

/// Failures starting [`crate::rust_analyzer::RustAnalyzerSession`] or wiring the file watcher (before gRPC bind in server mode).
#[derive(Debug, thiserror::Error)]
pub enum RacliBackendStartError {
    /// `rust-analyzer` could not be spawned or LSP initialization failed.
    #[error(transparent)]
    RustAnalyzer(#[from] RustAnalyzerError),
    /// A backend actor failed to start for a reason other than rust-analyzer itself.
    #[error("backend actor failed to start: {0}")]
    Actor(String),
}

impl RacliBackendStartError {
    /// Recovers a typed [`RustAnalyzerError`] from an actor startup failure, keeping anything else as text.
    pub(crate) fn from_actor(err: ractor::ActorProcessingErr) -> Self {
        match err.downcast::<RustAnalyzerError>() {
            Ok(e) => Self::RustAnalyzer(*e),
            Err(other) => Self::Actor(other.to_string()),
        }
    }
}

/// Live backend: handle to the [`BackendSupervisor`] actor and its shared RPC/session state.
pub struct RacliLiveBackend {
    session: Arc<RacliSession>,
    supervisor: ActorRef<BackendMsg>,
    handle: JoinHandle<()>,
}

impl RacliLiveBackend {
    /// Spawns `rust-analyzer` under `workspace_root` (capping `workspace/symbol` at `symbol_search_limit`), completes LSP init, starts the workspace watcher, and builds [`RacliSession`].
    pub async fn start(
        workspace_root: PathBuf,
        symbol_search_limit: u32,
    ) -> Result<Self, RacliBackendStartError> {
        let (supervisor, handle) = Actor::spawn(
            None,
            BackendSupervisor,
            BackendArgs {
                workspace_root,
                symbol_search_limit,
            },
        )
        .await
        .map_err(|e| RacliBackendStartError::from_actor(crate::actors::startup_error(e)))?;

        let session = ractor::call!(supervisor, BackendMsg::GetSession)
            .map_err(|e| RacliBackendStartError::Actor(e.to_string()))?;

        Ok(Self {
            session,
            supervisor,
            handle,
        })
    }

    /// Shared session for gRPC or MCP tool handlers.
    pub fn session(&self) -> &Arc<RacliSession> {
        &self.session
    }

    /// Stops the file watcher, then shuts down rust-analyzer gracefully, and waits for the supervisor to exit.
    pub async fn shutdown(self) -> Result<(), RustAnalyzerError> {
        let result = match ractor::call!(self.supervisor, BackendMsg::Shutdown) {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(error = %e, "backend supervisor unavailable at shutdown");
                Ok(())
            }
        };
        let _ = self.handle.await;
        result
    }
}

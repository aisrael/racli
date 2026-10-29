//! Supervisor for the rust-analyzer and (with the `notify` feature) file watcher actors; sequences
//! their startup and shutdown.

use std::path::PathBuf;
use std::sync::Arc;

use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::RpcReplyPort;
use ractor::SupervisionEvent;

#[cfg(feature = "notify")]
use crate::actors::file_watcher::FileWatcherActor;
#[cfg(feature = "notify")]
use crate::actors::file_watcher::FileWatcherArgs;
#[cfg(feature = "notify")]
use crate::actors::file_watcher::FileWatcherMsg;
use crate::actors::rust_analyzer::RustAnalyzerActor;
use crate::actors::rust_analyzer::RustAnalyzerArgs;
use crate::actors::rust_analyzer::RustAnalyzerMsg;
use crate::actors::startup_error;
use crate::racli_core::RacliCore;
use crate::rust_analyzer::RustAnalyzerError;

/// Failures starting the backend: rust-analyzer, or (with the `notify` feature) the file watcher,
/// before the front end starts.
#[derive(Debug, thiserror::Error)]
pub enum BackendStartError {
    /// `rust-analyzer` could not be spawned or LSP initialization failed.
    #[error(transparent)]
    RustAnalyzer(#[from] RustAnalyzerError),
    /// A backend actor failed to start for a reason other than rust-analyzer itself.
    #[error("backend actor failed to start: {0}")]
    Actor(String),
}

impl BackendStartError {
    /// Recovers a typed [`RustAnalyzerError`] from an actor startup failure, keeping anything else as text.
    pub(crate) fn from_actor(err: ActorProcessingErr) -> Self {
        match err.downcast::<RustAnalyzerError>() {
            Ok(e) => Self::RustAnalyzer(*e),
            Err(other) => Self::Actor(other.to_string()),
        }
    }
}

/// Messages handled by [`BackendSupervisor`].
pub(crate) enum BackendMsg {
    /// Returns the shared session used by gRPC/MCP handlers.
    GetSession(RpcReplyPort<Arc<RacliCore>>),
    /// Stops the watcher (if any), then shuts rust-analyzer down gracefully, then stops the supervisor.
    Shutdown(RpcReplyPort<Result<(), RustAnalyzerError>>),
}

/// Startup arguments for [`BackendSupervisor`].
pub(crate) struct BackendArgs {
    /// Workspace root for rust-analyzer and the file watcher.
    pub workspace_root: PathBuf,
    /// Cap on `workspace/symbol` results per query.
    pub symbol_search_limit: u32,
}

/// State of [`BackendSupervisor`]: child handles plus the session built from them.
pub(crate) struct BackendState {
    session: Arc<RacliCore>,
    rust_analyzer: ActorRef<RustAnalyzerMsg>,
    #[cfg(feature = "notify")]
    watcher: ActorRef<FileWatcherMsg>,
}

/// Starts rust-analyzer then, with the `notify` feature, the watcher (otherwise rust-analyzer watches the
/// workspace itself); stops them in reverse order. Child failures are logged, never restarted.
pub(crate) struct BackendSupervisor;

impl Actor for BackendSupervisor {
    type Msg = BackendMsg;
    type State = BackendState;
    type Arguments = BackendArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let (rust_analyzer, _) = Actor::spawn_linked(
            None,
            RustAnalyzerActor,
            RustAnalyzerArgs {
                workspace_root: args.workspace_root.clone(),
                symbol_search_limit: args.symbol_search_limit,
            },
            myself.get_cell(),
        )
        .await
        .map_err(startup_error)?;

        #[cfg(feature = "notify")]
        let watcher = {
            let watcher = Actor::spawn_linked(
                None,
                FileWatcherActor,
                FileWatcherArgs {
                    workspace_root: args.workspace_root,
                    rust_analyzer: rust_analyzer.clone(),
                },
                myself.get_cell(),
            )
            .await;
            match watcher {
                Ok((watcher, _)) => watcher,
                Err(e) => {
                    shutdown_rust_analyzer(&rust_analyzer).await?;
                    return Err(startup_error(e));
                }
            }
        };

        let lsp_server_info = ractor::call!(rust_analyzer, RustAnalyzerMsg::ServerInfo)?;
        let initialize_result = ractor::call!(rust_analyzer, RustAnalyzerMsg::InitializeResult)?;
        let events = ractor::call!(rust_analyzer, RustAnalyzerMsg::Events)?;
        let session = Arc::new(RacliCore::new(
            lsp_server_info,
            initialize_result,
            events,
            rust_analyzer.clone(),
        ));

        Ok(BackendState {
            session,
            rust_analyzer,
            #[cfg(feature = "notify")]
            watcher,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BackendMsg::GetSession(reply) => {
                let _ = reply.send(Arc::clone(&state.session));
            }
            BackendMsg::Shutdown(reply) => {
                // Drain so already-queued file events reach rust-analyzer before its LSP shutdown.
                #[cfg(feature = "notify")]
                if let Err(e) = state.watcher.drain_and_wait(None).await {
                    tracing::debug!(error = %e, "file watcher already stopped");
                }
                let result = shutdown_rust_analyzer(&state.rust_analyzer).await;
                let _ = reply.send(result);
                myself.stop(None);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Log only: later requests to a dead rust-analyzer actor fail with `Internal`.
        match message {
            SupervisionEvent::ActorFailed(who, reason) => {
                tracing::error!(actor = %who.get_id(), %reason, "backend child actor failed");
            }
            SupervisionEvent::ActorTerminated(who, _, reason) => {
                tracing::debug!(actor = %who.get_id(), ?reason, "backend child actor terminated");
            }
            _ => {}
        }
        Ok(())
    }
}

/// Runs rust-analyzer's graceful LSP shutdown and then stops its actor; an unreachable actor counts as already stopped.
async fn shutdown_rust_analyzer(
    rust_analyzer: &ActorRef<RustAnalyzerMsg>,
) -> Result<(), RustAnalyzerError> {
    let result = match ractor::call!(rust_analyzer, RustAnalyzerMsg::Shutdown) {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(error = %e, "rust-analyzer actor unavailable at shutdown");
            Ok(())
        }
    };
    let _ = rust_analyzer.stop_and_wait(None, None).await;
    result
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::task::JoinHandle;

    use super::*;
    use crate::racli_core::RacliRpcError;
    use crate::rust_analyzer::DEFAULT_SYMBOL_SEARCH_LIMIT;
    use crate::rust_analyzer::SymbolSearchOptions;

    /// Whether `rust-analyzer --version` succeeds; prints a skip notice otherwise.
    fn rust_analyzer_available() -> bool {
        let ok = std::process::Command::new("rust-analyzer")
            .arg("--version")
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("skip: rust-analyzer not on PATH or --version failed");
        }
        ok
    }

    /// Starts a [`BackendSupervisor`] on `workspace_root` and returns it with its session.
    async fn start(
        workspace_root: PathBuf,
    ) -> (ActorRef<BackendMsg>, JoinHandle<()>, Arc<RacliCore>) {
        let args = BackendArgs {
            workspace_root,
            symbol_search_limit: DEFAULT_SYMBOL_SEARCH_LIMIT,
        };
        let (supervisor, handle) = tokio::time::timeout(
            Duration::from_secs(120),
            Actor::spawn(None, BackendSupervisor, args),
        )
        .await
        .expect("backend start should finish within timeout")
        .expect("spawn BackendSupervisor");
        let session = ractor::call!(supervisor, BackendMsg::GetSession).expect("GetSession");
        (supervisor, handle, session)
    }

    /// Shuts the supervisor down and waits for it to exit.
    async fn shutdown(supervisor: ActorRef<BackendMsg>, handle: JoinHandle<()>) {
        let stop = async {
            let result = ractor::call!(supervisor, BackendMsg::Shutdown).expect("Shutdown");
            let _ = handle.await;
            result
        };
        tokio::time::timeout(Duration::from_secs(30), stop)
            .await
            .expect("shutdown should finish within timeout")
            .expect("shutdown ok");
    }

    /// The session returns the crate version and rust-analyzer's serverInfo, then the backend shuts down cleanly.
    #[tokio::test]
    async fn get_version_round_trip() {
        if !rust_analyzer_available() {
            return;
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let (supervisor, handle, session) = start(dir.path().to_path_buf()).await;

        let resp = session.get_version();
        assert_eq!(resp.version, env!("CARGO_PKG_VERSION"));
        let lsp = resp.lsp_server_info.expect("lsp_server_info");
        assert_eq!(lsp.name, "rust-analyzer");
        assert!(!lsp.version.is_empty());

        shutdown(supervisor, handle).await;
    }

    /// Once the backend is shut down, a retained session fails requests with `Internal` instead of hanging or panicking.
    #[tokio::test]
    async fn requests_after_shutdown_fail_internal() {
        if !rust_analyzer_available() {
            return;
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let (supervisor, handle, session) = start(dir.path().to_path_buf()).await;
        shutdown(supervisor, handle).await;

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            session.search("anything".into(), SymbolSearchOptions::default()),
        )
        .await
        .expect("search after shutdown should not hang");
        assert!(
            matches!(result, Err(RacliRpcError::Internal(_))),
            "expected Internal error after shutdown, got {result:?}"
        );
    }
}

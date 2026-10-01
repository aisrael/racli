//! Supervisor for the rust-analyzer and file watcher actors; sequences their startup and shutdown.

use std::path::PathBuf;
use std::sync::Arc;

use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::RpcReplyPort;
use ractor::SupervisionEvent;

use crate::actors::file_watcher::FileWatcherActor;
use crate::actors::file_watcher::FileWatcherArgs;
use crate::actors::file_watcher::FileWatcherMsg;
use crate::actors::rust_analyzer::RustAnalyzerActor;
use crate::actors::rust_analyzer::RustAnalyzerArgs;
use crate::actors::rust_analyzer::RustAnalyzerMsg;
use crate::actors::startup_error;
use crate::racli_session::RacliSession;
use crate::rust_analyzer::RustAnalyzerError;
use crate::server::Core;

/// Messages handled by [`BackendSupervisor`].
pub(crate) enum BackendMsg {
    /// Returns the shared session used by gRPC/MCP handlers.
    GetSession(RpcReplyPort<Arc<RacliSession>>),
    /// Stops the watcher, then shuts rust-analyzer down gracefully, then stops the supervisor.
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
    session: Arc<RacliSession>,
    rust_analyzer: ActorRef<RustAnalyzerMsg>,
    watcher: ActorRef<FileWatcherMsg>,
}

/// Starts rust-analyzer then the watcher; stops them in reverse order. Child failures are logged, never restarted.
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
        let watcher = match watcher {
            Ok((watcher, _)) => watcher,
            Err(e) => {
                shutdown_rust_analyzer(&rust_analyzer).await?;
                return Err(startup_error(e));
            }
        };

        let lsp_server_info = ractor::call!(rust_analyzer, RustAnalyzerMsg::ServerInfo)?;
        let initialize_result = ractor::call!(rust_analyzer, RustAnalyzerMsg::InitializeResult)?;
        let events = ractor::call!(rust_analyzer, RustAnalyzerMsg::Events)?;
        let session = Arc::new(RacliSession::new(
            Core::default(),
            lsp_server_info,
            initialize_result,
            events,
            rust_analyzer.clone(),
        ));

        Ok(BackendState {
            session,
            rust_analyzer,
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

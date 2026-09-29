//! Process-level supervisor: starts the backend then one front end, and tears them down in reverse order.

use std::future::Future;
use std::path::PathBuf;

use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::actors::backend::BackendArgs;
use crate::actors::backend::BackendMsg;
use crate::actors::backend::BackendSupervisor;
use crate::actors::frontend::FrontendError;
use crate::actors::frontend::FrontendMsg;
use crate::actors::frontend::GrpcFrontend;
use crate::actors::frontend::GrpcFrontendArgs;
use crate::actors::frontend::McpFrontend;
use crate::actors::frontend::McpFrontendArgs;
use crate::actors::startup_error;
use crate::racli_live_backend::RacliBackendStartError;
use crate::rust_analyzer::RustAnalyzerError;

/// Which front end [`RootActor`] serves.
pub(crate) enum FrontendKind {
    /// gRPC on the given Unix socket path (`racli server`).
    Grpc { socket_path: PathBuf },
    /// MCP on stdin/stdout (`racli mcp`).
    McpStdio,
}

/// Failures from any phase of the root actor's lifecycle.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RootError {
    /// The backend (rust-analyzer or watcher) failed to start.
    #[error(transparent)]
    StartBackend(RacliBackendStartError),
    /// The front end failed to start (socket bind or MCP handshake).
    #[error(transparent)]
    StartFrontend(FrontendError),
    /// The front end failed while serving or stopping.
    #[error(transparent)]
    Frontend(FrontendError),
    /// rust-analyzer's graceful shutdown failed.
    #[error(transparent)]
    Backend(RustAnalyzerError),
}

impl RootError {
    /// Classifies a startup failure by downcasting it to a front-end or backend error.
    fn from_startup(err: ActorProcessingErr) -> Self {
        match err.downcast::<FrontendError>() {
            Ok(e) => Self::StartFrontend(*e),
            Err(other) => Self::StartBackend(RacliBackendStartError::from_actor(other)),
        }
    }
}

/// Messages handled by [`RootActor`].
pub(crate) enum RootMsg {
    /// External shutdown request (e.g. SIGINT/SIGTERM).
    Shutdown,
    /// The front end stopped serving on its own (gRPC error or MCP stdin EOF).
    FrontendExited,
}

/// Startup arguments for [`RootActor`].
pub(crate) struct RootArgs {
    /// Workspace root for rust-analyzer and the file watcher.
    pub workspace_root: PathBuf,
    /// Cap on `workspace/symbol` results per query.
    pub symbol_search_limit: u32,
    /// Front end to serve.
    pub frontend: FrontendKind,
    /// Receives the combined teardown result once shutdown finishes.
    pub done_tx: oneshot::Sender<Result<(), RootError>>,
}

/// State of [`RootActor`]; `done_tx` is `None` once teardown has run.
pub(crate) struct RootState {
    backend: ActorRef<BackendMsg>,
    backend_handle: Option<JoinHandle<()>>,
    frontend: ActorRef<FrontendMsg>,
    frontend_handle: Option<JoinHandle<()>>,
    done_tx: Option<oneshot::Sender<Result<(), RootError>>>,
}

/// Root of the actor tree: backend first, then front end; teardown runs front end, then backend.
pub(crate) struct RootActor;

impl Actor for RootActor {
    type Msg = RootMsg;
    type State = RootState;
    type Arguments = RootArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let (backend, backend_handle) = Actor::spawn_linked(
            None,
            BackendSupervisor,
            BackendArgs {
                workspace_root: args.workspace_root,
                symbol_search_limit: args.symbol_search_limit,
            },
            myself.get_cell(),
        )
        .await
        .map_err(startup_error)?;
        let session = ractor::call!(backend, BackendMsg::GetSession)?;

        let frontend = match args.frontend {
            FrontendKind::Grpc { socket_path } => {
                Actor::spawn_linked(
                    None,
                    GrpcFrontend,
                    GrpcFrontendArgs {
                        socket_path,
                        session,
                        root: myself.clone(),
                    },
                    myself.get_cell(),
                )
                .await
            }
            FrontendKind::McpStdio => {
                Actor::spawn_linked(
                    None,
                    McpFrontend,
                    McpFrontendArgs {
                        session,
                        root: myself.clone(),
                    },
                    myself.get_cell(),
                )
                .await
            }
        };
        let (frontend, frontend_handle) = match frontend {
            Ok(spawned) => spawned,
            Err(e) => {
                if let Err(shutdown_err) = shutdown_backend(&backend, Some(backend_handle)).await {
                    tracing::warn!(error = %shutdown_err, "backend shutdown after front-end startup failure failed");
                }
                return Err(startup_error(e));
            }
        };

        Ok(RootState {
            backend,
            backend_handle: Some(backend_handle),
            frontend,
            frontend_handle: Some(frontend_handle),
            done_tx: Some(args.done_tx),
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let Some(done_tx) = state.done_tx.take() else {
            return Ok(());
        };
        match message {
            RootMsg::Shutdown => tracing::info!("shutdown requested; stopping racli"),
            RootMsg::FrontendExited => tracing::info!("front end exited; stopping racli"),
        }

        let frontend_result = match ractor::call!(state.frontend, FrontendMsg::Stop) {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(error = %e, "front-end actor unavailable at shutdown");
                Ok(())
            }
        };
        if let Some(handle) = state.frontend_handle.take() {
            let _ = handle.await;
        }
        tracing::debug!("front end stopped");

        let backend_result = shutdown_backend(&state.backend, state.backend_handle.take()).await;

        let result = match (frontend_result, backend_result) {
            (Err(e), _) => Err(RootError::Frontend(e)),
            (Ok(()), Err(e)) => Err(RootError::Backend(e)),
            (Ok(()), Ok(())) => Ok(()),
        };
        let _ = done_tx.send(result);
        myself.stop(None);
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Children stop during teardown; only an unexpected front-end failure triggers shutdown.
        if let SupervisionEvent::ActorFailed(who, reason) = message {
            tracing::error!(actor = %who.get_id(), %reason, "child actor failed");
            if who.get_id() == state.frontend.get_id() {
                let _ = myself.cast(RootMsg::FrontendExited);
            }
        }
        Ok(())
    }
}

/// Asks the backend to shut down (watcher, then rust-analyzer) and waits for its actor to exit.
async fn shutdown_backend(
    backend: &ActorRef<BackendMsg>,
    handle: Option<JoinHandle<()>>,
) -> Result<(), RustAnalyzerError> {
    let result = match ractor::call!(backend, BackendMsg::Shutdown) {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(error = %e, "backend actor unavailable at shutdown");
            Ok(())
        }
    };
    if let Some(handle) = handle {
        let _ = handle.await;
    }
    result
}

/// Runs the actor tree until `shutdown` resolves or the front end exits, then tears it down in order.
pub(crate) async fn run_until_shutdown(
    workspace_root: PathBuf,
    symbol_search_limit: u32,
    frontend: FrontendKind,
    shutdown: impl Future<Output = ()>,
) -> Result<(), RootError> {
    let (done_tx, mut done_rx) = oneshot::channel();
    let (root, root_handle) = Actor::spawn(
        None,
        RootActor,
        RootArgs {
            workspace_root,
            symbol_search_limit,
            frontend,
            done_tx,
        },
    )
    .await
    .map_err(|e| RootError::from_startup(startup_error(e)))?;

    let done = tokio::select! {
        done = &mut done_rx => done,
        () = shutdown => {
            let _ = root.cast(RootMsg::Shutdown);
            done_rx.await
        }
    };
    let _ = root_handle.await;

    done.unwrap_or_else(|_| {
        tracing::warn!("root actor exited without reporting a shutdown result");
        Ok(())
    })
}

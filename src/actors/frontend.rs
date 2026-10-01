//! Front-end actors: the gRPC Unix-socket server and the MCP stdio server, both backed by the shared [`RacliSession`].

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::RpcReplyPort;
use rmcp::ServiceExt;
use rmcp::service::QuitReason;
use rmcp::service::RunningServiceCancellationToken;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

use crate::actors::root::RootMsg;
use crate::grpc_server::RacliGrpc;
use crate::mcp::RacliMcpHandler;
use crate::proto::racli::racli_server::RacliServer;
use crate::racli_session::RacliSession;

/// Failures binding, serving, or stopping a front end.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FrontendError {
    /// The Unix socket path could not be bound.
    #[error("failed to bind Unix socket at {path}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// tonic failed while serving gRPC.
    #[error("failed serving gRPC transport")]
    Serve(#[source] tonic::transport::Error),
    /// `rmcp` could not complete the MCP handshake over stdio.
    #[error("mcp initialization failed")]
    McpInit(Box<rmcp::service::ServerInitializeError>),
    /// The serving task panicked or was aborted.
    #[error("front-end task failed")]
    Task(#[from] tokio::task::JoinError),
}

/// Messages handled by both front-end actors.
pub(crate) enum FrontendMsg {
    /// Stops serving, waits for the serving task, replies with its result, and stops the actor.
    Stop(RpcReplyPort<Result<(), FrontendError>>),
}

/// Startup arguments for [`GrpcFrontend`].
pub(crate) struct GrpcFrontendArgs {
    /// Unix socket path to bind (removed again when the actor stops).
    pub socket_path: PathBuf,
    /// Session that serves the RPCs.
    pub session: Arc<RacliSession>,
    /// Notified with [`RootMsg::FrontendExited`] if the server stops on its own.
    pub root: ActorRef<RootMsg>,
}

/// Identifies a filesystem entry by `(device, inode)`, so a path re-bound by another process is recognized.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

/// State of [`GrpcFrontend`]: the socket path plus the running serve task and its shutdown trigger.
pub(crate) struct GrpcFrontendState {
    socket_path: PathBuf,
    /// Identity of the socket file this actor bound; removal on stop is skipped if the path now holds another file.
    socket_identity: Option<(u64, u64)>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), tonic::transport::Error>>>,
}

/// Serves Racli gRPC on a Unix socket; binding happens in `pre_start` so bind errors fail startup.
pub(crate) struct GrpcFrontend;

impl Actor for GrpcFrontend {
    type Msg = FrontendMsg;
    type State = GrpcFrontendState;
    type Arguments = GrpcFrontendArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let uds = tokio::net::UnixListener::bind(&args.socket_path).map_err(|source| {
            FrontendError::Bind {
                path: args.socket_path.clone(),
                source,
            }
        })?;
        let socket_identity = file_identity(&args.socket_path);
        let incoming = UnixListenerStream::new(uds);
        let svc = RacliGrpc::new(args.session);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        tracing::info!(
            version = %crate::VERSION,
            socket = %args.socket_path.display(),
            "racli gRPC server starting"
        );

        let root = args.root;
        let task = tokio::spawn(async move {
            let result = Server::builder()
                .add_service(RacliServer::new(svc))
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = shutdown_rx.await;
                })
                .await;
            let _ = root.cast(RootMsg::FrontendExited);
            result
        });

        Ok(GrpcFrontendState {
            socket_path: args.socket_path,
            socket_identity,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let FrontendMsg::Stop(reply) = message;
        if let Some(tx) = state.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let result = match state.task.take() {
            Some(task) => match task.await {
                Ok(served) => served.map_err(FrontendError::Serve),
                Err(e) => Err(FrontendError::Task(e)),
            },
            None => Ok(()),
        };
        let _ = reply.send(result);
        myself.stop(None);
        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // During an editor restart the next racli may already have re-bound this path; its socket
        // is a different file, so only remove the path while it is still the one we bound.
        if state.socket_identity.is_some()
            && file_identity(&state.socket_path) == state.socket_identity
        {
            let _ = std::fs::remove_file(&state.socket_path);
        } else {
            tracing::info!(
                socket = %state.socket_path.display(),
                "socket path now belongs to another racli instance; leaving it in place"
            );
        }
        Ok(())
    }
}

/// Startup arguments for [`McpFrontend`].
pub(crate) struct McpFrontendArgs {
    /// Session that serves the MCP tools.
    pub session: Arc<RacliSession>,
    /// Notified with [`RootMsg::FrontendExited`] when the MCP peer disconnects (stdin EOF).
    pub root: ActorRef<RootMsg>,
}

/// State of [`McpFrontend`]: the cancellation handle and the task waiting on the MCP service.
pub(crate) struct McpFrontendState {
    cancel: Option<RunningServiceCancellationToken>,
    task: Option<JoinHandle<Result<QuitReason, tokio::task::JoinError>>>,
}

/// Serves MCP tools on stdin/stdout; the MCP handshake happens in `pre_start` so its errors fail startup.
pub(crate) struct McpFrontend;

impl Actor for McpFrontend {
    type Msg = FrontendMsg;
    type State = McpFrontendState;
    type Arguments = McpFrontendArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let running = RacliMcpHandler::new(args.session)
            .serve((tokio::io::stdin(), tokio::io::stdout()))
            .await
            .map_err(|e| FrontendError::McpInit(Box::new(e)))?;
        let cancel = running.cancellation_token();
        let root = args.root;
        let task = tokio::spawn(async move {
            let result = running.waiting().await;
            let _ = root.cast(RootMsg::FrontendExited);
            result
        });
        Ok(McpFrontendState {
            cancel: Some(cancel),
            task: Some(task),
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let FrontendMsg::Stop(reply) = message;
        if let Some(cancel) = state.cancel.take() {
            cancel.cancel();
        }
        let result = match state.task.take() {
            Some(task) => match task.await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "MCP runtime task ended with an error");
                    Ok(())
                }
                Err(e) => Err(FrontendError::Task(e)),
            },
            None => Ok(()),
        };
        let _ = reply.send(result);
        myself.stop(None);
        Ok(())
    }
}

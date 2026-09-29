//! Actor that owns the rust-analyzer LSP child; its mailbox serializes LSP traffic.

use std::path::PathBuf;

use lsp_types::CallHierarchyItem;
use lsp_types::DidChangeWatchedFilesParams;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::RpcReplyPort;
use serde_json::Value;

use crate::proto::racli::LspServerInfo;
use crate::rust_analyzer::RustAnalyzerError;
use crate::rust_analyzer::RustAnalyzerSession;
use crate::rust_analyzer::SymbolSearchOptions;
use crate::server::Core;

/// Reply port for an LSP request that returns the raw JSON-RPC `result`.
pub(crate) type LspReply = RpcReplyPort<Result<Value, RustAnalyzerError>>;

/// Messages handled by [`RustAnalyzerActor`]; request variants mirror the [`Core`] methods.
pub(crate) enum RustAnalyzerMsg {
    /// `serverInfo` captured from LSP `initialize`.
    ServerInfo(RpcReplyPort<LspServerInfo>),
    /// LSP `workspace/symbol` (see [`Core::search`]).
    Search {
        query: String,
        options: SymbolSearchOptions,
        reply: LspReply,
    },
    /// LSP `textDocument/definition`.
    FindDefinition {
        uri: String,
        line: u32,
        character: u32,
        reply: LspReply,
    },
    /// LSP `textDocument/implementation`.
    FindImplementations {
        uri: String,
        line: u32,
        character: u32,
        reply: LspReply,
    },
    /// LSP `textDocument/references`.
    FindReferences {
        uri: String,
        line: u32,
        character: u32,
        reply: LspReply,
    },
    /// LSP `textDocument/prepareCallHierarchy`.
    PrepareCallHierarchy {
        uri: String,
        line: u32,
        character: u32,
        reply: LspReply,
    },
    /// LSP `callHierarchy/incomingCalls`.
    IncomingCalls {
        item: CallHierarchyItem,
        reply: LspReply,
    },
    /// LSP `callHierarchy/outgoingCalls`.
    OutgoingCalls {
        item: CallHierarchyItem,
        reply: LspReply,
    },
    /// LSP `textDocument/documentSymbol`.
    DocumentSymbols { uri: String, reply: LspReply },
    /// Fire-and-forget LSP `workspace/didChangeWatchedFiles` from the file watcher.
    DidChangeWatchedFiles(DidChangeWatchedFilesParams),
    /// Graceful LSP `shutdown`/`exit` and child wait; later requests fail.
    Shutdown(RpcReplyPort<Result<(), RustAnalyzerError>>),
}

/// Startup arguments for [`RustAnalyzerActor`].
pub(crate) struct RustAnalyzerArgs {
    /// Workspace root rust-analyzer is started in.
    pub workspace_root: PathBuf,
    /// Cap on `workspace/symbol` results per query.
    pub symbol_search_limit: u32,
}

/// State of [`RustAnalyzerActor`]: the live session (`None` once shut down) and stateless [`Core`] helpers.
pub(crate) struct RustAnalyzerState {
    session: Option<RustAnalyzerSession>,
    core: Core,
}

/// Spawns rust-analyzer in `pre_start` and runs one LSP request at a time against it.
pub(crate) struct RustAnalyzerActor;

/// Error returned for requests that arrive after the session was shut down.
fn session_closed() -> RustAnalyzerError {
    RustAnalyzerError::Io(std::io::Error::other(
        "rust-analyzer session already shut down",
    ))
}

/// Runs `$call` against the live session bound to `$ra` (or fails with [`session_closed`]) and sends the result on `$reply`.
macro_rules! reply_with_session {
    ($state:expr, $reply:expr, |$ra:ident| $call:expr) => {{
        let result = match $state.session.as_mut() {
            Some($ra) => $call.await,
            None => Err(session_closed()),
        };
        let _ = $reply.send(result);
    }};
}

impl Actor for RustAnalyzerActor {
    type Msg = RustAnalyzerMsg;
    type State = RustAnalyzerState;
    type Arguments = RustAnalyzerArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let session =
            RustAnalyzerSession::spawn(&args.workspace_root, args.symbol_search_limit).await?;
        Ok(RustAnalyzerState {
            session: Some(session),
            core: Core::default(),
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // LSP failures go back on the reply port; returning `Err` here would kill the actor.
        let core = state.core;
        match message {
            RustAnalyzerMsg::ServerInfo(reply) => {
                let info = state
                    .session
                    .as_ref()
                    .map(|s| s.lsp_server_info.clone())
                    .unwrap_or_default();
                let _ = reply.send(info);
            }
            RustAnalyzerMsg::Search {
                query,
                options,
                reply,
            } => reply_with_session!(state, reply, |ra| core.search(ra, query, options)),
            RustAnalyzerMsg::FindDefinition {
                uri,
                line,
                character,
                reply,
            } => reply_with_session!(state, reply, |ra| core
                .find_definition(ra, uri, line, character)),
            RustAnalyzerMsg::FindImplementations {
                uri,
                line,
                character,
                reply,
            } => reply_with_session!(state, reply, |ra| core
                .find_implementations(ra, uri, line, character)),
            RustAnalyzerMsg::FindReferences {
                uri,
                line,
                character,
                reply,
            } => reply_with_session!(state, reply, |ra| core
                .find_references(ra, uri, line, character)),
            RustAnalyzerMsg::PrepareCallHierarchy {
                uri,
                line,
                character,
                reply,
            } => reply_with_session!(state, reply, |ra| core
                .prepare_call_hierarchy(ra, uri, line, character)),
            RustAnalyzerMsg::IncomingCalls { item, reply } => {
                reply_with_session!(state, reply, |ra| core
                    .call_hierarchy_incoming_calls(ra, item))
            }
            RustAnalyzerMsg::OutgoingCalls { item, reply } => {
                reply_with_session!(state, reply, |ra| core
                    .call_hierarchy_outgoing_calls(ra, item))
            }
            RustAnalyzerMsg::DocumentSymbols { uri, reply } => {
                reply_with_session!(state, reply, |ra| core.document_symbols(ra, uri))
            }
            RustAnalyzerMsg::DidChangeWatchedFiles(params) => {
                let Some(ra) = state.session.as_mut() else {
                    tracing::debug!(
                        "dropping didChangeWatchedFiles: rust-analyzer already shut down"
                    );
                    return Ok(());
                };
                if let Err(e) = ra.notify_did_change_watched_files(params).await {
                    tracing::warn!(
                        error = %e,
                        "workspace/didChangeWatchedFiles notification failed"
                    );
                }
            }
            RustAnalyzerMsg::Shutdown(reply) => {
                let result = match state.session.take() {
                    Some(session) => session.shutdown_gracefully().await,
                    None => Ok(()),
                };
                let _ = reply.send(result);
            }
        }
        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Safety net for stops that skipped `Shutdown`; `RustAnalyzerSession`'s `Drop` kill is the last resort.
        if let Some(session) = state.session.take() {
            tracing::warn!("rust-analyzer actor stopped without Shutdown; shutting down now");
            if let Err(e) = session.shutdown_gracefully().await {
                tracing::warn!(error = %e, "rust-analyzer shutdown in post_stop failed");
            }
        }
        Ok(())
    }
}

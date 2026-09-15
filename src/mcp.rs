//! The `racli mcp` server: MCP tools on stdio via `rmcp`, served by an in-process [`RacliSession`] (rust-analyzer + file watcher).

mod mcp_proto_json;

use std::sync::Arc;

use crate::call_hierarchy;
use crate::call_hierarchy::CallHierarchyError;
use crate::call_hierarchy::CallHierarchyOutput;
use crate::call_hierarchy::CallHierarchyRequestJson;
use crate::call_hierarchy::Direction;
use mcp_proto_json::FindDefinitionRequestJson;
use mcp_proto_json::FindDefinitionResponseJson;
use mcp_proto_json::FindReferencesRequestJson;
use mcp_proto_json::FindReferencesResponseJson;
use mcp_proto_json::GetVersionResponseJson;
use mcp_proto_json::SearchRequestJson;
use mcp_proto_json::SearchResponseJson;
use mcp_proto_json::find_definition_response_proto_to_json;
use mcp_proto_json::find_references_response_proto_to_json;
use mcp_proto_json::get_version_response_proto_to_json;
use mcp_proto_json::search_response_proto_to_json;

use rmcp::ErrorData;
use rmcp::ServerHandler;
use rmcp::ServiceExt;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Json;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::Implementation;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::tool;
use rmcp::tool_handler;
use rmcp::tool_router;

use crate::grpc_server::init_grpc_server_tracing;
use crate::grpc_server::install_unix_shutdown_signals;
use crate::racli_live_backend::RacliBackendStartError;
use crate::racli_live_backend::RacliLiveBackend;
use crate::racli_session::RacliRpcError;
use crate::racli_session::RacliSession;
use crate::rust_analyzer::RustAnalyzerError;

/// Failures during the MCP lifecycle on stdio or the embedded workspace backend.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// `rmcp` could not complete the MCP handshake over stdio.
    #[error("mcp initialization failed")]
    McpInit(Box<rmcp::service::ServerInitializeError>),
    /// The process working directory could not be read (workspace root for LSP).
    #[error("failed to read current working directory")]
    CurrentDir {
        #[source]
        source: std::io::Error,
    },
    /// Spawning rust-analyzer or starting the workspace watcher failed.
    #[error(transparent)]
    BackendStart(#[from] RacliBackendStartError),
    /// Shutting down rust-analyzer after MCP exited failed.
    #[error(transparent)]
    BackendShutdown(#[from] RustAnalyzerError),
}

impl From<rmcp::service::ServerInitializeError> for ServerError {
    fn from(value: rmcp::service::ServerInitializeError) -> Self {
        Self::McpInit(Box::new(value))
    }
}

/// MCP server state: shared [`RacliSession`], plus a generated [`ToolRouter`].
#[derive(Clone)]
pub(crate) struct RacliMcpHandler {
    session: Arc<RacliSession>,
    tool_router: ToolRouter<Self>,
}

impl RacliMcpHandler {
    /// Creates a handler that serves tools from `session` (rust-analyzer-backed).
    pub fn new(session: Arc<RacliSession>) -> Self {
        Self {
            session,
            tool_router: Self::tool_router(),
        }
    }

    fn racli_rpc_error_to_mcp(err: RacliRpcError) -> ErrorData {
        match err {
            RacliRpcError::InvalidArgument(msg) => ErrorData::invalid_params(msg, None),
            RacliRpcError::Internal(msg) => ErrorData::internal_error(msg, None),
        }
    }

    fn call_hierarchy_error_to_mcp(err: CallHierarchyError) -> ErrorData {
        match err {
            CallHierarchyError::NoItem => {
                ErrorData::invalid_params("no call hierarchy item at that position", None)
            }
            CallHierarchyError::Backend(msg) => ErrorData::internal_error(msg, None),
        }
    }
}

#[tool_router(router = tool_router)]
impl RacliMcpHandler {
    /// Returns the running racli version and rust-analyzer LSP serverInfo (`Racli.GetVersion`).
    #[tool(
        name = "get_version",
        description = "Returns crate version string and rust-analyzer serverInfo from LSP initialize (mirrors gRPC Racli.GetVersion)."
    )]
    async fn get_version(&self) -> Result<Json<GetVersionResponseJson>, ErrorData> {
        let resp = self.session.get_version();
        Ok(Json(get_version_response_proto_to_json(&resp)))
    }

    /// Runs workspace symbol resolution (`Racli.Search`).
    #[tool(
        name = "search",
        description = "Runs LSP workspace/symbol via rust-analyzer with the racli merged query semantics (mirrors gRPC Racli.Search)."
    )]
    async fn search_symbols(
        &self,
        Parameters(req): Parameters<SearchRequestJson>,
    ) -> Result<Json<SearchResponseJson>, ErrorData> {
        let resp = self
            .session
            .search(req.query)
            .await
            .map_err(Self::racli_rpc_error_to_mcp)?;
        Ok(Json(search_response_proto_to_json(&resp)))
    }

    /// Runs go-to-definition at a path + LSP position (`Racli.FindDefinition`).
    #[tool(
        name = "find_definition",
        description = "Runs LSP textDocument/definition for file_path and 0-based line/character UTF-16 (mirrors gRPC Racli.FindDefinition)."
    )]
    async fn find_definition(
        &self,
        Parameters(req): Parameters<FindDefinitionRequestJson>,
    ) -> Result<Json<FindDefinitionResponseJson>, ErrorData> {
        let resp = self
            .session
            .find_definition(req.file_path, req.line, req.character)
            .await
            .map_err(Self::racli_rpc_error_to_mcp)?;
        Ok(Json(find_definition_response_proto_to_json(&resp)))
    }

    /// Lists references (declaration included) at a path + LSP position (`Racli.FindReferences`).
    #[tool(
        name = "find_references",
        description = "Runs LSP textDocument/references (declaration included) for file_path and 0-based line/character UTF-16 (mirrors gRPC Racli.FindReferences)."
    )]
    async fn find_references(
        &self,
        Parameters(req): Parameters<FindReferencesRequestJson>,
    ) -> Result<Json<FindReferencesResponseJson>, ErrorData> {
        let resp = self
            .session
            .find_references(req.file_path, req.line, req.character)
            .await
            .map_err(Self::racli_rpc_error_to_mcp)?;
        Ok(Json(find_references_response_proto_to_json(&resp)))
    }

    /// Walks the call hierarchy (callers and/or callees) at a path + LSP position, resolving one
    /// item via `prepareCallHierarchy` then recursing up to `depth` levels of
    /// `incomingCalls`/`outgoingCalls`. Pair with `find_references` for full blast-radius /
    /// rename-safety analysis: `find_references` finds every usage site, `call_hierarchy` follows
    /// the call graph through those sites.
    #[tool(
        name = "call_hierarchy",
        description = "Runs LSP prepareCallHierarchy then walks incoming/outgoingCalls up to `depth` levels (default 1) for file_path and 0-based line/character UTF-16. direction is incoming, outgoing, or both (default). Pair with find_references for full blast-radius / rename-safety analysis."
    )]
    async fn call_hierarchy(
        &self,
        Parameters(req): Parameters<CallHierarchyRequestJson>,
    ) -> Result<Json<CallHierarchyOutput>, ErrorData> {
        call_hierarchy::run_call_hierarchy(
            self.session.as_ref(),
            req.file_path,
            req.line,
            req.character,
            req.direction.unwrap_or(Direction::Both),
            req.depth.unwrap_or(1),
        )
        .await
        .map(Json)
        .map_err(Self::call_hierarchy_error_to_mcp)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RacliMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), crate::VERSION))
    }
}

/// Serves MCP on stdin/stdout after starting rust-analyzer and the workspace file watcher in-process.
pub async fn run_stdio() -> Result<(), ServerError> {
    let _log_guard = init_grpc_server_tracing();

    // Install the shutdown signal handlers before any of the (potentially slow) startup work
    // below (rust-analyzer spawn + LSP initialize), so a Ctrl+C/SIGTERM during startup is
    // recorded by tokio instead of falling back to the OS default disposition (immediate
    // termination, skipping rust-analyzer's graceful LSP shutdown). See
    // `grpc_server::install_unix_shutdown_signals`.
    let shutdown_signal = install_unix_shutdown_signals();

    let cwd = std::env::current_dir().map_err(|source| ServerError::CurrentDir { source })?;

    tracing::info!(
        version = %crate::VERSION,
        workspace = %cwd.display(),
        "racli MCP server starting on stdio (embedded rust-analyzer)"
    );

    let backend = RacliLiveBackend::start(cwd).await?;

    let handler = RacliMcpHandler::new(backend.session().clone());
    let running = match handler
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let _ = backend.shutdown().await;
            return Err(e.into());
        }
    };

    tokio::select! {
        result = running.waiting() => {
            if let Err(e) = result {
                tracing::warn!(error = %e, "MCP runtime task ended with an error");
            }
        }
        () = shutdown_signal => {
            tracing::info!("received shutdown signal; stopping MCP server");
        }
    }

    backend.shutdown().await?;

    Ok(())
}

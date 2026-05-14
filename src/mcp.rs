//! The `racli mcp` server: MCP tools on stdio via `rmcp`, forwarding each tool call to `racli server` gRPC.

mod mcp_proto_json;

use std::path::PathBuf;

use mcp_proto_json::FindDefinitionRequestJson;
use mcp_proto_json::FindDefinitionResponseJson;
use mcp_proto_json::GetVersionResponseJson;
use mcp_proto_json::SearchRequestJson;
use mcp_proto_json::SearchResponseJson;
use mcp_proto_json::find_definition_response_proto_to_json;
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

/// Failures during the MCP lifecycle on stdio (gRPC errors surface as MCP tool errors).
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// `rmcp` could not complete the MCP handshake over stdio.
    #[error("mcp initialization failed")]
    McpInit(Box<rmcp::service::ServerInitializeError>),
}

impl From<rmcp::service::ServerInitializeError> for ServerError {
    fn from(value: rmcp::service::ServerInitializeError) -> Self {
        Self::McpInit(Box::new(value))
    }
}

/// MCP server state: Unix socket path for gRPC to `racli server`, plus a generated [`ToolRouter`].
#[derive(Clone)]
pub(crate) struct RacliMcpHandler {
    grpc_socket: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl RacliMcpHandler {
    /// Creates a handler that forwards tool calls to `racli server` at `grpc_socket`.
    pub fn new(grpc_socket: PathBuf) -> Self {
        Self {
            grpc_socket,
            tool_router: Self::tool_router(),
        }
    }

    fn grpc_err(err: impl std::fmt::Display) -> ErrorData {
        ErrorData::internal_error(err.to_string(), None)
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
        let resp = crate::client::get_version(&self.grpc_socket)
            .await
            .map_err(Self::grpc_err)?;
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
        let resp = crate::client::search(&self.grpc_socket, req.query)
            .await
            .map_err(Self::grpc_err)?;
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
        let resp = crate::client::find_definition(
            &self.grpc_socket,
            req.file_path,
            req.line,
            req.character,
        )
        .await
        .map_err(Self::grpc_err)?;
        Ok(Json(find_definition_response_proto_to_json(&resp)))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RacliMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), crate::VERSION))
    }
}

/// Serves MCP on stdin/stdout; each tool issues gRPC to `racli server` at [`crate::effective_unix_socket_path`].
pub async fn run_stdio() -> Result<(), ServerError> {
    init_grpc_server_tracing();

    let grpc_socket = crate::effective_unix_socket_path();
    tracing::info!(
        version = %crate::VERSION,
        grpc_socket = %grpc_socket.display(),
        "racli MCP server starting on stdio"
    );

    let handler = RacliMcpHandler::new(grpc_socket);
    let running = handler
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await?;

    if let Err(e) = running.waiting().await {
        tracing::warn!(error = %e, "MCP runtime task ended with an error");
    }

    Ok(())
}

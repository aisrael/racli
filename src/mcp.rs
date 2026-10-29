//! The `racli mcp` server: MCP tools on stdio via `rmcp`, served by an in-process `RacliSession`
//! (rust-analyzer). Each tool runs its CLI command and returns what the command prints (ADR 006).

mod mcp_proto_json;
mod params;

use std::sync::Arc;

use mcp_proto_json::GetVersionResponseJson;
use mcp_proto_json::get_version_response_proto_to_json;
use params::CallHierarchyParams;
use params::DocumentSymbolsParams;
use params::FindDefinitionParams;
use params::PositionParams;
use params::SearchParams;

use rmcp::ErrorData;
use rmcp::ServerHandler;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::model::ContentBlock;
use rmcp::model::Implementation;
use rmcp::model::MetaObject;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerConfig;
use rmcp::tool;
use rmcp::tool_handler;
use rmcp::tool_router;
use serde_json::Value;

use crate::actors::backend::BackendStartError;
use crate::actors::frontend::FrontendError;
use crate::actors::root::FrontendKind;
use crate::actors::root::RootError;
use crate::actors::root::run_until_shutdown;
use crate::backend_status::BackendStatus;
use crate::call_hierarchy::run_call_hierarchy_command;
use crate::command_output::CommandError;
use crate::command_output::CommandOutput;
use crate::document_symbols::run_document_symbols;
use crate::find_definition::run_find_definition;
use crate::find_implementations::run_find_implementations;
use crate::find_references::run_find_references;
use crate::grpc_server::install_unix_shutdown_signals;
use crate::logging::init_server_tracing;
use crate::racli_session::RacliSession;
use crate::rust_analyzer::RustAnalyzerError;
use crate::search::run_search;

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
    BackendStart(#[from] BackendStartError),
    /// Shutting down rust-analyzer after MCP exited failed.
    #[error(transparent)]
    BackendShutdown(#[from] RustAnalyzerError),
    /// An internal actor failed outside of rust-analyzer or the MCP handshake.
    #[error("racli actor failed: {0}")]
    Actor(String),
}

impl From<RootError> for ServerError {
    fn from(value: RootError) -> Self {
        match value {
            RootError::StartBackend(e) => Self::BackendStart(e),
            RootError::StartFrontend(FrontendError::McpInit(e))
            | RootError::Frontend(FrontendError::McpInit(e)) => Self::McpInit(e),
            RootError::StartFrontend(e) | RootError::Frontend(e) => Self::Actor(e.to_string()),
            RootError::Backend(e) => Self::BackendShutdown(e),
        }
    }
}

impl From<rmcp::service::ServerInitializeError> for ServerError {
    fn from(value: rmcp::service::ServerInitializeError) -> Self {
        Self::McpInit(Box::new(value))
    }
}

/// MCP server state: shared `RacliSession`, plus a generated `ToolRouter`.
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

    /// The tool result for a command that wrote `out` and ended with `result`, with the backend's
    /// current status (see [`command_result`]).
    fn tool_result(
        &self,
        out: &CommandOutput,
        result: Result<(), CommandError>,
    ) -> Result<CallToolResult, ErrorData> {
        command_result(out, result, &self.session.backend_status())
    }

    /// The tool result for arguments the command would reject (see [`rejected_result`]).
    fn rejected(&self, reason: String) -> CallToolResult {
        rejected_result(reason, &self.session.backend_status())
    }
}

/// The tool result for a command that wrote `out` and ended with `result` (ADR 006 decisions 1, 4
/// and 5): its standard output, then its standard error and a warning when rust-analyzer isn't
/// ready, with `status` in `_meta`, and `isError` if the backend answered for no target.
fn command_result(
    out: &CommandOutput,
    result: Result<(), CommandError>,
    status: &BackendStatus,
) -> Result<CallToolResult, ErrorData> {
    match result {
        Ok(()) => {}
        Err(CommandError::Usage(message)) => return Ok(rejected_result(message, status)),
        Err(CommandError::Failed(message)) => {
            return Err(ErrorData::internal_error(message, None));
        }
    }
    let mut content = vec![ContentBlock::text(out.stdout())];
    let stderr = out.stderr_messages().join("\n");
    if !stderr.is_empty() {
        content.push(ContentBlock::text(stderr));
    }
    if let Some(warning) = status_warning(status) {
        content.push(ContentBlock::text(warning));
    }
    let mut tool_result = if out.answered() {
        CallToolResult::success(content)
    } else {
        CallToolResult::error(content)
    };
    tool_result.meta = Some(status_meta(status));
    Ok(tool_result)
}

/// The tool result for arguments the command would reject: `isError`, with the reason, so the
/// caller can retry, as rmcp does for arguments that don't match the schema.
fn rejected_result(reason: String, status: &BackendStatus) -> CallToolResult {
    let mut tool_result = CallToolResult::error(vec![ContentBlock::text(reason)]);
    tool_result.meta = Some(status_meta(status));
    tool_result
}

/// `_meta` for a tool result: `racli/backend`, and `racli/backendMessage` with `error`.
fn status_meta(status: &BackendStatus) -> MetaObject {
    let mut meta = serde_json::Map::new();
    let name = match status {
        BackendStatus::Ready => "ready",
        BackendStatus::Indexing => "indexing",
        BackendStatus::Error(_) => "error",
        BackendStatus::Unknown => return MetaObject::from(meta),
    };
    meta.insert("racli/backend".into(), Value::from(name));
    if let BackendStatus::Error(message) = status {
        meta.insert("racli/backendMessage".into(), Value::from(message.as_str()));
    }
    MetaObject::from(meta)
}

/// The text block that says a result may be incomplete, unless rust-analyzer is ready.
fn status_warning(status: &BackendStatus) -> Option<String> {
    match status {
        BackendStatus::Ready | BackendStatus::Unknown => None,
        BackendStatus::Indexing => {
            Some("rust-analyzer is still indexing; results may be incomplete".into())
        }
        BackendStatus::Error(message) => Some(format!(
            "rust-analyzer reported an error ({message}); results may be incomplete"
        )),
    }
}

#[tool_router(router = tool_router)]
impl RacliMcpHandler {
    /// Returns the racli version and rust-analyzer's LSP serverInfo (`racli version`).
    #[tool(
        name = "get_version",
        description = "Returns racli's version and rust-analyzer's serverInfo, as JSON. Its _meta says whether rust-analyzer is ready, still indexing, or reported an error (racli/backend).",
        output_schema = rmcp::handler::server::tool::schema_for_output::<GetVersionResponseJson>()
    )]
    async fn get_version(&self) -> Result<CallToolResult, ErrorData> {
        let resp = get_version_response_proto_to_json(&self.session.get_version());
        let value = serde_json::to_value(resp)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let mut tool_result = CallToolResult::structured(value);
        tool_result.meta = Some(status_meta(&self.session.backend_status()));
        Ok(tool_result)
    }

    /// Runs `racli search`.
    #[tool(
        name = "search",
        description = "Fuzzy workspace symbol search (racli search). Returns one `name [KIND] path:line:col` line per symbol, 1-based. Only types unless kind is all_symbols (adds functions, methods, constants, fields); scope workspace_and_dependencies adds dependencies and std; exact keeps only exact names. `|` separates alternative patterns."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = params.into_args();
        let mut out = CommandOutput::default();
        let result = run_search(self.session.as_ref(), &args, &mut out).await;
        self.tool_result(&out, result)
    }

    /// Runs `racli find-definition`.
    #[tool(
        name = "find_definition",
        description = "Where symbols are defined (racli find-definition), by names such as `mkfifo`, `sys::mkfifo` or `Fifo::name`, and/or a 1-based position (path, line, character). Returns `path:line:col: source line` per definition, under a header per name. hover adds the signature and doc comment; source adds the whole body. kind and glob narrow the definitions names find."
    )]
    async fn find_definition(
        &self,
        Parameters(params): Parameters<FindDefinitionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = match params.into_args() {
            Ok(args) => args,
            Err(reason) => return Ok(self.rejected(reason)),
        };
        let mut out = CommandOutput::default();
        let result = run_find_definition(self.session.as_ref(), &args, &mut out).await;
        self.tool_result(&out, result)
    }

    /// Runs `racli find-references`.
    #[tool(
        name = "find_references",
        description = "Every use of symbols, declaration included (racli find-references), by names such as `mkfifo`, `sys::mkfifo` or `Fifo::name`, and/or a 1-based position (path, line, character). Returns `path:line:col: source line` per reference, under a header per definition. kind and glob narrow the definitions names find."
    )]
    async fn find_references(
        &self,
        Parameters(params): Parameters<PositionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = match params.into_find_references_args() {
            Ok(args) => args,
            Err(reason) => return Ok(self.rejected(reason)),
        };
        let mut out = CommandOutput::default();
        let result = run_find_references(self.session.as_ref(), &args, &mut out).await;
        self.tool_result(&out, result)
    }

    /// Runs `racli find-implementations`.
    #[tool(
        name = "find_implementations",
        description = "A trait's or type's impl blocks, or a trait method's overrides (racli find-implementations), by names or a 1-based position (path, line, character). Returns `path:line:col: source line` per implementation. Nothing from inside an impl block."
    )]
    async fn find_implementations(
        &self,
        Parameters(params): Parameters<PositionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = match params.into_find_implementations_args() {
            Ok(args) => args,
            Err(reason) => return Ok(self.rejected(reason)),
        };
        let mut out = CommandOutput::default();
        let result = run_find_implementations(self.session.as_ref(), &args, &mut out).await;
        self.tool_result(&out, result)
    }

    /// Runs `racli call-hierarchy`. Pair with `find_references` for blast-radius analysis:
    /// `find_references` finds every use, `call_hierarchy` follows the call graph.
    #[tool(
        name = "call_hierarchy",
        description = "Callers and callees of functions (racli call-hierarchy), by names or a 1-based position (path, line, character). Returns an indented tree of `name (KIND) path:line:col`. direction is incoming, outgoing or both (default); depth is how many levels (default 1). Pair with find_references for blast-radius analysis."
    )]
    async fn call_hierarchy(
        &self,
        Parameters(params): Parameters<CallHierarchyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = match params.into_args() {
            Ok(args) => args,
            Err(reason) => return Ok(self.rejected(reason)),
        };
        let mut out = CommandOutput::default();
        let session = self.session.as_ref();
        let result = run_call_hierarchy_command(session, session, &args, &mut out).await;
        self.tool_result(&out, result)
    }

    /// Runs `racli document-symbols`.
    #[tool(
        name = "document_symbols",
        description = "A file's symbol outline (racli document-symbols), instead of reading the whole file. Returns an indented tree of `name (KIND) start-end` with 1-based ranges."
    )]
    async fn document_symbols(
        &self,
        Parameters(params): Parameters<DocumentSymbolsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = params.into_args();
        let mut out = CommandOutput::default();
        let result = run_document_symbols(self.session.as_ref(), &args, &mut out).await;
        self.tool_result(&out, result)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RacliMcpHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), crate::VERSION))
    }
}

/// Serves MCP on stdin/stdout after starting rust-analyzer (capping `workspace/symbol` at `symbol_search_limit`) and the workspace file watcher in-process.
pub async fn run_stdio(symbol_search_limit: u32) -> Result<(), ServerError> {
    let _log_guard = init_server_tracing();

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

    run_until_shutdown(
        cwd,
        symbol_search_limit,
        FrontendKind::McpStdio,
        shutdown_signal,
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use rmcp::model::ErrorCode;
    use serde_json::json;

    use super::*;

    /// A result's text blocks, whether it has `isError`, and its `_meta`.
    fn parts(result: &CallToolResult) -> (Vec<String>, bool, Value) {
        let value = serde_json::to_value(result).unwrap();
        let texts = value["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["text"].as_str().unwrap().to_string())
            .collect();
        (texts, value["isError"] == true, value["_meta"].clone())
    }

    /// The output of a command the backend answered, with one result line.
    fn answered_output() -> CommandOutput {
        let mut out = CommandOutput::default();
        out.println("src/sys.rs:3:8: pub fn mkfifo");
        out.mark_answered();
        out
    }

    #[test]
    fn a_ready_result_is_stdout_with_the_status_in_meta() {
        let result = command_result(&answered_output(), Ok(()), &BackendStatus::Ready).unwrap();
        let (texts, is_error, meta) = parts(&result);
        assert_eq!(texts, ["src/sys.rs:3:8: pub fn mkfifo\n"]);
        assert!(!is_error);
        assert_eq!(meta, json!({"racli/backend": "ready"}));
    }

    #[test]
    fn stderr_then_the_indexing_warning_follow_stdout() {
        let mut out = answered_output();
        out.backend_error("no symbol named mkfif; closest:\n  mkfifo [FUNCTION] src/sys.rs:3:8");
        out.note("did you mean mkfifo at src/sys.rs:25:16?");
        let result = command_result(&out, Ok(()), &BackendStatus::Indexing).unwrap();
        let (texts, is_error, meta) = parts(&result);
        assert_eq!(texts.len(), 3);
        assert_eq!(
            texts[1],
            "no symbol named mkfif; closest:\n  mkfifo [FUNCTION] src/sys.rs:3:8\n\
             did you mean mkfifo at src/sys.rs:25:16?"
        );
        assert!(texts[2].contains("still indexing"), "{}", texts[2]);
        assert!(!is_error);
        assert_eq!(meta, json!({"racli/backend": "indexing"}));
    }

    #[test]
    fn an_error_status_carries_its_message() {
        let status = BackendStatus::Error("no workspace".into());
        let result = command_result(&answered_output(), Ok(()), &status).unwrap();
        let (texts, _, meta) = parts(&result);
        assert!(texts[1].contains("no workspace"), "{}", texts[1]);
        assert_eq!(
            meta,
            json!({"racli/backend": "error", "racli/backendMessage": "no workspace"})
        );
    }

    #[test]
    fn an_unknown_status_adds_no_warning_or_meta_keys() {
        let result = command_result(&answered_output(), Ok(()), &BackendStatus::Unknown).unwrap();
        let (texts, _, meta) = parts(&result);
        assert_eq!(texts.len(), 1);
        assert_eq!(meta, json!({}));
    }

    #[test]
    fn nothing_answered_is_an_error_result_that_keeps_stdout_first() {
        let mut out = CommandOutput::default();
        out.println("[]");
        out.backend_error("no symbol named mkfif");
        let result = command_result(&out, Ok(()), &BackendStatus::Ready).unwrap();
        let (texts, is_error, _) = parts(&result);
        assert!(is_error);
        assert_eq!(texts, ["[]\n", "no symbol named mkfif"]);
    }

    #[test]
    fn a_usage_error_is_an_error_result_with_the_reason() {
        let usage = Err(CommandError::Usage(
            "path, line and character are given together".into(),
        ));
        let result =
            command_result(&CommandOutput::default(), usage, &BackendStatus::Ready).unwrap();
        let (texts, is_error, meta) = parts(&result);
        assert!(is_error);
        assert_eq!(texts, ["path, line and character are given together"]);
        assert_eq!(meta, json!({"racli/backend": "ready"}));
    }

    #[test]
    fn a_failed_backend_call_is_an_internal_error() {
        let failed = Err(CommandError::Failed(
            "request timed out after 60 seconds".into(),
        ));
        let Err(error) = command_result(&CommandOutput::default(), failed, &BackendStatus::Ready)
        else {
            panic!("expected an internal error");
        };
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, "request timed out after 60 seconds");
    }
}

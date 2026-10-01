//! Spawns `rust-analyzer` as an LSP stdio child, initializes the workspace from a root path, and shuts down with LSP `shutdown` / `exit`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use clap::ValueEnum;
use lsp_types::CallHierarchyIncomingCallsParams;
use lsp_types::CallHierarchyItem;
use lsp_types::CallHierarchyOutgoingCallsParams;
use lsp_types::CallHierarchyPrepareParams;
use lsp_types::ClientCapabilities;
use lsp_types::ClientInfo;
use lsp_types::CodeActionClientCapabilities;
use lsp_types::CodeActionKind;
use lsp_types::CodeActionKindLiteralSupport;
use lsp_types::CodeActionLiteralSupport;
use lsp_types::CompletionClientCapabilities;
use lsp_types::CompletionItemCapability;
use lsp_types::CompletionItemCapabilityResolveSupport;
use lsp_types::DidChangeWatchedFilesClientCapabilities;
use lsp_types::DidChangeWatchedFilesParams;
use lsp_types::DocumentSymbolClientCapabilities;
use lsp_types::DocumentSymbolParams;
use lsp_types::GotoDefinitionParams;
use lsp_types::HoverClientCapabilities;
use lsp_types::InitializeParams;
use lsp_types::InlayHintClientCapabilities;
use lsp_types::MarkupKind;
use lsp_types::ParameterInformationSettings;
use lsp_types::PartialResultParams;
use lsp_types::Position;
use lsp_types::PublishDiagnosticsClientCapabilities;
use lsp_types::ReferenceContext;
use lsp_types::ReferenceParams;
use lsp_types::RenameClientCapabilities;
use lsp_types::SemanticTokensClientCapabilities;
use lsp_types::SemanticTokensClientCapabilitiesRequests;
use lsp_types::SemanticTokensFullOptions;
use lsp_types::SignatureHelpClientCapabilities;
use lsp_types::SignatureInformationSettings;
use lsp_types::TextDocumentClientCapabilities;
use lsp_types::TextDocumentIdentifier;
use lsp_types::TextDocumentPositionParams;
use lsp_types::TextDocumentSyncClientCapabilities;
use lsp_types::TokenFormat;
use lsp_types::Uri;
use lsp_types::WorkDoneProgressParams;
use lsp_types::WorkspaceClientCapabilities;
use lsp_types::WorkspaceFolder;
use lsp_types::WorkspaceSymbolResponse;
use lsp_types::notification::DidChangeWatchedFiles;
use lsp_types::notification::Notification;
use lsp_types::request::CallHierarchyIncomingCalls;
use lsp_types::request::CallHierarchyOutgoingCalls;
use lsp_types::request::CallHierarchyPrepare;
use lsp_types::request::DocumentSymbolRequest;
use lsp_types::request::GotoDefinition;
use lsp_types::request::GotoImplementation;
use lsp_types::request::GotoImplementationParams;
use lsp_types::request::References;
use lsp_types::request::Request;
use lsp_types::request::WorkspaceSymbolRequest;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::process::Child;
use tokio::process::Command;
use tokio::task::JoinHandle;
use url::Url;

use crate::lsp_client::LspClient;
use crate::lsp_client::transport::io_transport;
use crate::lsp_events::FORWARDED_METHODS;
use crate::lsp_events::LspEvents;
use crate::proto::racli::LspServerInfo;

/// Default `workspace.symbol.search.limit` sent to rust-analyzer at startup (rust-analyzer's own default is 128).
pub const DEFAULT_SYMBOL_SEARCH_LIMIT: u32 = 1000;

/// Which symbol kinds rust-analyzer's `workspace/symbol` returns (its `searchKind` LSP extension).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolSearchKind {
    /// Types only (modules, structs, enums, traits, type aliases); rust-analyzer's default.
    OnlyTypes,
    /// All symbols, including functions, methods, constants, statics, and fields.
    AllSymbols,
}

/// Which crates rust-analyzer's `workspace/symbol` searches (its `searchScope` LSP extension).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolSearchScope {
    /// Workspace crates only; rust-analyzer's default.
    Workspace,
    /// Workspace crates plus their dependencies (including the standard library).
    WorkspaceAndDependencies,
}

/// Per-request `workspace/symbol` overrides; `None` leaves rust-analyzer's configured default in effect.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SymbolSearchOptions {
    /// Symbol kinds to return.
    pub kind: Option<SymbolSearchKind>,
    /// Crates to search.
    pub scope: Option<SymbolSearchScope>,
}

/// `workspace/symbol` params including rust-analyzer's `searchKind` / `searchScope` extension fields.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RaWorkspaceSymbolParams {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_scope: Option<String>,
}

impl RaWorkspaceSymbolParams {
    /// Builds params for `query`, mapping `options` to rust-analyzer's camelCase extension values.
    fn new(query: String, options: SymbolSearchOptions) -> Self {
        let search_kind = options.kind.map(|k| {
            match k {
                SymbolSearchKind::OnlyTypes => "onlyTypes",
                SymbolSearchKind::AllSymbols => "allSymbols",
            }
            .to_string()
        });
        let search_scope = options.scope.map(|s| {
            match s {
                SymbolSearchScope::Workspace => "workspace",
                SymbolSearchScope::WorkspaceAndDependencies => "workspaceAndDependencies",
            }
            .to_string()
        });
        Self {
            query,
            search_kind,
            search_scope,
        }
    }
}

/// `workspace/symbol` request typed with [`RaWorkspaceSymbolParams`] instead of the plain LSP params.
enum RaWorkspaceSymbolRequest {}

impl Request for RaWorkspaceSymbolRequest {
    type Params = RaWorkspaceSymbolParams;
    type Result = Option<WorkspaceSymbolResponse>;
    const METHOD: &'static str = WorkspaceSymbolRequest::METHOD;
}

/// Client capabilities advertised to rust-analyzer during LSP `initialize`: watched-files dynamic
/// registration, hierarchical `textDocument/documentSymbol` (otherwise servers return a flat
/// `SymbolInformation[]`), and editor-level text document features for `racli tee`. Capabilities
/// that make the server send requests to the client (configuration, applyEdit, refresh, progress)
/// are deliberately omitted because racli cannot forward those to an editor; `experimental/serverStatus`
/// notifications are requested so editors can show rust-analyzer's health.
fn racli_lsp_client_capabilities() -> ClientCapabilities {
    let markup = || Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]);
    ClientCapabilities {
        workspace: Some(WorkspaceClientCapabilities {
            did_change_watched_files: Some(DidChangeWatchedFilesClientCapabilities {
                dynamic_registration: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities {
                did_save: Some(true),
                ..Default::default()
            }),
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(true),
                    documentation_format: markup(),
                    resolve_support: Some(CompletionItemCapabilityResolveSupport {
                        properties: vec![
                            "documentation".into(),
                            "detail".into(),
                            "additionalTextEdits".into(),
                        ],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            hover: Some(HoverClientCapabilities {
                content_format: markup(),
                ..Default::default()
            }),
            signature_help: Some(SignatureHelpClientCapabilities {
                signature_information: Some(SignatureInformationSettings {
                    documentation_format: markup(),
                    parameter_information: Some(ParameterInformationSettings {
                        label_offset_support: Some(true),
                    }),
                    active_parameter_support: Some(true),
                }),
                ..Default::default()
            }),
            code_action: Some(CodeActionClientCapabilities {
                code_action_literal_support: Some(CodeActionLiteralSupport {
                    code_action_kind: CodeActionKindLiteralSupport {
                        value_set: [
                            CodeActionKind::EMPTY,
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::REFACTOR,
                            CodeActionKind::REFACTOR_EXTRACT,
                            CodeActionKind::REFACTOR_INLINE,
                            CodeActionKind::REFACTOR_REWRITE,
                            CodeActionKind::SOURCE,
                            CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
                        ]
                        .into_iter()
                        .map(|k| k.as_str().to_string())
                        .collect(),
                    },
                }),
                ..Default::default()
            }),
            rename: Some(RenameClientCapabilities {
                prepare_support: Some(true),
                ..Default::default()
            }),
            semantic_tokens: Some(SemanticTokensClientCapabilities {
                requests: SemanticTokensClientCapabilitiesRequests {
                    range: Some(true),
                    full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                },
                formats: vec![TokenFormat::RELATIVE],
                ..Default::default()
            }),
            inlay_hint: Some(InlayHintClientCapabilities::default()),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                related_information: Some(true),
                ..Default::default()
            }),
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        // Lets editors behind `racli tee` show rust-analyzer's health (see `lsp_events::SERVER_STATUS`).
        experimental: Some(serde_json::json!({ "serverStatusNotification": true })),
        ..Default::default()
    }
}

/// Puts the child in its own process group so a terminal Ctrl+C (`SIGINT`) does not also kill rust-analyzer before LSP shutdown.
#[cfg(unix)]
fn configure_command_isolated_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `pre_exec` runs in the child after fork and before exec; `setpgid(0,0)` is the
    // standard pattern to isolate the child from the parent's terminal process group.
    unsafe {
        cmd.as_std_mut().pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_command_isolated_process_group(_cmd: &mut Command) {}

/// Failures spawning `rust-analyzer`, during LSP stdio I/O, or when JSON-RPC returns an error.
#[derive(Debug, thiserror::Error)]
pub enum RustAnalyzerError {
    /// The workspace directory could not be turned into a `file://` URI.
    #[error("invalid workspace directory for file URL")]
    InvalidWorkspaceUrl,
    /// A source file path could not be turned into a `file://` document URI.
    #[error("invalid source file path for file URL")]
    InvalidDocumentUrl,
    /// Failed to start the `rust-analyzer` process.
    #[error("failed to spawn rust-analyzer")]
    Spawn(#[source] std::io::Error),
    /// Stdin/stdout on the child process failed.
    #[error("rust-analyzer I/O error")]
    Io(#[source] std::io::Error),
    /// [`LspClient`] / JSON-RPC stack error (including transport).
    #[error(transparent)]
    Lsp(#[from] crate::lsp_client::LspError),
    /// JSON parse/build error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// JSON-RPC `error` object in a response (e.g. conflicting merged search shapes).
    #[error("rust-analyzer: {0}")]
    Rpc(String),
    /// rust-analyzer exited before completing LSP `initialize`; its own stderr explains why.
    #[error(
        "rust-analyzer exited during startup ({status}); see its error output above for the cause \
         (for example, rustup reporting that the rust-analyzer component isn't installed for this \
         project's toolchain)"
    )]
    ExitedDuringStartup { status: std::process::ExitStatus },
}

/// Owns a running `rust-analyzer` child and an [`LspClient`] over stdio.
pub struct RustAnalyzerSession {
    child: Child,
    /// Wrapped so [`RustAnalyzerSession::shutdown_gracefully`] can consume the client before waiting on [`Child`] (this type implements [`Drop`]).
    lsp: Option<LspClient>,
    /// OS process id captured right after spawn (still logged after `wait` when [`Child::id`] is unset).
    child_pid: Option<u32>,
    /// Set when [`RustAnalyzerSession::shutdown_gracefully`] finishes so [`Drop`] does not log an abnormal teardown.
    shutdown_complete: bool,
    /// `serverInfo` from the LSP [`initialize`](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#initialize) result.
    pub lsp_server_info: LspServerInfo,
    /// The full LSP `InitializeResult` as JSON, handed to `racli tee` editors.
    pub initialize_result: Value,
    /// Hub receiving rust-analyzer's server-to-client notifications.
    pub events: Arc<LspEvents>,
    /// Tasks forwarding notification subscriptions into [`Self::events`]; aborted on teardown.
    event_tasks: Vec<JoinHandle<()>>,
}

impl RustAnalyzerSession {
    /// Spawns `rust-analyzer` in `workspace_root`, sends `initialize` (capping `workspace/symbol` at `symbol_search_limit`) and `initialized`, and returns a live session.
    pub async fn spawn(
        workspace_root: &Path,
        symbol_search_limit: u32,
    ) -> Result<Self, RustAnalyzerError> {
        let root_uri_str = workspace_uri(workspace_root)?;
        let root_uri: Uri = root_uri_str
            .parse()
            .map_err(|_| RustAnalyzerError::InvalidWorkspaceUrl)?;

        let mut cmd = Command::new("rust-analyzer");
        cmd.current_dir(workspace_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        configure_command_isolated_process_group(&mut cmd);
        let mut child = cmd.spawn().map_err(RustAnalyzerError::Spawn)?;

        tracing::info!(
            pid = ?child.id(),
            workspace = %workspace_root.display(),
            "starting rust-analyzer child process"
        );

        let child_pid = child.id();

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("missing child stdin")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("missing child stdout")))?;

        let (sender, receiver) = io_transport(stdin, stdout);
        let lsp = LspClient::new(sender, receiver);

        let folder_name = workspace_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".into());

        #[allow(deprecated)]
        // Mirrors prior JSON handshake; deprecated in favour of workspace_folders-only.
        let init_params = InitializeParams {
            process_id: None,
            root_uri: Some(root_uri.clone()),
            capabilities: racli_lsp_client_capabilities(),
            initialization_options: Some(serde_json::json!({
                "workspace": { "symbol": { "search": { "limit": symbol_search_limit } } }
            })),
            client_info: Some(ClientInfo {
                name: "racli".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: folder_name,
            }]),
            ..Default::default()
        };

        let init = match lsp.initialize(init_params).await {
            Ok(init) => init,
            Err(e) => return Err(startup_failure(&mut child, e).await),
        };
        let initialize_result = serde_json::to_value(&init)?;
        let lsp_server_info = lsp_server_info_from_server_info(init.server_info);

        // Subscribe before `initialized` so no early notification is dropped.
        let events = Arc::new(LspEvents::default());
        let mut event_tasks = Vec::with_capacity(FORWARDED_METHODS.len());
        for method in FORWARDED_METHODS {
            let mut subscription = lsp.subscribe_raw(method).await?;
            let events = Arc::clone(&events);
            event_tasks.push(tokio::spawn(async move {
                while let Some(params) = subscription.next().await {
                    match params {
                        Ok(params) => events.publish(method, &params),
                        Err(e) => tracing::warn!(method, error = %e, "malformed LSP notification"),
                    }
                }
            }));
        }

        lsp.initialized().await?;

        let session = RustAnalyzerSession {
            child,
            lsp: Some(lsp),
            child_pid,
            shutdown_complete: false,
            lsp_server_info,
            initialize_result,
            events,
            event_tasks,
        };

        tracing::info!(
            pid = ?session.child_pid,
            lsp_name = %session.lsp_server_info.name,
            lsp_version = %session.lsp_server_info.version,
            "rust-analyzer LSP initialized"
        );

        Ok(session)
    }

    /// Sends LSP `shutdown` and `exit` (best-effort with timeouts), waits for the process, and drops the LSP client.
    pub async fn shutdown_gracefully(mut self) -> Result<(), RustAnalyzerError> {
        self.shutdown_handshake().await
    }

    /// Core of graceful shutdown: LSP `shutdown` + `exit` (best-effort, timed out), then waits for
    /// the child to exit (killing it if it doesn't in time).
    async fn shutdown_handshake(&mut self) -> Result<(), RustAnalyzerError> {
        tracing::info!(
            pid = ?self.child_pid,
            "stopping rust-analyzer child process"
        );
        for task in self.event_tasks.drain(..) {
            task.abort();
        }

        // Keep the client alive until the child has exited: jsonrpsee only queues notifications, and
        // dropping the client stops its send task before the queued `exit` is written, so
        // rust-analyzer would see stdin close first and fail with "client exited without proper
        // shutdown sequence".
        let lsp = self.lsp.take();
        if let Some(lsp) = &lsp {
            match tokio::time::timeout(Duration::from_secs(8), lsp.shutdown()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(
                        pid = ?self.child_pid,
                        error = %e,
                        "LSP shutdown failed; continuing teardown"
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        pid = ?self.child_pid,
                        "LSP shutdown timed out; continuing teardown"
                    );
                }
            }

            if let Err(e) = lsp.exit().await {
                tracing::warn!(
                    pid = ?self.child_pid,
                    error = %e,
                    "LSP exit notification failed; continuing teardown"
                );
            }
        } else {
            tracing::warn!(
                pid = ?self.child_pid,
                "LSP client missing during shutdown handshake; tearing down child only"
            );
        }

        let wait = self.child.wait();
        match tokio::time::timeout(Duration::from_secs(10), wait).await {
            Ok(Ok(status)) => {
                tracing::info!(
                    pid = ?self.child_pid,
                    ?status,
                    "rust-analyzer child process exited"
                );
            }
            Ok(Err(e)) => return Err(RustAnalyzerError::Io(e)),
            Err(_) => {
                tracing::warn!(
                    pid = ?self.child_pid,
                    "rust-analyzer child did not exit in time; killing"
                );
                self.child.kill().await.map_err(RustAnalyzerError::Io)?;
            }
        }
        drop(lsp);

        self.shutdown_complete = true;
        Ok(())
    }

    /// Sends LSP `workspace/symbol` with the given query and kind/scope overrides and returns the JSON-RPC `result` (often an array or `null`).
    pub async fn workspace_symbol(
        &mut self,
        query: impl Into<String>,
        options: SymbolSearchOptions,
    ) -> Result<Value, RustAnalyzerError> {
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let result = lsp
            .send_request::<RaWorkspaceSymbolRequest>(RaWorkspaceSymbolParams::new(
                query.into(),
                options,
            ))
            .await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends LSP `textDocument/definition` for `document_uri` at `line` / `character` (0-based LSP position) and returns the JSON-RPC `result` (`null` or a location payload).
    pub async fn text_document_definition(
        &mut self,
        document_uri: impl Into<String>,
        line: u32,
        character: u32,
    ) -> Result<Value, RustAnalyzerError> {
        let uri_str = document_uri.into();
        let uri: Uri = uri_str
            .parse()
            .map_err(|_| RustAnalyzerError::InvalidDocumentUrl)?;
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let result = lsp.send_request::<GotoDefinition>(params).await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends LSP `textDocument/implementation` for `document_uri` at `line` / `character` (0-based LSP position) and returns the JSON-RPC `result` (`null` or a location payload); on a trait/type this resolves its `impl` blocks, on a trait method it resolves the per-`impl` overrides.
    pub async fn text_document_implementation(
        &mut self,
        document_uri: impl Into<String>,
        line: u32,
        character: u32,
    ) -> Result<Value, RustAnalyzerError> {
        let uri_str = document_uri.into();
        let uri: Uri = uri_str
            .parse()
            .map_err(|_| RustAnalyzerError::InvalidDocumentUrl)?;
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let params = GotoImplementationParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let result = lsp.send_request::<GotoImplementation>(params).await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends LSP `textDocument/references` for `document_uri` at `line` / `character` (0-based LSP position), always including the declaration, and returns the JSON-RPC `result` (`null` or a location array).
    pub async fn text_document_references(
        &mut self,
        document_uri: impl Into<String>,
        line: u32,
        character: u32,
    ) -> Result<Value, RustAnalyzerError> {
        let uri_str = document_uri.into();
        let uri: Uri = uri_str
            .parse()
            .map_err(|_| RustAnalyzerError::InvalidDocumentUrl)?;
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        };
        let result = lsp.send_request::<References>(params).await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends LSP `textDocument/prepareCallHierarchy` for `document_uri` at `line` / `character` (0-based LSP position) and returns the JSON-RPC `result` (`null` or a list of candidate items).
    pub async fn prepare_call_hierarchy(
        &mut self,
        document_uri: impl Into<String>,
        line: u32,
        character: u32,
    ) -> Result<Value, RustAnalyzerError> {
        let uri_str = document_uri.into();
        let uri: Uri = uri_str
            .parse()
            .map_err(|_| RustAnalyzerError::InvalidDocumentUrl)?;
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let params = CallHierarchyPrepareParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let result = lsp.send_request::<CallHierarchyPrepare>(params).await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends LSP `callHierarchy/incomingCalls` for `item` (the exact item from `prepare_call_hierarchy`) and returns the JSON-RPC `result` (`null` or a list of callers).
    pub async fn call_hierarchy_incoming_calls(
        &mut self,
        item: CallHierarchyItem,
    ) -> Result<Value, RustAnalyzerError> {
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let params = CallHierarchyIncomingCallsParams {
            item,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let result = lsp
            .send_request::<CallHierarchyIncomingCalls>(params)
            .await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends LSP `callHierarchy/outgoingCalls` for `item` (the exact item from `prepare_call_hierarchy`) and returns the JSON-RPC `result` (`null` or a list of callees).
    pub async fn call_hierarchy_outgoing_calls(
        &mut self,
        item: CallHierarchyItem,
    ) -> Result<Value, RustAnalyzerError> {
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let params = CallHierarchyOutgoingCallsParams {
            item,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let result = lsp
            .send_request::<CallHierarchyOutgoingCalls>(params)
            .await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends LSP `textDocument/documentSymbol` for `document_uri` and returns the JSON-RPC `result` (`null` or a hierarchical/flat symbol payload).
    pub async fn text_document_document_symbol(
        &mut self,
        document_uri: impl Into<String>,
    ) -> Result<Value, RustAnalyzerError> {
        let uri_str = document_uri.into();
        let uri: Uri = uri_str
            .parse()
            .map_err(|_| RustAnalyzerError::InvalidDocumentUrl)?;
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let result = lsp.send_request::<DocumentSymbolRequest>(params).await?;
        serde_json::to_value(result).map_err(RustAnalyzerError::from)
    }

    /// Sends an arbitrary LSP request and returns the raw JSON-RPC `result`.
    pub async fn raw_request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, RustAnalyzerError> {
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        Ok(lsp.request_raw(method, params).await?)
    }

    /// Sends an arbitrary LSP notification.
    pub async fn raw_notify(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), RustAnalyzerError> {
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        Ok(lsp.notify_raw(method, params).await?)
    }

    /// Sends LSP `workspace/didChangeWatchedFiles` so the server can refresh state for filesystem changes.
    pub async fn notify_did_change_watched_files(
        &mut self,
        params: DidChangeWatchedFilesParams,
    ) -> Result<(), RustAnalyzerError> {
        tracing::trace!(
            lsp_method = DidChangeWatchedFiles::METHOD,
            change_count = params.changes.len(),
            changes = ?params.changes,
            "sending workspace/didChangeWatchedFiles notification to rust-analyzer"
        );
        let lsp = self
            .lsp
            .as_ref()
            .ok_or_else(|| RustAnalyzerError::Io(io_other("LSP client missing")))?;
        lsp.send_notification::<DidChangeWatchedFiles>(params)
            .await?;
        Ok(())
    }
}

impl Drop for RustAnalyzerSession {
    fn drop(&mut self) {
        if !self.shutdown_complete {
            tracing::warn!(
                pid = ?self.child_pid,
                "rust-analyzer child session dropped without graceful shutdown"
            );
        }
        for task in &self.event_tasks {
            task.abort();
        }
        let _ = self.child.start_kill();
    }
}

/// Turns an `initialize` failure into [`RustAnalyzerError::ExitedDuringStartup`] when the child has
/// exited (the usual cause, e.g. a rustup shim with no rust-analyzer component); otherwise keeps `err`.
async fn startup_failure(child: &mut Child, err: crate::lsp_client::LspError) -> RustAnalyzerError {
    match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
        Ok(Ok(status)) => {
            tracing::debug!(error = %err, %status, "rust-analyzer exited during initialize");
            RustAnalyzerError::ExitedDuringStartup { status }
        }
        _ => err.into(),
    }
}

fn io_other(msg: &'static str) -> std::io::Error {
    std::io::Error::other(msg)
}

fn workspace_uri(root: &Path) -> Result<String, RustAnalyzerError> {
    Url::from_directory_path(root)
        .map(|u| u.to_string())
        .map_err(|()| RustAnalyzerError::InvalidWorkspaceUrl)
}

/// Builds an LSP `file://` document URI for an absolute regular file path.
pub fn document_uri_from_path(path: &Path) -> Result<String, RustAnalyzerError> {
    Url::from_file_path(path)
        .map(|u| u.to_string())
        .map_err(|()| RustAnalyzerError::InvalidDocumentUrl)
}

/// Maps LSP `InitializeResult.server_info` into [`LspServerInfo`].
fn lsp_server_info_from_server_info(server: Option<lsp_types::ServerInfo>) -> LspServerInfo {
    let Some(si) = server else {
        return LspServerInfo::default();
    };
    let version = si.version.unwrap_or_default();
    LspServerInfo {
        name: si.name,
        version,
    }
}

//! Shared backend for gRPC [`crate::grpc_server::RacliGrpc`].

use std::path::PathBuf;

use ractor::ActorRef;
use ractor::RactorErr;

use crate::actors::rust_analyzer::RustAnalyzerMsg;
use crate::proto::racli::DocumentSymbolsResponse;
use crate::proto::racli::FindDefinitionResponse;
use crate::proto::racli::FindImplementationsResponse;
use crate::proto::racli::FindReferencesResponse;
use crate::proto::racli::GetVersionResponse;
use crate::proto::racli::IncomingCallsResponse;
use crate::proto::racli::LspCallHierarchyItem;
use crate::proto::racli::LspServerInfo;
use crate::proto::racli::LspWorkspaceSymbolResponse;
use crate::proto::racli::OutgoingCallsResponse;
use crate::proto::racli::PrepareCallHierarchyResponse;
use crate::proto::racli::SearchResponse;
use crate::proto::racli::SymbolSearchKind as ProtoSymbolSearchKind;
use crate::proto::racli::SymbolSearchScope as ProtoSymbolSearchScope;
use crate::rust_analyzer::RustAnalyzerError;
use crate::rust_analyzer::SymbolSearchKind;
use crate::rust_analyzer::SymbolSearchOptions;
use crate::rust_analyzer::SymbolSearchScope;
use crate::server::Core;

/// Mirrors gRPC [`tonic::Status`] intent for callers that are not tonic-specific.
#[derive(Debug, thiserror::Error)]
pub enum RacliRpcError {
    /// Invalid RPC arguments (`INVALID_ARGUMENT`).
    #[error("{0}")]
    InvalidArgument(String),
    /// Unexpected server or LSP failure (`INTERNAL`).
    #[error("{0}")]
    Internal(String),
}

impl From<RustAnalyzerError> for RacliRpcError {
    fn from(value: RustAnalyzerError) -> Self {
        RacliRpcError::Internal(value.to_string())
    }
}

impl<T> From<RactorErr<T>> for RacliRpcError {
    fn from(value: RactorErr<T>) -> Self {
        RacliRpcError::Internal(format!("rust-analyzer actor unavailable: {value}"))
    }
}

/// Converts protobuf `SearchRequest.kind` / `scope` enum values into [`SymbolSearchOptions`] (unspecified becomes `None`).
pub fn symbol_search_options_from_proto(
    kind: i32,
    scope: i32,
) -> Result<SymbolSearchOptions, RacliRpcError> {
    let kind = match ProtoSymbolSearchKind::try_from(kind) {
        Ok(ProtoSymbolSearchKind::Unspecified) => None,
        Ok(ProtoSymbolSearchKind::OnlyTypes) => Some(SymbolSearchKind::OnlyTypes),
        Ok(ProtoSymbolSearchKind::AllSymbols) => Some(SymbolSearchKind::AllSymbols),
        Err(_) => {
            return Err(RacliRpcError::InvalidArgument(format!(
                "unknown symbol search kind: {kind}"
            )));
        }
    };
    let scope = match ProtoSymbolSearchScope::try_from(scope) {
        Ok(ProtoSymbolSearchScope::Unspecified) => None,
        Ok(ProtoSymbolSearchScope::Workspace) => Some(SymbolSearchScope::Workspace),
        Ok(ProtoSymbolSearchScope::WorkspaceAndDependencies) => {
            Some(SymbolSearchScope::WorkspaceAndDependencies)
        }
        Err(_) => {
            return Err(RacliRpcError::InvalidArgument(format!(
                "unknown symbol search scope: {scope}"
            )));
        }
    };
    Ok(SymbolSearchOptions { kind, scope })
}

/// Shared [`Core`] plus a handle to the rust-analyzer actor that owns the live LSP session.
pub struct RacliSession {
    core: Core,
    lsp_server_info: LspServerInfo,
    rust_analyzer: ActorRef<RustAnalyzerMsg>,
}

impl RacliSession {
    /// Builds a session from an initialized rust-analyzer actor and its LSP `serverInfo`.
    pub(crate) fn new(
        core: Core,
        lsp_server_info: LspServerInfo,
        rust_analyzer: ActorRef<RustAnalyzerMsg>,
    ) -> Self {
        Self {
            core,
            lsp_server_info,
            rust_analyzer,
        }
    }

    /// Returns protobuf [`GetVersionResponse`] (`Racli.GetVersion`).
    pub fn get_version(&self) -> GetVersionResponse {
        let version = self.core.version();
        let lsp_server_info = self.lsp_server_info.clone();
        GetVersionResponse {
            version,
            lsp_server_info: Some(lsp_server_info),
        }
    }

    /// Runs workspace symbol search (`Racli.Search`) with optional kind/scope overrides.
    pub async fn search(
        &self,
        query: String,
        options: SymbolSearchOptions,
    ) -> Result<SearchResponse, RacliRpcError> {
        let value = ractor::call!(self.rust_analyzer, |reply| RustAnalyzerMsg::Search {
            query,
            options,
            reply,
        })??;

        let ws: LspWorkspaceSymbolResponse = if value.is_null() {
            LspWorkspaceSymbolResponse { payload: None }
        } else {
            let lsp_resp: lsp_types::WorkspaceSymbolResponse = serde_json::from_value(value)
                .map_err(|e| RacliRpcError::Internal(e.to_string()))?;
            crate::lsp_map::workspace_symbol_response_to_proto(lsp_resp)
        };

        Ok(SearchResponse {
            workspace_symbol_response: Some(ws),
        })
    }

    /// Resolves definitions at `file_path` + LSP position (`Racli.FindDefinition`).
    pub async fn find_definition(
        &self,
        file_path: String,
        line: u32,
        character: u32,
    ) -> Result<FindDefinitionResponse, RacliRpcError> {
        let path = PathBuf::from(file_path.trim());
        if path.as_os_str().is_empty() {
            return Err(RacliRpcError::InvalidArgument(
                "file_path must not be empty".into(),
            ));
        }
        let abs = std::fs::canonicalize(&path).map_err(|e| {
            RacliRpcError::InvalidArgument(format!("cannot resolve file path: {e}"))
        })?;
        let uri = crate::rust_analyzer::document_uri_from_path(&abs)
            .map_err(|e| RacliRpcError::InvalidArgument(e.to_string()))?;

        let value = ractor::call!(self.rust_analyzer, |reply| {
            RustAnalyzerMsg::FindDefinition {
                uri,
                line,
                character,
                reply,
            }
        })??;

        let locations = if value.is_null() {
            vec![]
        } else {
            let resp: lsp_types::GotoDefinitionResponse = serde_json::from_value(value)
                .map_err(|e| RacliRpcError::Internal(e.to_string()))?;
            crate::lsp_map::goto_definition_response_to_locations(resp)
        };

        Ok(FindDefinitionResponse { locations })
    }

    /// Resolves trait/type implementations (or per-`impl` method overrides) at `file_path` + LSP position (`Racli.FindImplementations`).
    pub async fn find_implementations(
        &self,
        file_path: String,
        line: u32,
        character: u32,
    ) -> Result<FindImplementationsResponse, RacliRpcError> {
        let path = PathBuf::from(file_path.trim());
        if path.as_os_str().is_empty() {
            return Err(RacliRpcError::InvalidArgument(
                "file_path must not be empty".into(),
            ));
        }
        let abs = std::fs::canonicalize(&path).map_err(|e| {
            RacliRpcError::InvalidArgument(format!("cannot resolve file path: {e}"))
        })?;
        let uri = crate::rust_analyzer::document_uri_from_path(&abs)
            .map_err(|e| RacliRpcError::InvalidArgument(e.to_string()))?;

        let value = ractor::call!(self.rust_analyzer, |reply| {
            RustAnalyzerMsg::FindImplementations {
                uri,
                line,
                character,
                reply,
            }
        })??;

        let locations = if value.is_null() {
            vec![]
        } else {
            let resp: lsp_types::GotoDefinitionResponse = serde_json::from_value(value)
                .map_err(|e| RacliRpcError::Internal(e.to_string()))?;
            crate::lsp_map::goto_definition_response_to_locations(resp)
        };

        Ok(FindImplementationsResponse { locations })
    }

    /// Resolves references (including the declaration) at `file_path` + LSP position (`Racli.FindReferences`).
    pub async fn find_references(
        &self,
        file_path: String,
        line: u32,
        character: u32,
    ) -> Result<FindReferencesResponse, RacliRpcError> {
        let path = PathBuf::from(file_path.trim());
        if path.as_os_str().is_empty() {
            return Err(RacliRpcError::InvalidArgument(
                "file_path must not be empty".into(),
            ));
        }
        let abs = std::fs::canonicalize(&path).map_err(|e| {
            RacliRpcError::InvalidArgument(format!("cannot resolve file path: {e}"))
        })?;
        let uri = crate::rust_analyzer::document_uri_from_path(&abs)
            .map_err(|e| RacliRpcError::InvalidArgument(e.to_string()))?;

        let value = ractor::call!(self.rust_analyzer, |reply| {
            RustAnalyzerMsg::FindReferences {
                uri,
                line,
                character,
                reply,
            }
        })??;

        let locations = if value.is_null() {
            vec![]
        } else {
            let resp: Option<Vec<lsp_types::Location>> = serde_json::from_value(value)
                .map_err(|e| RacliRpcError::Internal(e.to_string()))?;
            crate::lsp_map::references_to_locations(resp.unwrap_or_default())
        };

        Ok(FindReferencesResponse { locations })
    }

    /// Resolves call hierarchy candidates at `file_path` + LSP position (`Racli.PrepareCallHierarchy`).
    pub async fn prepare_call_hierarchy(
        &self,
        file_path: String,
        line: u32,
        character: u32,
    ) -> Result<PrepareCallHierarchyResponse, RacliRpcError> {
        let path = PathBuf::from(file_path.trim());
        if path.as_os_str().is_empty() {
            return Err(RacliRpcError::InvalidArgument(
                "file_path must not be empty".into(),
            ));
        }
        let abs = std::fs::canonicalize(&path).map_err(|e| {
            RacliRpcError::InvalidArgument(format!("cannot resolve file path: {e}"))
        })?;
        let uri = crate::rust_analyzer::document_uri_from_path(&abs)
            .map_err(|e| RacliRpcError::InvalidArgument(e.to_string()))?;

        let value = ractor::call!(self.rust_analyzer, |reply| {
            RustAnalyzerMsg::PrepareCallHierarchy {
                uri,
                line,
                character,
                reply,
            }
        })??;

        let items = if value.is_null() {
            vec![]
        } else {
            let resp: Option<Vec<lsp_types::CallHierarchyItem>> = serde_json::from_value(value)
                .map_err(|e| RacliRpcError::Internal(e.to_string()))?;
            resp.unwrap_or_default()
                .into_iter()
                .map(crate::lsp_map::call_hierarchy_item_to_proto)
                .collect()
        };

        Ok(PrepareCallHierarchyResponse { items })
    }

    /// Resolves callers of `item` (`Racli.IncomingCalls`); `item` must be the exact item returned by `prepare_call_hierarchy`.
    pub async fn incoming_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<IncomingCallsResponse, RacliRpcError> {
        let lsp_item = crate::lsp_map::call_hierarchy_item_from_proto(&item)
            .map_err(RacliRpcError::InvalidArgument)?;

        let value = ractor::call!(self.rust_analyzer, |reply| RustAnalyzerMsg::IncomingCalls {
            item: lsp_item,
            reply,
        })??;

        let calls = if value.is_null() {
            vec![]
        } else {
            let resp: Option<Vec<lsp_types::CallHierarchyIncomingCall>> =
                serde_json::from_value(value)
                    .map_err(|e| RacliRpcError::Internal(e.to_string()))?;
            resp.unwrap_or_default()
                .into_iter()
                .map(crate::lsp_map::call_hierarchy_incoming_call_to_proto)
                .collect()
        };

        Ok(IncomingCallsResponse { calls })
    }

    /// Resolves callees of `item` (`Racli.OutgoingCalls`); `item` must be the exact item returned by `prepare_call_hierarchy`.
    pub async fn outgoing_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<OutgoingCallsResponse, RacliRpcError> {
        let lsp_item = crate::lsp_map::call_hierarchy_item_from_proto(&item)
            .map_err(RacliRpcError::InvalidArgument)?;

        let value = ractor::call!(self.rust_analyzer, |reply| RustAnalyzerMsg::OutgoingCalls {
            item: lsp_item,
            reply,
        })??;

        let calls = if value.is_null() {
            vec![]
        } else {
            let resp: Option<Vec<lsp_types::CallHierarchyOutgoingCall>> =
                serde_json::from_value(value)
                    .map_err(|e| RacliRpcError::Internal(e.to_string()))?;
            resp.unwrap_or_default()
                .into_iter()
                .map(crate::lsp_map::call_hierarchy_outgoing_call_to_proto)
                .collect()
        };

        Ok(OutgoingCallsResponse { calls })
    }

    /// Runs LSP `textDocument/documentSymbol` for `file_path` (`Racli.DocumentSymbols`); file-scoped, no line/character.
    pub async fn document_symbols(
        &self,
        file_path: String,
    ) -> Result<DocumentSymbolsResponse, RacliRpcError> {
        let path = PathBuf::from(file_path.trim());
        if path.as_os_str().is_empty() {
            return Err(RacliRpcError::InvalidArgument(
                "file_path must not be empty".into(),
            ));
        }
        let abs = std::fs::canonicalize(&path).map_err(|e| {
            RacliRpcError::InvalidArgument(format!("cannot resolve file path: {e}"))
        })?;
        let uri = crate::rust_analyzer::document_uri_from_path(&abs)
            .map_err(|e| RacliRpcError::InvalidArgument(e.to_string()))?;

        let value = ractor::call!(self.rust_analyzer, |reply| {
            RustAnalyzerMsg::DocumentSymbols { uri, reply }
        })??;

        let symbols = if value.is_null() {
            vec![]
        } else {
            let resp: lsp_types::DocumentSymbolResponse = serde_json::from_value(value)
                .map_err(|e| RacliRpcError::Internal(e.to_string()))?;
            crate::lsp_map::document_symbol_response_to_symbols(resp)
        };

        Ok(DocumentSymbolsResponse { symbols })
    }
}

//! Shared backend for gRPC [`crate::grpc_server::RacliGrpc`].

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::proto::racli::FindDefinitionResponse;
use crate::proto::racli::FindReferencesResponse;
use crate::proto::racli::GetVersionResponse;
use crate::proto::racli::IncomingCallsResponse;
use crate::proto::racli::LspCallHierarchyItem;
use crate::proto::racli::LspServerInfo;
use crate::proto::racli::LspWorkspaceSymbolResponse;
use crate::proto::racli::OutgoingCallsResponse;
use crate::proto::racli::PrepareCallHierarchyResponse;
use crate::proto::racli::SearchResponse;
use crate::rust_analyzer::RustAnalyzerError;
use crate::rust_analyzer::RustAnalyzerSession;
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

/// Shared [`Core`] plus a live rust-analyzer LSP session (`Arc<Mutex<RustAnalyzerSession>>`).
pub struct RacliSession {
    core: Core,
    lsp_server_info: LspServerInfo,
    rust_analyzer: Arc<Mutex<RustAnalyzerSession>>,
}

impl RacliSession {
    /// Builds a session using an already-running LSP handshake and shared `Arc` to the analyzer mutex.
    pub fn new(
        core: Core,
        lsp_server_info: LspServerInfo,
        rust_analyzer: Arc<Mutex<RustAnalyzerSession>>,
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

    /// Runs workspace symbol search (`Racli.Search`).
    pub async fn search(&self, query: String) -> Result<SearchResponse, RacliRpcError> {
        let mut ra = self.rust_analyzer.lock().await;
        let value = self.core.search(&mut ra, query).await?;
        drop(ra);

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

        let mut ra = self.rust_analyzer.lock().await;
        let value = self
            .core
            .find_definition(&mut ra, uri, line, character)
            .await?;
        drop(ra);

        let locations = if value.is_null() {
            vec![]
        } else {
            let resp: lsp_types::GotoDefinitionResponse = serde_json::from_value(value)
                .map_err(|e| RacliRpcError::Internal(e.to_string()))?;
            crate::lsp_map::goto_definition_response_to_locations(resp)
        };

        Ok(FindDefinitionResponse { locations })
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

        let mut ra = self.rust_analyzer.lock().await;
        let value = self
            .core
            .find_references(&mut ra, uri, line, character)
            .await?;
        drop(ra);

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

        let mut ra = self.rust_analyzer.lock().await;
        let value = self
            .core
            .prepare_call_hierarchy(&mut ra, uri, line, character)
            .await?;
        drop(ra);

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

        let mut ra = self.rust_analyzer.lock().await;
        let value = self
            .core
            .call_hierarchy_incoming_calls(&mut ra, lsp_item)
            .await?;
        drop(ra);

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

        let mut ra = self.rust_analyzer.lock().await;
        let value = self
            .core
            .call_hierarchy_outgoing_calls(&mut ra, lsp_item)
            .await?;
        drop(ra);

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
}

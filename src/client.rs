//! gRPC client for `racli server` over a Unix socket (`GetVersion`, `Search`, `FindDefinition`, `FindReferences`, `FindImplementations`, `PrepareCallHierarchy`, `IncomingCalls`, `OutgoingCalls`, `DocumentSymbols`, `Hover`). It wraps only racli's typed RPCs, not the LSP passthrough used by `racli tee` (see ADR 003).

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use tonic::transport::Channel;
use tonic::transport::Endpoint;

use crate::backend_status::BackendStatus;
use crate::proto::racli::CallHierarchyCallsRequest;
use crate::proto::racli::DocumentSymbolsRequest;
use crate::proto::racli::DocumentSymbolsResponse;
use crate::proto::racli::FindDefinitionRequest;
use crate::proto::racli::FindDefinitionResponse;
use crate::proto::racli::FindImplementationsRequest;
use crate::proto::racli::FindImplementationsResponse;
use crate::proto::racli::FindReferencesRequest;
use crate::proto::racli::FindReferencesResponse;
use crate::proto::racli::GetVersionRequest;
use crate::proto::racli::GetVersionResponse;
use crate::proto::racli::HoverRequest;
use crate::proto::racli::HoverResponse;
use crate::proto::racli::IncomingCallsResponse;
use crate::proto::racli::LspCallHierarchyItem;
use crate::proto::racli::OutgoingCallsResponse;
use crate::proto::racli::PrepareCallHierarchyRequest;
use crate::proto::racli::PrepareCallHierarchyResponse;
use crate::proto::racli::SearchRequest;
use crate::proto::racli::SearchResponse;
use crate::proto::racli::SymbolSearchKind as ProtoSymbolSearchKind;
use crate::proto::racli::SymbolSearchScope as ProtoSymbolSearchScope;
use crate::proto::racli::racli_client::RacliClient;
use crate::rust_analyzer::SymbolSearchKind;
use crate::rust_analyzer::SymbolSearchOptions;
use crate::rust_analyzer::SymbolSearchScope;
use crate::utils::derive_socket_path_enabled;
use crate::utils::derived_unix_socket_path;
use crate::utils::effective_unix_socket_path;
use crate::utils::explicit_socket_path_set;
use crate::utils::find_live_socket;
use crate::utils::socket_is_live;

/// Timeout for connecting to the server's socket.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for each request (LSP requests such as `workspace/symbol` can be slow).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Timeout for `GetVersion`, which the server answers without asking rust-analyzer.
const GET_VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// Socket for client commands: `RACLI_UNIX_SOCKET` if set, [`DEFAULT_UNIX_SOCKET_PATH`](crate::utils::DEFAULT_UNIX_SOCKET_PATH) if
/// deriving is off, else the live derived socket of the cwd or its nearest ancestor (so clients work
/// from any subdirectory of a served project). With no live server, warns and returns the cwd's derived path.
pub fn client_unix_socket_path() -> PathBuf {
    if explicit_socket_path_set() || !derive_socket_path_enabled() {
        return effective_unix_socket_path();
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => std::fs::canonicalize(&cwd).unwrap_or(cwd),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read current directory; using the default socket");
            return effective_unix_socket_path();
        }
    };
    find_live_socket(&cwd, socket_is_live).unwrap_or_else(|| {
        let socket = derived_unix_socket_path(&cwd);
        tracing::warn!(
            cwd = %cwd.display(),
            socket = %socket.display(),
            "no running racli server found for this directory or its parents; start `racli server` \
             or `racli tee` in the project root, or set RACLI_UNIX_SOCKET"
        );
        socket
    })
}

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// A response's value with the backend's status when the server produced it (ADR 004).
#[derive(Clone, Debug)]
pub struct Reply<T> {
    /// The RPC's response message.
    pub value: T,
    /// Whether rust-analyzer had finished loading and indexing when the server answered.
    pub backend: BackendStatus,
}

impl<T> Reply<T> {
    /// Takes the value and the backend's status from a gRPC response.
    fn from_response(response: tonic::Response<T>) -> Self {
        let backend = BackendStatus::from_metadata(response.metadata());
        Self {
            value: response.into_inner(),
            backend,
        }
    }
}

/// A client for the `racli server` on a Unix socket; its calls share one connection, opened on the first call.
#[derive(Clone, Debug)]
pub struct Client {
    inner: RacliClient<Channel>,
}

impl Client {
    /// A client for the server at `socket_path`, with a 10s connect and 60s per-request timeout; nothing connects until the first call.
    pub fn new(socket_path: &Path) -> Result<Self, ClientError> {
        let channel = Endpoint::try_from(format!("unix://{}", socket_path.display()))?
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .connect_lazy();
        Ok(Self {
            inner: RacliClient::new(channel),
        })
    }

    /// Calls `GetVersion`, with a 10s timeout.
    pub async fn get_version(&self) -> Result<Reply<GetVersionResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = tokio::time::timeout(
            GET_VERSION_TIMEOUT,
            client.get_version(GetVersionRequest {}),
        )
        .await
        .map_err(|_| tonic::Status::deadline_exceeded("GetVersion timed out after 10 seconds"))??;
        Ok(Reply::from_response(resp))
    }

    /// Calls `Search` with the server's default symbol kind and scope (see [`Client::search_with_options`]).
    pub async fn search(
        &self,
        query: impl AsRef<str>,
    ) -> Result<Reply<SearchResponse>, ClientError> {
        self.search_with_options(query, SymbolSearchOptions::default())
            .await
    }

    /// Calls `Search` with kind/scope overrides.
    pub async fn search_with_options(
        &self,
        query: impl AsRef<str>,
        options: SymbolSearchOptions,
    ) -> Result<Reply<SearchResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = client
            .search(SearchRequest {
                query: query.as_ref().to_string(),
                kind: options
                    .kind
                    .map_or(ProtoSymbolSearchKind::Unspecified, |k| match k {
                        SymbolSearchKind::OnlyTypes => ProtoSymbolSearchKind::OnlyTypes,
                        SymbolSearchKind::AllSymbols => ProtoSymbolSearchKind::AllSymbols,
                    }) as i32,
                scope: options
                    .scope
                    .map_or(ProtoSymbolSearchScope::Unspecified, |s| match s {
                        SymbolSearchScope::Workspace => ProtoSymbolSearchScope::Workspace,
                        SymbolSearchScope::WorkspaceAndDependencies => {
                            ProtoSymbolSearchScope::WorkspaceAndDependencies
                        }
                    }) as i32,
            })
            .await?;
        Ok(Reply::from_response(resp))
    }

    /// Calls `FindDefinition` at a 0-based position in `file_path`.
    pub async fn find_definition(
        &self,
        file_path: impl AsRef<str>,
        line: u32,
        character: u32,
    ) -> Result<Reply<FindDefinitionResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = client
            .find_definition(FindDefinitionRequest {
                file_path: file_path.as_ref().to_string(),
                line,
                character,
            })
            .await?;
        Ok(Reply::from_response(resp))
    }

    /// Calls `FindImplementations` at a 0-based position in `file_path`.
    pub async fn find_implementations(
        &self,
        file_path: impl AsRef<str>,
        line: u32,
        character: u32,
    ) -> Result<Reply<FindImplementationsResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = client
            .find_implementations(FindImplementationsRequest {
                file_path: file_path.as_ref().to_string(),
                line,
                character,
            })
            .await?;
        Ok(Reply::from_response(resp))
    }

    /// Calls `FindReferences` at a 0-based position in `file_path`.
    pub async fn find_references(
        &self,
        file_path: impl AsRef<str>,
        line: u32,
        character: u32,
    ) -> Result<Reply<FindReferencesResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = client
            .find_references(FindReferencesRequest {
                file_path: file_path.as_ref().to_string(),
                line,
                character,
            })
            .await?;
        Ok(Reply::from_response(resp))
    }

    /// Calls `PrepareCallHierarchy` at a 0-based position in `file_path`.
    pub async fn prepare_call_hierarchy(
        &self,
        file_path: impl AsRef<str>,
        line: u32,
        character: u32,
    ) -> Result<Reply<PrepareCallHierarchyResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = client
            .prepare_call_hierarchy(PrepareCallHierarchyRequest {
                file_path: file_path.as_ref().to_string(),
                line,
                character,
            })
            .await?;
        Ok(Reply::from_response(resp))
    }

    /// Calls `DocumentSymbols` for `file_path`.
    pub async fn document_symbols(
        &self,
        file_path: impl AsRef<str>,
    ) -> Result<Reply<DocumentSymbolsResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = client
            .document_symbols(DocumentSymbolsRequest {
                file_path: file_path.as_ref().to_string(),
            })
            .await?;
        Ok(Reply::from_response(resp))
    }

    /// Calls `IncomingCalls` for `item`, which must be the exact item returned by [`Client::prepare_call_hierarchy`] (or a prior `IncomingCalls`/`OutgoingCalls` call).
    pub async fn incoming_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Reply<IncomingCallsResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = client
            .incoming_calls(CallHierarchyCallsRequest { item: Some(item) })
            .await?;
        Ok(Reply::from_response(resp))
    }

    /// Calls `OutgoingCalls` for `item`, which must be the exact item returned by [`Client::prepare_call_hierarchy`] (or a prior `IncomingCalls`/`OutgoingCalls` call).
    pub async fn outgoing_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Reply<OutgoingCallsResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = client
            .outgoing_calls(CallHierarchyCallsRequest { item: Some(item) })
            .await?;
        Ok(Reply::from_response(resp))
    }

    /// Calls `Hover` at a 0-based position in `file_path`.
    pub async fn hover(
        &self,
        file_path: impl AsRef<str>,
        line: u32,
        character: u32,
    ) -> Result<Reply<HoverResponse>, ClientError> {
        let mut client = self.inner.clone();
        let resp = client
            .hover(HoverRequest {
                file_path: file_path.as_ref().to_string(),
                line,
                character,
            })
            .await?;
        Ok(Reply::from_response(resp))
    }
}

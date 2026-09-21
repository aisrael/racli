//! gRPC client for `racli server` over a Unix socket (`GetVersion`, `Search`, `FindDefinition`, `FindReferences`, `FindImplementations`, `PrepareCallHierarchy`, `IncomingCalls`, `OutgoingCalls`, `DocumentSymbols`).

use std::path::Path;
use std::time::Duration;

use tonic::transport::Endpoint;

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

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `GetVersion`.
#[derive(Debug, thiserror::Error)]
pub enum ClientVersionError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `Search`.
#[derive(Debug, thiserror::Error)]
pub enum ClientSearchError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `FindDefinition`.
#[derive(Debug, thiserror::Error)]
pub enum ClientFindDefinitionError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `FindImplementations`.
#[derive(Debug, thiserror::Error)]
pub enum ClientFindImplementationsError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `FindReferences`.
#[derive(Debug, thiserror::Error)]
pub enum ClientFindReferencesError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `PrepareCallHierarchy`.
#[derive(Debug, thiserror::Error)]
pub enum ClientPrepareCallHierarchyError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `IncomingCalls`.
#[derive(Debug, thiserror::Error)]
pub enum ClientIncomingCallsError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `OutgoingCalls`.
#[derive(Debug, thiserror::Error)]
pub enum ClientOutgoingCallsError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Calls `GetVersion` on the server at `socket_path` with 10s connect and request timeouts.
pub async fn get_version(socket_path: &Path) -> Result<GetVersionResponse, ClientVersionError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
    let resp = client.get_version(GetVersionRequest {}).await?;

    Ok(resp.into_inner())
}

/// Calls `Search` on the server at `socket_path` with the server's default symbol kind and scope (see [`search_with_options`]).
pub async fn search(
    socket_path: &Path,
    query: impl AsRef<str>,
) -> Result<SearchResponse, ClientSearchError> {
    search_with_options(socket_path, query, SymbolSearchOptions::default()).await
}

/// Calls `Search` on the server at `socket_path` with kind/scope overrides, 10s connect and 60s per-request timeout (LSP `workspace/symbol` can be slow).
pub async fn search_with_options(
    socket_path: &Path,
    query: impl AsRef<str>,
    options: SymbolSearchOptions,
) -> Result<SearchResponse, ClientSearchError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
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

    Ok(resp.into_inner())
}

/// Calls `FindDefinition` on the server at `socket_path` with 10s connect and 60s per-request timeout.
pub async fn find_definition(
    socket_path: &Path,
    file_path: impl AsRef<str>,
    line: u32,
    character: u32,
) -> Result<FindDefinitionResponse, ClientFindDefinitionError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
    let resp = client
        .find_definition(FindDefinitionRequest {
            file_path: file_path.as_ref().to_string(),
            line,
            character,
        })
        .await?;

    Ok(resp.into_inner())
}

/// Calls `FindImplementations` on the server at `socket_path` with 10s connect and 60s per-request timeout.
pub async fn find_implementations(
    socket_path: &Path,
    file_path: impl AsRef<str>,
    line: u32,
    character: u32,
) -> Result<FindImplementationsResponse, ClientFindImplementationsError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
    let resp = client
        .find_implementations(FindImplementationsRequest {
            file_path: file_path.as_ref().to_string(),
            line,
            character,
        })
        .await?;

    Ok(resp.into_inner())
}

/// Failures building the endpoint, connecting, or interpreting a non-OK gRPC status for `DocumentSymbols`.
#[derive(Debug, thiserror::Error)]
pub enum ClientDocumentSymbolsError {
    /// Failed to build the channel endpoint or connect over the Unix URI.
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    /// gRPC call completed with a non-OK status from the server.
    #[error(transparent)]
    Status(#[from] tonic::Status),
}

/// Calls `FindReferences` on the server at `socket_path` with 10s connect and 60s per-request timeout.
pub async fn find_references(
    socket_path: &Path,
    file_path: impl AsRef<str>,
    line: u32,
    character: u32,
) -> Result<FindReferencesResponse, ClientFindReferencesError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
    let resp = client
        .find_references(FindReferencesRequest {
            file_path: file_path.as_ref().to_string(),
            line,
            character,
        })
        .await?;

    Ok(resp.into_inner())
}

/// Calls `PrepareCallHierarchy` on the server at `socket_path` with 10s connect and 60s per-request timeout.
pub async fn prepare_call_hierarchy(
    socket_path: &Path,
    file_path: impl AsRef<str>,
    line: u32,
    character: u32,
) -> Result<PrepareCallHierarchyResponse, ClientPrepareCallHierarchyError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
    let resp = client
        .prepare_call_hierarchy(PrepareCallHierarchyRequest {
            file_path: file_path.as_ref().to_string(),
            line,
            character,
        })
        .await?;

    Ok(resp.into_inner())
}

/// Calls `DocumentSymbols` on the server at `socket_path` with 10s connect and 60s per-request timeout.
pub async fn document_symbols(
    socket_path: &Path,
    file_path: impl AsRef<str>,
) -> Result<DocumentSymbolsResponse, ClientDocumentSymbolsError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
    let resp = client
        .document_symbols(DocumentSymbolsRequest {
            file_path: file_path.as_ref().to_string(),
        })
        .await?;

    Ok(resp.into_inner())
}

/// Calls `IncomingCalls` on the server at `socket_path` with 10s connect and 60s per-request timeout.
///
/// `item` must be the exact item returned by [`prepare_call_hierarchy`] (or a prior `IncomingCalls`/`OutgoingCalls` call).
pub async fn incoming_calls(
    socket_path: &Path,
    item: LspCallHierarchyItem,
) -> Result<IncomingCallsResponse, ClientIncomingCallsError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
    let resp = client
        .incoming_calls(CallHierarchyCallsRequest { item: Some(item) })
        .await?;

    Ok(resp.into_inner())
}

/// Calls `OutgoingCalls` on the server at `socket_path` with 10s connect and 60s per-request timeout.
///
/// `item` must be the exact item returned by [`prepare_call_hierarchy`] (or a prior `IncomingCalls`/`OutgoingCalls` call).
pub async fn outgoing_calls(
    socket_path: &Path,
    item: LspCallHierarchyItem,
) -> Result<OutgoingCallsResponse, ClientOutgoingCallsError> {
    let ep = Endpoint::try_from(format!("unix://{}", socket_path.display()))?;

    let channel = ep
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .connect()
        .await?;

    let mut client = RacliClient::new(channel);
    let resp = client
        .outgoing_calls(CallHierarchyCallsRequest { item: Some(item) })
        .await?;

    Ok(resp.into_inner())
}

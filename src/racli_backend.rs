//! The racli backend's operations, over a gRPC [`Client`] (CLI) or an in-process [`RacliCore`]
//! (MCP), so the client-side lookups run the same way over both (ADR 006).

use crate::client::Client;
use crate::proto::racli::DocumentSymbolsResponse;
use crate::proto::racli::FindDefinitionResponse;
use crate::proto::racli::FindImplementationsResponse;
use crate::proto::racli::FindReferencesResponse;
use crate::proto::racli::HoverResponse;
use crate::proto::racli::LspCallHierarchyIncomingCall;
use crate::proto::racli::LspCallHierarchyItem;
use crate::proto::racli::LspCallHierarchyOutgoingCall;
use crate::proto::racli::SearchResponse;
use crate::racli_core::RacliCore;
use crate::rust_analyzer::SymbolSearchOptions;

/// The racli operations the client commands use. Positions are LSP's, 0-based.
// `#[async_trait]`'s expansion trips `double_must_use` (Rust 1.99+).
#[allow(clippy::double_must_use)]
#[tonic::async_trait]
pub trait RacliBackend: Send + Sync {
    /// Runs `workspace/symbol` for `query` (`Racli.Search`).
    async fn search(
        &self,
        query: &str,
        options: SymbolSearchOptions,
    ) -> Result<SearchResponse, String>;
    /// Runs `textDocument/definition` at a position in `file_path` (`Racli.FindDefinition`).
    async fn find_definition(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindDefinitionResponse, String>;
    /// Runs `textDocument/references`, declaration included, at a position in `file_path`
    /// (`Racli.FindReferences`).
    async fn find_references(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindReferencesResponse, String>;
    /// Runs `textDocument/implementation` at a position in `file_path` (`Racli.FindImplementations`).
    async fn find_implementations(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindImplementationsResponse, String>;
    /// Runs `textDocument/documentSymbol` for `file_path` (`Racli.DocumentSymbols`).
    async fn document_symbols(&self, file_path: &str) -> Result<DocumentSymbolsResponse, String>;
    /// Runs `textDocument/hover` at a position in `file_path` (`Racli.Hover`).
    async fn hover(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<HoverResponse, String>;
    /// Runs `textDocument/prepareCallHierarchy` at a position in `file_path`.
    async fn prepare_call_hierarchy(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspCallHierarchyItem>, String>;
    /// Runs `callHierarchy/incomingCalls` for `item` (must be the exact item from `prepare_call_hierarchy`).
    async fn incoming_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyIncomingCall>, String>;
    /// Runs `callHierarchy/outgoingCalls` for `item` (must be the exact item from `prepare_call_hierarchy`).
    async fn outgoing_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyOutgoingCall>, String>;
}

/// [`RacliBackend`] over a `racli server` or `racli tee` socket, used by the CLI.
#[tonic::async_trait]
impl RacliBackend for Client {
    async fn search(
        &self,
        query: &str,
        options: SymbolSearchOptions,
    ) -> Result<SearchResponse, String> {
        self.search_with_options(query, options)
            .await
            .map(|reply| reply.value)
            .map_err(|e| e.to_string())
    }

    async fn find_definition(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindDefinitionResponse, String> {
        Client::find_definition(self, file_path, line, character)
            .await
            .map(|reply| reply.value)
            .map_err(|e| e.to_string())
    }

    async fn find_references(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindReferencesResponse, String> {
        Client::find_references(self, file_path, line, character)
            .await
            .map(|reply| reply.value)
            .map_err(|e| e.to_string())
    }

    async fn find_implementations(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindImplementationsResponse, String> {
        Client::find_implementations(self, file_path, line, character)
            .await
            .map(|reply| reply.value)
            .map_err(|e| e.to_string())
    }

    async fn document_symbols(&self, file_path: &str) -> Result<DocumentSymbolsResponse, String> {
        Client::document_symbols(self, file_path)
            .await
            .map(|reply| reply.value)
            .map_err(|e| e.to_string())
    }

    async fn hover(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<HoverResponse, String> {
        Client::hover(self, file_path, line, character)
            .await
            .map(|reply| reply.value)
            .map_err(|e| e.to_string())
    }

    async fn prepare_call_hierarchy(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspCallHierarchyItem>, String> {
        Client::prepare_call_hierarchy(self, file_path.to_string(), line, character)
            .await
            .map(|reply| reply.value.items)
            .map_err(|e| e.to_string())
    }

    async fn incoming_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyIncomingCall>, String> {
        Client::incoming_calls(self, item)
            .await
            .map(|reply| reply.value.calls)
            .map_err(|e| e.to_string())
    }

    async fn outgoing_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyOutgoingCall>, String> {
        Client::outgoing_calls(self, item)
            .await
            .map(|reply| reply.value.calls)
            .map_err(|e| e.to_string())
    }
}

/// [`RacliBackend`] over an in-process [`RacliCore`], used by the MCP tools.
#[tonic::async_trait]
impl RacliBackend for RacliCore {
    async fn search(
        &self,
        query: &str,
        options: SymbolSearchOptions,
    ) -> Result<SearchResponse, String> {
        RacliCore::search(self, query.to_string(), options)
            .await
            .map_err(|e| e.to_string())
    }

    async fn find_definition(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindDefinitionResponse, String> {
        RacliCore::find_definition(self, file_path.to_string(), line, character)
            .await
            .map_err(|e| e.to_string())
    }

    async fn find_references(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindReferencesResponse, String> {
        RacliCore::find_references(self, file_path.to_string(), line, character)
            .await
            .map_err(|e| e.to_string())
    }

    async fn find_implementations(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindImplementationsResponse, String> {
        RacliCore::find_implementations(self, file_path.to_string(), line, character)
            .await
            .map_err(|e| e.to_string())
    }

    async fn document_symbols(&self, file_path: &str) -> Result<DocumentSymbolsResponse, String> {
        RacliCore::document_symbols(self, file_path.to_string())
            .await
            .map_err(|e| e.to_string())
    }

    async fn hover(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<HoverResponse, String> {
        RacliCore::hover(self, file_path.to_string(), line, character)
            .await
            .map_err(|e| e.to_string())
    }

    async fn prepare_call_hierarchy(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspCallHierarchyItem>, String> {
        RacliCore::prepare_call_hierarchy(self, file_path.to_string(), line, character)
            .await
            .map(|reply| reply.items)
            .map_err(|e| e.to_string())
    }

    async fn incoming_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyIncomingCall>, String> {
        RacliCore::incoming_calls(self, item)
            .await
            .map(|reply| reply.calls)
            .map_err(|e| e.to_string())
    }

    async fn outgoing_calls(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyOutgoingCall>, String> {
        RacliCore::outgoing_calls(self, item)
            .await
            .map(|reply| reply.calls)
            .map_err(|e| e.to_string())
    }
}

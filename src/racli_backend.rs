//! The racli backend's operations, over a gRPC [`Client`] (CLI) or an in-process [`RacliSession`]
//! (MCP), so the client-side lookups run the same way over both (ADR 006).

use crate::client::Client;
use crate::proto::racli::DocumentSymbolsResponse;
use crate::proto::racli::FindDefinitionResponse;
use crate::proto::racli::HoverResponse;
use crate::proto::racli::SearchResponse;
use crate::racli_session::RacliSession;
use crate::rust_analyzer::SymbolSearchOptions;

/// The racli operations the client-side lookups use. Positions are LSP's, 0-based.
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
    /// Runs `textDocument/documentSymbol` for `file_path` (`Racli.DocumentSymbols`).
    async fn document_symbols(&self, file_path: &str) -> Result<DocumentSymbolsResponse, String>;
    /// Runs `textDocument/hover` at a position in `file_path` (`Racli.Hover`).
    async fn hover(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<HoverResponse, String>;
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
}

/// [`RacliBackend`] over an in-process [`RacliSession`], used by the MCP tools.
#[tonic::async_trait]
impl RacliBackend for RacliSession {
    async fn search(
        &self,
        query: &str,
        options: SymbolSearchOptions,
    ) -> Result<SearchResponse, String> {
        RacliSession::search(self, query.to_string(), options)
            .await
            .map_err(|e| e.to_string())
    }

    async fn find_definition(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<FindDefinitionResponse, String> {
        RacliSession::find_definition(self, file_path.to_string(), line, character)
            .await
            .map_err(|e| e.to_string())
    }

    async fn document_symbols(&self, file_path: &str) -> Result<DocumentSymbolsResponse, String> {
        RacliSession::document_symbols(self, file_path.to_string())
            .await
            .map_err(|e| e.to_string())
    }

    async fn hover(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
    ) -> Result<HoverResponse, String> {
        RacliSession::hover(self, file_path.to_string(), line, character)
            .await
            .map_err(|e| e.to_string())
    }
}

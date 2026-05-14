//! Server-side building blocks: [`Core`] and future service glue.

use serde_json::Value;

use crate::rust_analyzer::RustAnalyzerError;
use crate::rust_analyzer::RustAnalyzerSession;

/// Holds stateless helpers shared by gRPC handlers (e.g. [`Core::version`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Core {}

impl Core {
    /// Returns [`crate::VERSION`] as an owned string for protobuf responses.
    pub fn version(&self) -> String {
        crate::VERSION.to_string()
    }

    /// Runs LSP `workspace/symbol` on the live rust-analyzer session and returns the raw JSON `result`.
    pub async fn search(
        &self,
        ra: &mut RustAnalyzerSession,
        query: String,
    ) -> Result<Value, RustAnalyzerError> {
        ra.workspace_symbol(query).await
    }

    /// Runs LSP `textDocument/definition` on the live rust-analyzer session and returns the raw JSON `result`.
    pub async fn find_definition(
        &self,
        ra: &mut RustAnalyzerSession,
        document_uri: String,
        line: u32,
        character: u32,
    ) -> Result<Value, RustAnalyzerError> {
        ra.text_document_definition(document_uri, line, character)
            .await
    }
}

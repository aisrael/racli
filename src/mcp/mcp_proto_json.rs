//! MCP tool payloads using protobuf-style JSON (`camelCase` field names matching `racli.proto`).

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::proto::racli::FindDefinitionResponse;
use crate::proto::racli::GetVersionResponse;
use crate::proto::racli::LspLocation;
use crate::proto::racli::LspPosition;
use crate::proto::racli::LspRange;
use crate::proto::racli::LspServerInfo;
use crate::proto::racli::LspSymbolInformation;
use crate::proto::racli::LspSymbolInformationList;
use crate::proto::racli::LspWorkspaceSymbol;
use crate::proto::racli::LspWorkspaceSymbolList;
use crate::proto::racli::LspWorkspaceSymbolResponse;
use crate::proto::racli::SearchResponse;
use crate::proto::racli::lsp_workspace_symbol_response;

/// `SearchRequest` JSON body for MCP `search`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequestJson {
    /// Passed to LSP `workspace/symbol`; `|` separates alternative substring patterns (`proto/racli.proto`).
    pub query: String,
}

/// `FindDefinitionRequest` JSON body for MCP `find_definition`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FindDefinitionRequestJson {
    /// Resolved on the server; same rules as gRPC [`crate::proto::racli::FindDefinitionRequest::file_path`].
    pub file_path: String,
    /// Zero-based line (LSP `Position`).
    pub line: u32,
    /// Zero-based UTF-16 character offset on the line.
    pub character: u32,
}

/// Mirrors [`GetVersionResponse`] for structured MCP output.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetVersionResponseJson {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_server_info: Option<LspServerInfoJson>,
}

/// Mirrors [`LspServerInfo`].
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspServerInfoJson {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspPositionJson {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspRangeJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<LspPositionJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<LspPositionJson>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspLocationJson {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<LspRangeJson>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspSymbolInformationJson {
    pub name: String,
    pub kind: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<LspRangeJson>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspSymbolInformationListJson {
    pub items: Vec<LspSymbolInformationJson>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspWorkspaceSymbolJson {
    pub name: String,
    pub kind: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<LspRangeJson>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspWorkspaceSymbolListJson {
    pub items: Vec<LspWorkspaceSymbolJson>,
}

/// One protobuf `oneof`-style wrapper: exactly one branch is non-null in practice.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspWorkspaceSymbolResponseJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flat: Option<LspSymbolInformationListJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested: Option<LspWorkspaceSymbolListJson>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponseJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_symbol_response: Option<LspWorkspaceSymbolResponseJson>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FindDefinitionResponseJson {
    pub locations: Vec<LspLocationJson>,
}

/// Builds [`GetVersionResponseJson`] from the gRPC protobuf struct.
pub fn get_version_response_proto_to_json(p: &GetVersionResponse) -> GetVersionResponseJson {
    GetVersionResponseJson {
        version: p.version.clone(),
        lsp_server_info: p
            .lsp_server_info
            .as_ref()
            .map(lsp_server_info_proto_to_json),
    }
}

fn lsp_server_info_proto_to_json(p: &LspServerInfo) -> LspServerInfoJson {
    LspServerInfoJson {
        name: p.name.clone(),
        version: p.version.clone(),
    }
}

/// Builds [`SearchResponseJson`] from the gRPC protobuf struct.
pub fn search_response_proto_to_json(p: &SearchResponse) -> SearchResponseJson {
    SearchResponseJson {
        workspace_symbol_response: p
            .workspace_symbol_response
            .as_ref()
            .map(lsp_workspace_symbol_response_proto_to_json),
    }
}

fn lsp_workspace_symbol_response_proto_to_json(
    p: &LspWorkspaceSymbolResponse,
) -> LspWorkspaceSymbolResponseJson {
    match &p.payload {
        None => LspWorkspaceSymbolResponseJson {
            flat: None,
            nested: None,
        },
        Some(lsp_workspace_symbol_response::Payload::Flat(list)) => {
            LspWorkspaceSymbolResponseJson {
                flat: Some(lsp_symbol_information_list_proto_to_json(list)),
                nested: None,
            }
        }
        Some(lsp_workspace_symbol_response::Payload::Nested(list)) => {
            LspWorkspaceSymbolResponseJson {
                flat: None,
                nested: Some(lsp_workspace_symbol_list_proto_to_json(list)),
            }
        }
    }
}

fn lsp_symbol_information_list_proto_to_json(
    p: &LspSymbolInformationList,
) -> LspSymbolInformationListJson {
    LspSymbolInformationListJson {
        items: p
            .items
            .iter()
            .map(lsp_symbol_information_proto_to_json)
            .collect(),
    }
}

fn lsp_workspace_symbol_list_proto_to_json(
    p: &LspWorkspaceSymbolList,
) -> LspWorkspaceSymbolListJson {
    LspWorkspaceSymbolListJson {
        items: p
            .items
            .iter()
            .map(lsp_workspace_symbol_proto_to_json)
            .collect(),
    }
}

fn lsp_symbol_information_proto_to_json(p: &LspSymbolInformation) -> LspSymbolInformationJson {
    LspSymbolInformationJson {
        name: p.name.clone(),
        kind: p.kind.clone(),
        uri: p.uri.clone(),
        range: p.range.as_ref().map(range_proto_to_json),
    }
}

fn lsp_workspace_symbol_proto_to_json(p: &LspWorkspaceSymbol) -> LspWorkspaceSymbolJson {
    LspWorkspaceSymbolJson {
        name: p.name.clone(),
        kind: p.kind.clone(),
        uri: p.uri.clone(),
        range: p.range.as_ref().map(range_proto_to_json),
    }
}

fn position_proto_to_json(p: &LspPosition) -> LspPositionJson {
    LspPositionJson {
        line: p.line,
        character: p.character,
    }
}

fn range_proto_to_json(p: &LspRange) -> LspRangeJson {
    LspRangeJson {
        start: p.start.as_ref().map(position_proto_to_json),
        end: p.end.as_ref().map(position_proto_to_json),
    }
}

fn lsp_location_proto_to_json(p: &LspLocation) -> LspLocationJson {
    LspLocationJson {
        uri: p.uri.clone(),
        range: p.range.as_ref().map(range_proto_to_json),
    }
}

/// Builds [`FindDefinitionResponseJson`] from the gRPC protobuf struct.
pub fn find_definition_response_proto_to_json(
    p: &FindDefinitionResponse,
) -> FindDefinitionResponseJson {
    FindDefinitionResponseJson {
        locations: p.locations.iter().map(lsp_location_proto_to_json).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::racli::GetVersionResponse;
    use crate::proto::racli::LspServerInfo;

    #[test]
    fn get_version_json_uses_camel_case_fields() {
        let proto = GetVersionResponse {
            version: "1.0".into(),
            lsp_server_info: Some(LspServerInfo {
                name: "rust-analyzer".into(),
                version: "x".into(),
            }),
        };
        let v = serde_json::to_value(get_version_response_proto_to_json(&proto)).unwrap();
        assert_eq!(v["version"], serde_json::json!("1.0"));
        assert_eq!(
            v["lspServerInfo"]["name"],
            serde_json::json!("rust-analyzer")
        );
    }
}

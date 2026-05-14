//! Maps `lsp_types` workspace symbol responses into racli protobuf messages.

use lsp_types::GotoDefinitionResponse;
use lsp_types::Location;
use lsp_types::LocationLink;
use lsp_types::OneOf;
use lsp_types::Range;
use lsp_types::SymbolInformation;
use lsp_types::SymbolKind;
use lsp_types::Uri;
use lsp_types::WorkspaceLocation;
use lsp_types::WorkspaceSymbol;
use lsp_types::WorkspaceSymbolResponse;

use crate::proto::racli::lsp_workspace_symbol_response;

use crate::proto::racli::LspLocation;
use crate::proto::racli::LspPosition;
use crate::proto::racli::LspRange;
use crate::proto::racli::LspSymbolInformation;
use crate::proto::racli::LspSymbolInformationList;
use crate::proto::racli::LspWorkspaceSymbol;
use crate::proto::racli::LspWorkspaceSymbolList;
use crate::proto::racli::LspWorkspaceSymbolResponse;

/// Builds a protobuf [`LspWorkspaceSymbolResponse`] from a deserialized LSP workspace symbol result.
pub fn workspace_symbol_response_to_proto(
    resp: WorkspaceSymbolResponse,
) -> LspWorkspaceSymbolResponse {
    let payload = match resp {
        WorkspaceSymbolResponse::Flat(items) => {
            lsp_workspace_symbol_response::Payload::Flat(LspSymbolInformationList {
                items: items.into_iter().map(symbol_information_to_proto).collect(),
            })
        }
        WorkspaceSymbolResponse::Nested(items) => {
            lsp_workspace_symbol_response::Payload::Nested(LspWorkspaceSymbolList {
                items: items.into_iter().map(workspace_symbol_to_proto).collect(),
            })
        }
    };
    LspWorkspaceSymbolResponse {
        payload: Some(payload),
    }
}

/// Flattens LSP `textDocument/definition` result shapes into protobuf [`LspLocation`] rows.
pub fn goto_definition_response_to_locations(resp: GotoDefinitionResponse) -> Vec<LspLocation> {
    match resp {
        GotoDefinitionResponse::Scalar(loc) => vec![location_to_proto(loc)],
        GotoDefinitionResponse::Array(locations) => {
            locations.into_iter().map(location_to_proto).collect()
        }
        GotoDefinitionResponse::Link(links) => {
            links.into_iter().map(location_link_to_proto).collect()
        }
    }
}

fn location_to_proto(loc: Location) -> LspLocation {
    LspLocation {
        uri: uri_to_string(&loc.uri),
        range: Some(range_to_proto(loc.range)),
    }
}

fn location_link_to_proto(link: LocationLink) -> LspLocation {
    LspLocation {
        uri: uri_to_string(&link.target_uri),
        range: Some(range_to_proto(link.target_selection_range)),
    }
}

fn uri_to_string(uri: &Uri) -> String {
    uri.as_str().to_string()
}

fn range_to_proto(range: Range) -> LspRange {
    LspRange {
        start: Some(LspPosition {
            line: range.start.line,
            character: range.start.character,
        }),
        end: Some(LspPosition {
            line: range.end.line,
            character: range.end.character,
        }),
    }
}

fn symbol_kind_to_string(kind: SymbolKind) -> String {
    use SymbolKind as K;
    match () {
        _ if kind == K::FILE => "FILE",
        _ if kind == K::MODULE => "MODULE",
        _ if kind == K::NAMESPACE => "NAMESPACE",
        _ if kind == K::PACKAGE => "PACKAGE",
        _ if kind == K::CLASS => "CLASS",
        _ if kind == K::METHOD => "METHOD",
        _ if kind == K::PROPERTY => "PROPERTY",
        _ if kind == K::FIELD => "FIELD",
        _ if kind == K::CONSTRUCTOR => "CONSTRUCTOR",
        _ if kind == K::ENUM => "ENUM",
        _ if kind == K::INTERFACE => "INTERFACE",
        _ if kind == K::FUNCTION => "FUNCTION",
        _ if kind == K::VARIABLE => "VARIABLE",
        _ if kind == K::CONSTANT => "CONSTANT",
        _ if kind == K::STRING => "STRING",
        _ if kind == K::NUMBER => "NUMBER",
        _ if kind == K::BOOLEAN => "BOOLEAN",
        _ if kind == K::ARRAY => "ARRAY",
        _ if kind == K::OBJECT => "OBJECT",
        _ if kind == K::KEY => "KEY",
        _ if kind == K::NULL => "NULL",
        _ if kind == K::ENUM_MEMBER => "ENUM_MEMBER",
        _ if kind == K::STRUCT => "STRUCT",
        _ if kind == K::EVENT => "EVENT",
        _ if kind == K::OPERATOR => "OPERATOR",
        _ if kind == K::TYPE_PARAMETER => "TYPE_PARAMETER",
        _ => {
            return serde_json::to_string(&kind).unwrap_or_else(|_| "\"UNKNOWN\"".into());
        }
    }
    .to_string()
}

fn symbol_information_to_proto(si: SymbolInformation) -> LspSymbolInformation {
    LspSymbolInformation {
        name: si.name,
        kind: symbol_kind_to_string(si.kind),
        uri: uri_to_string(&si.location.uri),
        range: Some(range_to_proto(si.location.range)),
    }
}

fn workspace_location_to_parts(loc: Location) -> (String, Option<LspRange>) {
    (uri_to_string(&loc.uri), Some(range_to_proto(loc.range)))
}

fn workspace_uri_only(wl: WorkspaceLocation) -> (String, Option<LspRange>) {
    (uri_to_string(&wl.uri), None)
}

fn workspace_symbol_to_proto(ws: WorkspaceSymbol) -> LspWorkspaceSymbol {
    let (uri, range) = match ws.location {
        OneOf::Left(loc) => workspace_location_to_parts(loc),
        OneOf::Right(wl) => workspace_uri_only(wl),
    };
    LspWorkspaceSymbol {
        name: ws.name,
        kind: symbol_kind_to_string(ws.kind),
        uri,
        range,
    }
}

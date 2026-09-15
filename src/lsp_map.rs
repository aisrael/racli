//! Maps `lsp_types` workspace symbol responses into racli protobuf messages.

use lsp_types::CallHierarchyIncomingCall;
use lsp_types::CallHierarchyItem;
use lsp_types::CallHierarchyOutgoingCall;
use lsp_types::GotoDefinitionResponse;
use lsp_types::Location;
use lsp_types::LocationLink;
use lsp_types::OneOf;
use lsp_types::Position;
use lsp_types::Range;
use lsp_types::SymbolInformation;
use lsp_types::SymbolKind;
use lsp_types::Uri;
use lsp_types::WorkspaceLocation;
use lsp_types::WorkspaceSymbol;
use lsp_types::WorkspaceSymbolResponse;

use crate::proto::racli::LspCallHierarchyIncomingCall;
use crate::proto::racli::LspCallHierarchyItem;
use crate::proto::racli::LspCallHierarchyOutgoingCall;
use crate::proto::racli::LspLocation;
use crate::proto::racli::LspPosition;
use crate::proto::racli::LspRange;
use crate::proto::racli::LspSymbolInformation;
use crate::proto::racli::LspSymbolInformationList;
use crate::proto::racli::LspWorkspaceSymbol;
use crate::proto::racli::LspWorkspaceSymbolList;
use crate::proto::racli::LspWorkspaceSymbolResponse;
use crate::proto::racli::lsp_workspace_symbol_response;

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

/// Maps LSP `textDocument/references` results into protobuf [`LspLocation`] rows.
pub fn references_to_locations(locations: Vec<Location>) -> Vec<LspLocation> {
    locations.into_iter().map(location_to_proto).collect()
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

/// Inverse of [`range_to_proto`]: missing `start`/`end` map to position `(0, 0)`.
fn proto_range_to_range(range: &LspRange) -> Range {
    let pos = |p: &LspPosition| Position {
        line: p.line,
        character: p.character,
    };
    Range {
        start: range.start.as_ref().map(pos).unwrap_or_default(),
        end: range.end.as_ref().map(pos).unwrap_or_default(),
    }
}

/// Extracts the JSON number backing `SymbolKind` for the wire (the struct field is crate-private).
pub(crate) fn symbol_kind_i32(kind: SymbolKind) -> i32 {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32
}

/// Inverse of [`symbol_kind_i32`]. `SymbolKind` deserializes transparently from any `i32`, so this
/// never fails.
pub(crate) fn symbol_kind_from_i32(kind: i32) -> SymbolKind {
    serde_json::from_value(serde_json::Value::from(kind))
        .expect("SymbolKind deserializes transparently from any i32")
}

pub(crate) fn symbol_kind_to_string(kind: SymbolKind) -> String {
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

/// Builds a protobuf [`LspCallHierarchyItem`] from an LSP item, preserving `data` opaquely as JSON text.
pub(crate) fn call_hierarchy_item_to_proto(item: CallHierarchyItem) -> LspCallHierarchyItem {
    LspCallHierarchyItem {
        name: item.name,
        kind: symbol_kind_i32(item.kind),
        uri: uri_to_string(&item.uri),
        range: Some(range_to_proto(item.range)),
        selection_range: Some(range_to_proto(item.selection_range)),
        detail: item.detail,
        data_json: item.data.and_then(|v| serde_json::to_string(&v).ok()),
    }
}

/// Inverse of [`call_hierarchy_item_to_proto`]; fails only on an unparseable `uri` or `data_json`.
pub(crate) fn call_hierarchy_item_from_proto(
    item: &LspCallHierarchyItem,
) -> Result<CallHierarchyItem, String> {
    let uri: Uri = item
        .uri
        .parse()
        .map_err(|_| format!("invalid call hierarchy item uri: {}", item.uri))?;
    let data = item
        .data_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| format!("invalid call hierarchy item data_json: {e}"))?;
    Ok(CallHierarchyItem {
        name: item.name.clone(),
        kind: symbol_kind_from_i32(item.kind),
        tags: None,
        detail: item.detail.clone(),
        uri,
        range: item.range.as_ref().map(proto_range_to_range).unwrap_or_default(),
        selection_range: item
            .selection_range
            .as_ref()
            .map(proto_range_to_range)
            .unwrap_or_default(),
        data,
    })
}

/// Maps an LSP incoming call (a caller) into its protobuf mirror.
pub(crate) fn call_hierarchy_incoming_call_to_proto(
    call: CallHierarchyIncomingCall,
) -> LspCallHierarchyIncomingCall {
    LspCallHierarchyIncomingCall {
        from: Some(call_hierarchy_item_to_proto(call.from)),
        from_ranges: call.from_ranges.into_iter().map(range_to_proto).collect(),
    }
}

/// Maps an LSP outgoing call (a callee) into its protobuf mirror.
pub(crate) fn call_hierarchy_outgoing_call_to_proto(
    call: CallHierarchyOutgoingCall,
) -> LspCallHierarchyOutgoingCall {
    LspCallHierarchyOutgoingCall {
        to: Some(call_hierarchy_item_to_proto(call.to)),
        from_ranges: call.from_ranges.into_iter().map(range_to_proto).collect(),
    }
}

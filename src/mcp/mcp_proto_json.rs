//! The MCP `get_version` result, as protobuf-style JSON (`camelCase` field names matching `racli.proto`).

use schemars::JsonSchema;
use serde::Serialize;

use crate::proto::racli::GetVersionResponse;
use crate::proto::racli::LspServerInfo;

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

/// Builds [`LspServerInfoJson`] from the gRPC protobuf struct.
fn lsp_server_info_proto_to_json(p: &LspServerInfo) -> LspServerInfoJson {
    LspServerInfoJson {
        name: p.name.clone(),
        version: p.version.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

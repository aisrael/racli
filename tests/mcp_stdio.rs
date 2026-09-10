use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::ConfigureCommandExt;
use rmcp::transport::TokioChildProcess;
use serde_json::json;
use tokio::process::Command;

/// Integration test: spawns the compiled `racli mcp` binary and drives it over real MCP
/// JSON-RPC on its stdio, exercising the `get_version` and `search` tools end-to-end
/// (tool router, in-process rust-analyzer session, and graceful shutdown on transport close).
#[tokio::test]
async fn mcp_stdio_get_version_and_search_round_trip() {
    if std::process::Command::new("rust-analyzer")
        .arg("--version")
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("skip: rust-analyzer not on PATH or --version failed");
        return;
    }

    let transport =
        TokioChildProcess::new(Command::new(env!("CARGO_BIN_EXE_racli")).configure(|cmd| {
            cmd.arg("mcp");
        }))
        .expect("spawn `racli mcp` child process");

    let client = tokio::time::timeout(Duration::from_secs(30), ().serve(transport))
        .await
        .expect("MCP initialize handshake should finish within timeout")
        .expect("MCP initialize handshake");

    let peer = client.peer();

    let version_result = tokio::time::timeout(
        Duration::from_secs(10),
        peer.call_tool(CallToolRequestParams::new("get_version")),
    )
    .await
    .expect("get_version tool call should finish within timeout")
    .expect("get_version tool call");

    let version_content = version_result
        .structured_content
        .expect("get_version should return structured content");
    assert_eq!(version_content["version"], json!(env!("CARGO_PKG_VERSION")));
    assert_eq!(
        version_content["lspServerInfo"]["name"],
        json!("rust-analyzer")
    );
    assert!(
        !version_content["lspServerInfo"]["version"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    );

    // Poll `search` (workspace/symbol via the in-process rust-analyzer session) until
    // indexing has caught up and results appear.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let mut arguments = serde_json::Map::new();
        arguments.insert("query".to_string(), json!(""));

        let search_result = peer
            .call_tool(CallToolRequestParams::new("search").with_arguments(arguments))
            .await
            .expect("search tool call");

        let structured = search_result
            .structured_content
            .expect("search should return structured content");
        let payload = &structured["workspaceSymbolResponse"];
        let non_empty = payload["flat"]["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
            || payload["nested"]["items"]
                .as_array()
                .is_some_and(|items| !items.is_empty());
        if non_empty {
            break;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for MCP search to return results"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    tokio::time::timeout(Duration::from_secs(10), client.cancel())
        .await
        .expect("MCP client shutdown should finish within timeout")
        .expect("MCP client shutdown");
}

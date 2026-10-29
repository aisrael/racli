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

    // `get_version` declares the shape of its structured JSON (ADR 006 decision 2).
    let tools = tokio::time::timeout(Duration::from_secs(10), peer.list_all_tools())
        .await
        .expect("tools/list should finish within timeout")
        .expect("tools/list");
    let get_version = tools
        .iter()
        .find(|tool| tool.name == "get_version")
        .expect("get_version tool");
    let output_schema = get_version
        .output_schema
        .as_ref()
        .expect("get_version should declare an outputSchema");
    assert!(
        output_schema["properties"]["version"].is_object(),
        "unexpected outputSchema: {output_schema:?}"
    );

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

    // Poll `get_version` until its `_meta` says rust-analyzer has finished indexing (ADR 006).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let result = peer
            .call_tool(CallToolRequestParams::new("get_version"))
            .await
            .expect("get_version tool call");
        let backend = result
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get("racli/backend"))
            .cloned();
        if backend == Some(json!("ready")) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for `racli/backend: ready`, last {backend:?}"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    // `search` returns what `racli search --text` prints, with paths relative to the workspace root.
    let mut arguments = serde_json::Map::new();
    arguments.insert("query".to_string(), json!("RacliSession"));
    arguments.insert("exact".to_string(), json!(true));
    let search_result = peer
        .call_tool(CallToolRequestParams::new("search").with_arguments(arguments))
        .await
        .expect("search tool call");
    assert_ne!(search_result.is_error, Some(true));
    let content = serde_json::to_value(&search_result.content).expect("serialize content");
    let text = content[0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("RacliSession [STRUCT] src/racli_session.rs:"),
        "unexpected search output: {text}"
    );

    tokio::time::timeout(Duration::from_secs(10), client.cancel())
        .await
        .expect("MCP client shutdown should finish within timeout")
        .expect("MCP client shutdown");
}

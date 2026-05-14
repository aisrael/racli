use std::path::Path;
use std::time::Duration;

use racli::client::search;
use racli::grpc_server::run_grpc_unix_socket_until_shutdown;
use racli::proto::racli::SearchResponse;
use racli::proto::racli::lsp_workspace_symbol_response::Payload;
use tempfile::tempdir;

fn payload_non_empty(payload: &Payload) -> bool {
    match payload {
        Payload::Flat(list) => !list.items.is_empty(),
        Payload::Nested(list) => !list.items.is_empty(),
    }
}

fn payload_has_symbol_named(payload: &Payload, needle: &str) -> bool {
    match payload {
        Payload::Flat(list) => list.items.iter().any(|i| i.name == needle),
        Payload::Nested(list) => list.items.iter().any(|i| i.name == needle),
    }
}

/// True if every listed name appears on some symbol in `payload`.
fn payload_has_all_symbol_names(payload: &Payload, names: &[&str]) -> bool {
    names.iter().all(|n| payload_has_symbol_named(payload, n))
}

async fn wait_until_socket_path_exists(sock: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !sock.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected server to bind {}",
            sock.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Polls `workspace/symbol` until every `name` appears in one merged result for `query`.
async fn search_until_all_symbols_named(
    sock: &Path,
    query: &str,
    names: &[&str],
) -> SearchResponse {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let resp = search(sock, query)
            .await
            .expect("search should succeed when rust-analyzer is available");
        if let Some(ws) = resp.workspace_symbol_response.as_ref()
            && let Some(p) = ws.payload.as_ref()
            && payload_has_all_symbol_names(p, names)
        {
            return resp;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for symbols {names:?} with query {query:?}"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// Polls `workspace/symbol` until rust-analyzer returns a non-empty flat or nested list (or times out).
async fn search_until_non_empty(sock: &Path, query: &str) -> SearchResponse {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let resp = search(sock, query)
            .await
            .expect("search should succeed when rust-analyzer is available");
        if let Some(ws) = resp.workspace_symbol_response.as_ref()
            && let Some(p) = ws.payload.as_ref()
            && payload_non_empty(p)
        {
            return resp;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for non-empty workspace/symbol for query {query:?}"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// Polls `workspace/symbol` until a symbol with exact `needle` name appears in the result (or times out).
async fn search_until_symbol_named(sock: &Path, query: &str, needle: &str) -> SearchResponse {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let resp = search(sock, query)
            .await
            .expect("search should succeed when rust-analyzer is available");
        if let Some(ws) = resp.workspace_symbol_response.as_ref()
            && let Some(p) = ws.payload.as_ref()
            && payload_has_symbol_named(p, needle)
        {
            return resp;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for symbol named {needle:?} (query {query:?})"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// Integration test: gRPC `Search` returns a flat or nested workspace symbol list from a real workspace.
#[tokio::test]
async fn grpc_search_workspace_symbol_round_trip() {
    if std::process::Command::new("rust-analyzer")
        .arg("--version")
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("skip: rust-analyzer not on PATH or --version failed");
        return;
    }

    let dir = tempdir().expect("temp dir");
    let sock = dir.path().join("test.sock");

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

    let sock_path = sock.clone();
    let server = tokio::spawn(async move {
        run_grpc_unix_socket_until_shutdown(&sock_path, async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });

    wait_until_socket_path_exists(sock.as_path()).await;

    // Empty query requests all symbols per LSP; poll until indexing produces results.
    let resp = search_until_non_empty(sock.as_path(), "").await;
    let ws = resp
        .workspace_symbol_response
        .expect("workspace_symbol_response set");
    let payload = ws
        .payload
        .expect("expected flat or nested payload once indexed");
    match payload {
        Payload::Flat(list) => {
            let first = &list.items[0];
            assert!(!first.name.is_empty() || !first.uri.is_empty());
        }
        Payload::Nested(list) => {
            let first = &list.items[0];
            assert!(!first.name.is_empty() || !first.uri.is_empty());
        }
    }

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

/// Integration test: `search` with query `"GetVersionResponse"` includes that protobuf message symbol.
#[tokio::test]
async fn grpc_search_get_version_response_symbol() {
    if std::process::Command::new("rust-analyzer")
        .arg("--version")
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("skip: rust-analyzer not on PATH or --version failed");
        return;
    }

    let dir = tempdir().expect("temp dir");
    let sock = dir.path().join("test.sock");

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

    let sock_path = sock.clone();
    let server = tokio::spawn(async move {
        run_grpc_unix_socket_until_shutdown(&sock_path, async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });

    wait_until_socket_path_exists(sock.as_path()).await;

    let needle = "GetVersionResponse";
    let _resp = search_until_symbol_named(sock.as_path(), needle, needle).await;

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

/// Integration test: unescaped `|` merges workspace symbol hits from separate substring queries.
#[tokio::test]
async fn grpc_search_pipe_merges_alternative_patterns() {
    if std::process::Command::new("rust-analyzer")
        .arg("--version")
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("skip: rust-analyzer not on PATH or --version failed");
        return;
    }

    let dir = tempdir().expect("temp dir");
    let sock = dir.path().join("test.sock");

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

    let sock_path = sock.clone();
    let server = tokio::spawn(async move {
        run_grpc_unix_socket_until_shutdown(&sock_path, async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });

    wait_until_socket_path_exists(sock.as_path()).await;

    let a = "GetVersionRequest";
    let b = "GetVersionResponse";
    let query = format!("{a}|{b}");
    let _resp = search_until_all_symbols_named(sock.as_path(), &query, &[a, b]).await;

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

use std::path::Path;
use std::time::Duration;

use racli::client::incoming_calls;
use racli::client::prepare_call_hierarchy;
use racli::client::search;
use racli::grpc_server::run_grpc_unix_socket_until_shutdown;
use racli::proto::racli::lsp_workspace_symbol_response::Payload;
use tempfile::tempdir;

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

/// Polls `workspace/symbol` until results appear so rust-analyzer has indexed the workspace.
async fn search_until_non_empty(sock: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let resp = search(sock, "")
            .await
            .expect("search should succeed when rust-analyzer is available");
        let non_empty = match resp.workspace_symbol_response.as_ref() {
            Some(ws) => match ws.payload.as_ref() {
                Some(Payload::Flat(list)) => !list.items.is_empty(),
                Some(Payload::Nested(list)) => !list.items.is_empty(),
                None => false,
            },
            None => false,
        };
        if non_empty {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for workspace/symbol to return results"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// Integration test: gRPC `PrepareCallHierarchy` + `IncomingCalls` resolves
/// `document_uri_from_path` in `src/rust_analyzer.rs` to its callers in `src/racli_session.rs`.
#[tokio::test]
async fn grpc_call_hierarchy_incoming_calls_document_uri_from_path() {
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

    search_until_non_empty(sock.as_path()).await;

    let rust_analyzer_rs = std::env::current_dir()
        .expect("cwd")
        .join("src/rust_analyzer.rs");
    let file_path = rust_analyzer_rs
        .canonicalize()
        .expect("canonicalize src/rust_analyzer.rs");

    // 0-based LSP position on `document_uri_from_path` in `pub fn document_uri_from_path(...)`.
    let prepared =
        prepare_call_hierarchy(sock.as_path(), file_path.to_string_lossy().as_ref(), 482, 7)
            .await
            .expect("prepare_call_hierarchy");

    assert!(
        !prepared.items.is_empty(),
        "expected at least one call hierarchy item for document_uri_from_path"
    );
    let item = prepared.items[0].clone();
    assert_eq!(item.name, "document_uri_from_path");

    let calls = incoming_calls(sock.as_path(), item)
        .await
        .expect("incoming_calls");

    assert!(
        calls.calls.len() >= 2,
        "expected multiple callers of document_uri_from_path, got {:?}",
        calls.calls
    );
    let racli_session_callers = calls
        .calls
        .iter()
        .filter(|c| {
            c.from
                .as_ref()
                .is_some_and(|f| f.uri.ends_with("racli_session.rs"))
        })
        .count();
    assert!(
        racli_session_callers >= 2,
        "expected multiple callers in racli_session.rs, got {:?}",
        calls.calls
    );

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

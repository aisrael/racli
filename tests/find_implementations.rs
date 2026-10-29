use std::path::Path;
use std::time::Duration;

use racli::client::Client;
use racli::grpc_server::run_grpc_unix_socket_until_shutdown;
use racli::proto::racli::lsp_workspace_symbol_response::Payload;
use racli::rust_analyzer::DEFAULT_SYMBOL_SEARCH_LIMIT;
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
        let resp = Client::new(sock)
            .unwrap()
            .search("")
            .await
            .expect("search should succeed when rust-analyzer is available")
            .value;
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

/// Integration test: gRPC `FindImplementations` resolves `RacliBackend` in `src/racli_backend.rs` to its impl blocks.
#[tokio::test]
async fn grpc_find_implementations_racli_backend() {
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
        run_grpc_unix_socket_until_shutdown(&sock_path, DEFAULT_SYMBOL_SEARCH_LIMIT, async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });

    wait_until_socket_path_exists(sock.as_path()).await;

    search_until_non_empty(sock.as_path()).await;

    let racli_backend_rs = std::env::current_dir()
        .expect("cwd")
        .join("src/racli_backend.rs");
    let file_path = racli_backend_rs
        .canonicalize()
        .expect("canonicalize racli_backend.rs");

    // 0-based LSP position on `RacliBackend` in `pub trait RacliBackend: Send + Sync {`.
    let resp = Client::new(sock.as_path())
        .unwrap()
        .find_implementations(file_path.to_string_lossy().as_ref(), 20, 10)
        .await
        .expect("find_implementations")
        .value;

    let mut saw_racli_backend = false;
    for loc in &resp.locations {
        if loc.uri.ends_with("racli_backend.rs") {
            saw_racli_backend = true;
            assert!(
                loc.range.is_some(),
                "expected range on implementation location"
            );
        }
    }
    assert!(
        saw_racli_backend,
        "expected at least one implementation in racli_backend.rs, got {:?}",
        resp.locations
    );

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

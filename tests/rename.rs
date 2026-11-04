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

/// Integration test: gRPC `Rename` returns the edits that rename `RustAnalyzerSession` and writes none of them.
#[tokio::test]
async fn grpc_rename_reports_edits_without_writing() {
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

    let definition_rs = std::env::current_dir()
        .expect("cwd")
        .join("src/rust_analyzer.rs");
    let file_path = definition_rs
        .canonicalize()
        .expect("canonicalize rust_analyzer.rs");
    let before = std::fs::read_to_string(&file_path).expect("read rust_analyzer.rs");

    // 0-based LSP position on the name in `pub struct RustAnalyzerSession`. (Renaming from a
    // `use` line would add an `as` alias there instead of renaming the declaration.)
    let declaration = "pub struct RustAnalyzerSession";
    let line = before
        .lines()
        .position(|l| l.starts_with(declaration))
        .expect("rust_analyzer.rs declares RustAnalyzerSession") as u32;
    let character = "pub struct ".len() as u32 + 1;

    let resp = Client::new(sock.as_path())
        .unwrap()
        .rename(
            file_path.to_string_lossy().as_ref(),
            line,
            character,
            "RenamedSession",
        )
        .await
        .expect("rename")
        .value;

    let edited = |suffix: &str| resp.files.iter().find(|f| f.uri.ends_with(suffix));
    for suffix in ["src/rust_analyzer.rs", "src/server.rs"] {
        let file =
            edited(suffix).unwrap_or_else(|| panic!("expected edits in {suffix}, got {resp:?}"));
        assert!(!file.edits.is_empty());
        assert!(file.edits.iter().all(|e| e.new_text == "RenamedSession"));
        assert!(file.edits.iter().all(|e| e.range.is_some()));
    }

    let after = std::fs::read_to_string(&file_path).expect("read rust_analyzer.rs again");
    assert_eq!(before, after, "rename must not write to the workspace");

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

use std::path::Path;
use std::time::Duration;

use racli::client::document_symbols;
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

/// Integration test: gRPC `DocumentSymbols` returns `Core` in `src/server.rs` with its methods present in its subtree.
#[tokio::test]
async fn grpc_document_symbols_server_rs_core_struct() {
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

    let server_rs = std::env::current_dir().expect("cwd").join("src/server.rs");
    let file_path = server_rs.canonicalize().expect("canonicalize server.rs");

    let resp = document_symbols(sock.as_path(), file_path.to_string_lossy().as_ref())
        .await
        .expect("document_symbols");

    assert!(
        !resp.symbols.is_empty(),
        "expected at least one top-level symbol in src/server.rs"
    );

    // Flatten the tree (depth-first) so `Core`'s methods are found whether rust-analyzer nests
    // them directly under the `Core` struct symbol or under an intermediate `impl` symbol.
    fn collect_names<'a>(symbols: &'a [racli::proto::racli::LspDocumentSymbol], out: &mut Vec<&'a str>) {
        for s in symbols {
            out.push(s.name.as_str());
            collect_names(&s.children, out);
        }
    }
    let mut all_names = Vec::new();
    collect_names(&resp.symbols, &mut all_names);

    assert!(
        all_names.contains(&"Core"),
        "expected a `Core` symbol, got {all_names:?}"
    );
    for expected in [
        "version",
        "search",
        "find_definition",
        "find_references",
        "document_symbols",
    ] {
        assert!(
            all_names.contains(&expected),
            "expected symbol `{expected}` somewhere in the tree, got {all_names:?}"
        );
    }

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

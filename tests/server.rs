use std::time::Duration;

use racli::client::get_version;
use racli::grpc_server::run_grpc_unix_socket_until_shutdown;
use racli::rust_analyzer::DEFAULT_SYMBOL_SEARCH_LIMIT;
use tempfile::tempdir;

/// Integration test: temporary UDS, gRPC `GetVersion` matches `CARGO_PKG_VERSION`, then server shuts down cleanly.
#[tokio::test]
async fn grpc_get_version_round_trip() {
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

    tokio::time::sleep(Duration::from_millis(100)).await;

    let resp = get_version(&sock).await.expect("get_version");
    assert_eq!(resp.version, env!("CARGO_PKG_VERSION"));
    let lsp = resp.lsp_server_info.expect("lsp_server_info");
    assert_eq!(lsp.name, "rust-analyzer");
    assert!(!lsp.version.is_empty());

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

/// Regression test for editor restarts: when a new server re-binds the socket path before the old
/// one finishes shutting down, the old server must not delete the new server's socket.
#[tokio::test]
async fn stopping_old_server_keeps_successors_socket() {
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
    let sock = dir.path().join("restart.sock");

    let start = |stop_rx: tokio::sync::oneshot::Receiver<()>| {
        let sock = sock.clone();
        tokio::spawn(async move {
            run_grpc_unix_socket_until_shutdown(&sock, DEFAULT_SYMBOL_SEARCH_LIMIT, async {
                let _ = stop_rx.await;
            })
            .await
            .unwrap();
        })
    };
    let wait_for_server = async |what: &str| {
        for _ in 0..100 {
            if get_version(&sock).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("{what} never answered on {}", sock.display());
    };

    let inode = |path: &std::path::Path| {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).ok().map(|m| m.ino())
    };

    let (old_stop, old_rx) = tokio::sync::oneshot::channel::<()>();
    let old = start(old_rx);
    wait_for_server("old server").await;
    let old_inode = inode(&sock);

    // The successor takes over the path while the old server is still running. Wait until the
    // path holds the successor's own socket file, not the old one it is about to replace.
    let (new_stop, new_rx) = tokio::sync::oneshot::channel::<()>();
    let new = start(new_rx);
    for _ in 0..100 {
        if inode(&sock).is_some_and(|i| Some(i) != old_inode) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_ne!(
        inode(&sock),
        old_inode,
        "successor never re-bound the socket"
    );
    wait_for_server("new server").await;

    let _ = old_stop.send(());
    tokio::time::timeout(Duration::from_secs(30), old)
        .await
        .expect("old server should stop")
        .expect("old server join");

    assert!(sock.exists(), "old server deleted its successor's socket");
    get_version(&sock)
        .await
        .expect("successor should still answer after the old server stopped");

    let _ = new_stop.send(());
    tokio::time::timeout(Duration::from_secs(30), new)
        .await
        .expect("new server should stop")
        .expect("new server join");
    assert!(
        !sock.exists(),
        "server should remove its own socket on shutdown"
    );
}

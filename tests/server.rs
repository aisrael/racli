use std::time::Duration;

use racli::client::get_server_version;
use racli::grpc_server::run_grpc_unix_socket_until_shutdown;
use tempfile::tempdir;

/// Integration test: temporary UDS, gRPC `GetVersion` matches `CARGO_PKG_VERSION`, then server shuts down cleanly.
#[tokio::test]
async fn grpc_get_version_round_trip() {
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

    tokio::time::sleep(Duration::from_millis(100)).await;

    let v = get_server_version(&sock).await.expect("get_version");
    assert_eq!(v, env!("CARGO_PKG_VERSION"));

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

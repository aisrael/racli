use std::time::Duration;

use racli::racli_live_backend::RacliLiveBackend;
use tempfile::tempdir;

/// Integration test: [`RacliLiveBackend`] returns the crate version and rust-analyzer serverInfo, then shuts down cleanly.
#[tokio::test]
async fn live_backend_get_version_round_trip() {
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
    let root = dir.path().to_path_buf();

    let backend = tokio::time::timeout(Duration::from_secs(120), RacliLiveBackend::start(root))
        .await
        .expect("backend start should finish within timeout")
        .expect("RacliLiveBackend::start");

    let resp = backend.session().get_version();
    assert_eq!(resp.version, env!("CARGO_PKG_VERSION"));
    let lsp = resp.lsp_server_info.expect("lsp_server_info");
    assert_eq!(lsp.name, "rust-analyzer");
    assert!(!lsp.version.is_empty());

    tokio::time::timeout(Duration::from_secs(30), backend.shutdown())
        .await
        .expect("shutdown should finish within timeout")
        .expect("shutdown ok");
}

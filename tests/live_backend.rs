use std::time::Duration;

use racli::backend_status::BackendStatus;
use racli::proto::racli::lsp_workspace_symbol_response::Payload;
use racli::racli_live_backend::RacliLiveBackend;
use racli::racli_session::RacliRpcError;
use racli::racli_session::RacliSession;
use racli::rust_analyzer::DEFAULT_SYMBOL_SEARCH_LIMIT;
use racli::rust_analyzer::SymbolSearchOptions;
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

    let backend = tokio::time::timeout(
        Duration::from_secs(120),
        RacliLiveBackend::start(root, DEFAULT_SYMBOL_SEARCH_LIMIT),
    )
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

/// Integration test: once the backend is shut down, a retained [`racli::racli_session::RacliSession`] fails requests with `Internal` instead of hanging or panicking.
#[tokio::test]
async fn live_backend_requests_after_shutdown_fail_internal() {
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

    let backend = tokio::time::timeout(
        Duration::from_secs(120),
        RacliLiveBackend::start(root, DEFAULT_SYMBOL_SEARCH_LIMIT),
    )
    .await
    .expect("backend start should finish within timeout")
    .expect("RacliLiveBackend::start");

    let session = backend.session().clone();
    tokio::time::timeout(Duration::from_secs(30), backend.shutdown())
        .await
        .expect("shutdown should finish within timeout")
        .expect("shutdown ok");

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        session.search("anything".into(), SymbolSearchOptions::default()),
    )
    .await
    .expect("search after shutdown should not hang");
    assert!(
        matches!(result, Err(RacliRpcError::Internal(_))),
        "expected Internal error after shutdown, got {result:?}"
    );
}

/// Whether a default (types only) `search` for `name` finds a symbol named exactly `name`.
async fn finds_type(session: &RacliSession, name: &str) -> bool {
    let Ok(resp) = session
        .search(name.into(), SymbolSearchOptions::default())
        .await
    else {
        return false;
    };
    match resp.workspace_symbol_response.and_then(|r| r.payload) {
        Some(Payload::Flat(list)) => list.items.iter().any(|i| i.name == name),
        Some(Payload::Nested(list)) => list.items.iter().any(|i| i.name == name),
        None => false,
    }
}

/// Retries `done` every 100ms until it returns `true`, panicking after 60s.
async fn wait_until<F, Fut>(what: &str, mut done: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..600 {
        if done().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Integration test: a type added to a file after startup is found without a restart, whether
/// racli's own watcher (the `notify` feature) or rust-analyzer's reports the change.
#[tokio::test]
async fn backend_sees_files_changed_on_disk() {
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
    let root = dir.path().canonicalize().expect("canonical temp dir");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"watched\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::create_dir(root.join("src")).expect("create src");
    let lib_rs = root.join("src").join("lib.rs");
    std::fs::write(&lib_rs, "pub struct OriginalType;\n").expect("write lib.rs");

    let backend = tokio::time::timeout(
        Duration::from_secs(120),
        RacliLiveBackend::start(root, DEFAULT_SYMBOL_SEARCH_LIMIT),
    )
    .await
    .expect("backend start should finish within timeout")
    .expect("RacliLiveBackend::start");
    let session = backend.session().clone();

    wait_until("rust-analyzer to be ready", || async {
        session.backend_status() == BackendStatus::Ready
    })
    .await;
    wait_until("OriginalType", || finds_type(&session, "OriginalType")).await;

    std::fs::write(
        &lib_rs,
        "pub struct OriginalType;\npub struct AddedAfterStart;\n",
    )
    .expect("rewrite lib.rs");
    wait_until("AddedAfterStart after the file changed", || {
        finds_type(&session, "AddedAfterStart")
    })
    .await;

    tokio::time::timeout(Duration::from_secs(30), backend.shutdown())
        .await
        .expect("shutdown should finish within timeout")
        .expect("shutdown ok");
}

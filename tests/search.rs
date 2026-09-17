use std::path::Path;
use std::time::Duration;

use racli::client::search;
use racli::client::search_with_options;
use racli::grpc_server::run_grpc_unix_socket_until_shutdown;
use racli::proto::racli::SearchResponse;
use racli::proto::racli::lsp_workspace_symbol_response::Payload;
use racli::rust_analyzer::DEFAULT_SYMBOL_SEARCH_LIMIT;
use racli::rust_analyzer::SymbolSearchKind;
use racli::rust_analyzer::SymbolSearchOptions;
use racli::rust_analyzer::SymbolSearchScope;
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

/// Number of symbols in `payload`.
fn payload_len(payload: &Payload) -> usize {
    match payload {
        Payload::Flat(list) => list.items.len(),
        Payload::Nested(list) => list.items.len(),
    }
}

/// True if some symbol in `payload` has the given LSP kind name (e.g. `FUNCTION`).
fn payload_has_kind(payload: &Payload, kind: &str) -> bool {
    match payload {
        Payload::Flat(list) => list.items.iter().any(|i| i.kind == kind),
        Payload::Nested(list) => list.items.iter().any(|i| i.kind == kind),
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

/// Polls `workspace/symbol` with `options` until `pred` holds for the payload (or times out).
async fn search_with_options_until(
    sock: &Path,
    query: &str,
    options: SymbolSearchOptions,
    pred: impl Fn(&Payload) -> bool,
) -> SearchResponse {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let resp = search_with_options(sock, query, options)
            .await
            .expect("search should succeed when rust-analyzer is available");
        if let Some(ws) = resp.workspace_symbol_response.as_ref()
            && let Some(p) = ws.payload.as_ref()
            && pred(p)
        {
            return resp;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting on workspace/symbol for query {query:?} with {options:?}"
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// Unwraps the payload of a search response, treating a missing payload as an empty list.
fn payload_of(resp: &SearchResponse) -> Payload {
    resp.workspace_symbol_response
        .as_ref()
        .and_then(|ws| ws.payload.clone())
        .unwrap_or(Payload::Flat(Default::default()))
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
        run_grpc_unix_socket_until_shutdown(&sock_path, DEFAULT_SYMBOL_SEARCH_LIMIT, async {
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
        run_grpc_unix_socket_until_shutdown(&sock_path, DEFAULT_SYMBOL_SEARCH_LIMIT, async {
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
        run_grpc_unix_socket_until_shutdown(&sock_path, DEFAULT_SYMBOL_SEARCH_LIMIT, async {
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

/// Integration test: the server's symbol search limit reaches rust-analyzer and caps an empty query.
#[tokio::test]
async fn grpc_search_respects_symbol_search_limit() {
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

    const LIMIT: u32 = 3;
    let sock_path = sock.clone();
    let server = tokio::spawn(async move {
        run_grpc_unix_socket_until_shutdown(&sock_path, LIMIT, async {
            let _ = stop_rx.await;
        })
        .await
        .unwrap();
    });

    wait_until_socket_path_exists(sock.as_path()).await;

    let _resp =
        search_with_options_until(sock.as_path(), "", SymbolSearchOptions::default(), |p| {
            let len = payload_len(p);
            assert!(
                len <= LIMIT as usize,
                "expected at most {LIMIT} symbols, got {len}"
            );
            len == LIMIT as usize
        })
        .await;

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

/// Integration test: `kind` and `scope` overrides reach rust-analyzer's `searchKind` / `searchScope` extension.
#[tokio::test]
async fn grpc_search_kind_and_scope_overrides() {
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

    // All symbols: an empty query includes functions once indexed.
    let all_symbols = SymbolSearchOptions {
        kind: Some(SymbolSearchKind::AllSymbols),
        scope: None,
    };
    let _resp = search_with_options_until(sock.as_path(), "", all_symbols, |p| {
        payload_has_kind(p, "FUNCTION")
    })
    .await;

    // Only types: the same empty query returns no functions.
    let only_types = SymbolSearchOptions {
        kind: Some(SymbolSearchKind::OnlyTypes),
        scope: None,
    };
    let resp = search_with_options(sock.as_path(), "", only_types)
        .await
        .expect("only-types search");
    let payload = payload_of(&resp);
    assert!(payload_len(&payload) > 0, "expected types for empty query");
    assert!(
        !payload_has_kind(&payload, "FUNCTION"),
        "only-types returned a function"
    );

    // Dependencies scope finds std's `HashMap`; the default workspace scope does not.
    let with_deps = SymbolSearchOptions {
        kind: None,
        scope: Some(SymbolSearchScope::WorkspaceAndDependencies),
    };
    let _resp = search_with_options_until(sock.as_path(), "HashMap", with_deps, |p| {
        payload_has_symbol_named(p, "HashMap")
    })
    .await;
    let resp = search(sock.as_path(), "HashMap")
        .await
        .expect("workspace search");
    assert!(
        !payload_has_symbol_named(&payload_of(&resp), "HashMap"),
        "workspace scope returned a dependency symbol"
    );

    let _ = stop_tx.send(());

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server task should finish within timeout")
        .expect("server join");
}

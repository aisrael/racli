use std::process::Stdio;
use std::time::Duration;

use racli::client::get_version;
use serde_json::Value;
use serde_json::json;
use tempfile::tempdir;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;

/// Writes `msg` to the tee's stdin as one LSP frame.
async fn send(stdin: &mut ChildStdin, msg: Value) {
    let body = msg.to_string();
    let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
    stdin.write_all(frame.as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
}

/// Reads one LSP frame from the tee's stdout.
async fn recv(stdout: &mut BufReader<ChildStdout>) -> Value {
    let mut len = None;
    loop {
        let mut line = String::new();
        assert!(
            stdout.read_line(&mut line).await.unwrap() > 0,
            "tee closed stdout"
        );
        if line == "\r\n" {
            break;
        }
        if let Some(n) = line.strip_prefix("Content-Length: ") {
            len = Some(n.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; len.expect("Content-Length")];
    stdout.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Reads frames until the response with `id`, skipping server notifications.
async fn recv_response(stdout: &mut BufReader<ChildStdout>, id: i64) -> Value {
    loop {
        let msg = recv(stdout).await;
        if msg.get("id") == Some(&json!(id)) {
            return msg;
        }
        assert!(msg.get("method").is_some(), "unexpected message: {msg}");
    }
}

/// Returns whether `rust-analyzer --version` succeeds; logs a skip notice otherwise.
fn rust_analyzer_available() -> bool {
    let ok = std::process::Command::new("rust-analyzer")
        .arg("--version")
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("skip: rust-analyzer not on PATH or --version failed");
    }
    ok
}

/// Integration test: `racli tee` answers editor LSP on stdio (via its own gRPC socket), shares that
/// socket with other gRPC clients, and cleans up after `shutdown`/`exit`.
#[tokio::test]
async fn tee_proxies_stdio_lsp_through_grpc() {
    if !rust_analyzer_available() {
        return;
    }

    let dir = tempdir().expect("temp dir");
    let sock = dir.path().join("tee.sock");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_racli"))
        .arg("tee")
        .env("RACLI_UNIX_SOCKET", &sock)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn racli tee");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let lib_rs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/lib.rs")
        .canonicalize()
        .unwrap();
    let lib_uri = url::Url::from_file_path(&lib_rs).unwrap().to_string();

    let session = async {
        send(
            &mut stdin,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}}),
        )
        .await;
        let init = recv_response(&mut stdout, 1).await;
        assert_eq!(init["result"]["serverInfo"]["name"], "rust-analyzer");
        send(
            &mut stdin,
            json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        )
        .await;

        // The gRPC socket is shared with other clients while the editor session is live.
        let version = get_version(&sock).await.expect("get_version");
        assert_eq!(version.version, env!("CARGO_PKG_VERSION"));

        // Like an editor, open the document first (forwarded via `LspNotify`).
        send(
            &mut stdin,
            json!({"jsonrpc": "2.0", "method": "textDocument/didOpen",
                   "params": {"textDocument": {"uri": lib_uri, "languageId": "rust", "version": 1,
                                               "text": std::fs::read_to_string(&lib_rs).unwrap()}}}),
        )
        .await;
        send(
            &mut stdin,
            json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/documentSymbol",
                   "params": {"textDocument": {"uri": lib_uri}}}),
        )
        .await;
        let symbols = recv_response(&mut stdout, 2).await;
        let names: Vec<&str> = symbols["result"]
            .as_array()
            .unwrap_or_else(|| panic!("documentSymbol result array: {symbols}"))
            .iter()
            .filter_map(|s| s["name"].as_str())
            .collect();
        assert!(names.contains(&"tee"), "expected `tee` module in {names:?}");

        // Unknown methods come back as rust-analyzer's own JSON-RPC error, with the editor's id.
        send(
            &mut stdin,
            json!({"jsonrpc": "2.0", "id": 3, "method": "racli/doesNotExist", "params": {}}),
        )
        .await;
        let unknown = recv_response(&mut stdout, 3).await;
        assert!(
            unknown["error"]["code"].is_i64(),
            "expected error: {unknown}"
        );

        send(
            &mut stdin,
            json!({"jsonrpc": "2.0", "id": 4, "method": "shutdown"}),
        )
        .await;
        assert_eq!(recv_response(&mut stdout, 4).await["result"], Value::Null);
        send(&mut stdin, json!({"jsonrpc": "2.0", "method": "exit"})).await;
    };
    tokio::time::timeout(Duration::from_secs(120), session)
        .await
        .expect("LSP session should finish in time");

    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("racli tee should exit after `exit`")
        .unwrap();
    assert!(status.success(), "racli tee exited with {status}");
    assert!(!sock.exists(), "socket should be removed on shutdown");
}

/// Integration test: with `RACLI_DERIVE_SOCKET_PATH=1` and no `RACLI_UNIX_SOCKET`, `racli tee`
/// serves on the socket derived from its working directory and removes it on shutdown.
#[tokio::test]
async fn tee_derives_socket_path_from_cwd() {
    if !rust_analyzer_available() {
        return;
    }

    // A fresh directory, so the derived socket can't collide with a real project's racli.
    let project = tempdir().expect("temp dir");
    let sock = racli::utils::derived_unix_socket_path(project.path());
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_racli"))
        .arg("tee")
        .env_remove("RACLI_UNIX_SOCKET")
        .env("RACLI_DERIVE_SOCKET_PATH", "1")
        .current_dir(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn racli tee");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let session = async {
        send(
            &mut stdin,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}}),
        )
        .await;
        recv_response(&mut stdout, 1).await;

        let version = get_version(&sock)
            .await
            .expect("get_version on derived socket");
        assert_eq!(version.version, env!("CARGO_PKG_VERSION"));

        send(
            &mut stdin,
            json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
        )
        .await;
        recv_response(&mut stdout, 2).await;
        send(&mut stdin, json!({"jsonrpc": "2.0", "method": "exit"})).await;
    };
    tokio::time::timeout(Duration::from_secs(120), session)
        .await
        .expect("LSP session should finish in time");

    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("racli tee should exit after `exit`")
        .unwrap();
    assert!(status.success(), "racli tee exited with {status}");
    assert!(
        !sock.exists(),
        "derived socket should be removed on shutdown"
    );
}

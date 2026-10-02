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

/// Integration test: the `racli-tee` binary, run with no arguments as an editor would, serves an
/// LSP session through the socket and exits cleanly.
#[tokio::test]
async fn racli_tee_binary_serves_lsp_without_arguments() {
    if !rust_analyzer_available() {
        return;
    }

    let dir = tempdir().expect("temp dir");
    let sock = dir.path().join("tee.sock");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_racli-tee"))
        .env("RACLI_UNIX_SOCKET", &sock)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn racli-tee");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

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
        send(
            &mut stdin,
            json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
        )
        .await;
        assert_eq!(recv_response(&mut stdout, 2).await["result"], Value::Null);
        send(&mut stdin, json!({"jsonrpc": "2.0", "method": "exit"})).await;
    };
    tokio::time::timeout(Duration::from_secs(120), session)
        .await
        .expect("LSP session should finish in time");

    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("racli-tee should exit after `exit`")
        .unwrap();
    assert!(status.success(), "racli-tee exited with {status}");
    assert!(!sock.exists(), "socket should be removed on shutdown");
}

/// `racli-tee --version` exits 0, since editors probe the server binary with it before launching.
#[test]
fn racli_tee_binary_version_exits_zero() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_racli-tee"))
        .arg("--version")
        .output()
        .expect("run racli-tee --version");
    assert!(output.status.success(), "exited with {}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "expected version in {stdout:?}"
    );
}

/// Integration test: by default (neither `RACLI_UNIX_SOCKET` nor `RACLI_DERIVE_SOCKET_PATH` set),
/// `racli tee` serves on the socket derived from its working directory, a client run from a
/// subdirectory finds it automatically, and the socket is removed on shutdown.
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
        .env_remove("RACLI_DERIVE_SOCKET_PATH")
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

        // A client run from a subdirectory walks up and finds the project's socket on its own.
        let subdir = project.path().join("sub/dir");
        std::fs::create_dir_all(&subdir).unwrap();
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_racli"))
            .arg("version")
            .env_remove("RACLI_UNIX_SOCKET")
            .env_remove("RACLI_DERIVE_SOCKET_PATH")
            .current_dir(&subdir)
            .output()
            .await
            .expect("run racli version");
        let stdout_text = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout_text.contains(concat!("server: ", env!("CARGO_PKG_VERSION"))),
            "racli version from a subdirectory should reach the tee; stdout: {stdout_text}, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

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

/// Creates `<dir>/bin/rust-analyzer`, a stand-in for rustup's shim on a toolchain without the
/// component (prints rustup's error, exits 1), and returns a `PATH` that finds it first.
fn path_with_failing_rust_analyzer(dir: &std::path::Path) -> String {
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("rust-analyzer");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho \"error: Unknown binary 'rust-analyzer' in official toolchain 'nightly'.\" >&2\nexit 1\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Integration test: when rust-analyzer exits during startup, `racli tee` exits 1 with a readable
/// message, not a Debug dump.
#[tokio::test]
async fn tee_reports_rust_analyzer_startup_failure_readably() {
    let dir = tempdir().expect("temp dir");
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_racli"))
            .arg("tee")
            .env("PATH", path_with_failing_rust_analyzer(dir.path()))
            .env("RACLI_UNIX_SOCKET", dir.path().join("tee.sock"))
            .env_remove("RACLI_SERVER_LOG_FILE")
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .output(),
    )
    .await
    .expect("racli tee should exit when rust-analyzer fails to start")
    .expect("run racli tee");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(output.stdout.is_empty(), "stdout must stay LSP-only");
    assert!(
        stderr.contains("Unknown binary 'rust-analyzer'"),
        "rust-analyzer's own stderr should pass through: {stderr}"
    );
    assert!(
        stderr.contains("error: rust-analyzer exited during startup (exit status: 1)"),
        "expected a readable startup error: {stderr}"
    );
    assert!(
        !stderr.contains("Grpc("),
        "no Debug-formatted errors: {stderr}"
    );
}

/// Integration test: a relative `RACLI_SERVER_LOG_FILE` is created under the working directory,
/// including missing parent directories.
#[tokio::test]
async fn tee_relative_log_file_is_based_on_cwd() {
    let dir = tempdir().expect("temp dir");
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_racli"))
            .arg("tee")
            .env("PATH", path_with_failing_rust_analyzer(dir.path()))
            .env("RACLI_UNIX_SOCKET", dir.path().join("tee.sock"))
            .env("RACLI_SERVER_LOG_FILE", ".racli/racli.log")
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .output(),
    )
    .await
    .expect("racli tee should exit when rust-analyzer fails to start")
    .expect("run racli tee");

    let log = dir.path().join(".racli/racli.log");
    let contents = std::fs::read_to_string(&log).unwrap_or_else(|e| {
        panic!(
            "expected {}: {e}; stderr: {}",
            log.display(),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(contents.contains("racli tee starting"), "log: {contents}");
    assert!(
        contents.contains("rust-analyzer exited during startup"),
        "log: {contents}"
    );
}

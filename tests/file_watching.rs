use std::process::Stdio;
use std::time::Duration;

use racli::backend_status::BackendStatus;
use racli::client::Client;
use racli::proto::racli::lsp_workspace_symbol_response::Payload;
use tempfile::tempdir;
use tokio::process::Child;

/// Whether a default (types only) `search` for `name` finds a symbol named exactly `name`.
async fn finds_type(client: &Client, name: &str) -> bool {
    let Ok(reply) = client.search(name).await else {
        return false;
    };
    match reply
        .value
        .workspace_symbol_response
        .and_then(|r| r.payload)
    {
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

/// Sends `SIGTERM` so the server shuts rust-analyzer down cleanly, and waits for it to exit.
async fn stop(mut server: Child) {
    let pid = server.id().expect("racli server pid");
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    let status = tokio::time::timeout(Duration::from_secs(30), server.wait())
        .await
        .expect("racli server should exit within timeout")
        .expect("wait for racli server");
    assert!(status.success(), "racli server exited with {status}");
}

/// Integration test: `racli server` finds a type added to a file after startup without a restart,
/// whether racli's own watcher (the `notify` feature) or rust-analyzer's reports the change.
#[tokio::test]
async fn server_sees_files_changed_on_disk() {
    if std::process::Command::new("rust-analyzer")
        .arg("--version")
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("skip: rust-analyzer not on PATH or --version failed");
        return;
    }

    let workspace = tempdir().expect("temp workspace");
    let root = workspace.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"watched\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::create_dir(root.join("src")).expect("create src");
    let lib_rs = root.join("src").join("lib.rs");
    std::fs::write(&lib_rs, "pub struct OriginalType;\n").expect("write lib.rs");

    // Outside the workspace, so the socket isn't one of the files being watched.
    let sockets = tempdir().expect("temp socket dir");
    let sock = sockets.path().join("racli.sock");
    let server = tokio::process::Command::new(env!("CARGO_BIN_EXE_racli"))
        .arg("server")
        .env("RACLI_UNIX_SOCKET", &sock)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn `racli server`");
    let client = Client::new(&sock).expect("client");

    wait_until("rust-analyzer to be ready", || async {
        client
            .get_version()
            .await
            .is_ok_and(|reply| reply.backend == BackendStatus::Ready)
    })
    .await;
    wait_until("OriginalType", || finds_type(&client, "OriginalType")).await;

    std::fs::write(
        &lib_rs,
        "pub struct OriginalType;\npub struct AddedAfterStart;\n",
    )
    .expect("rewrite lib.rs");
    wait_until("AddedAfterStart after the file changed", || {
        finds_type(&client, "AddedAfterStart")
    })
    .await;

    stop(server).await;
}

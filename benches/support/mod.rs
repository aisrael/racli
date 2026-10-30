//! Shared helpers for spawning and tearing down a background `racli server` and a `racli mcp`
//! session in benchmarks.

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use rmcp::RoleClient;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::ConfigureCommandExt;
use rmcp::transport::TokioChildProcess;
use serde_json::Value;
use tempfile::TempDir;

/// Resolves the `racli` binary built by Cargo, or `target/{release,debug}/racli` under the manifest.
pub fn racli_executable() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_racli")
        .map(PathBuf::from)
        .or_else(|| {
            let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
            for sub in ["release", "debug"] {
                let p = manifest.join("target").join(sub).join("racli");
                if p.is_file() {
                    return Some(p);
                }
            }
            None
        })
        .expect("build the `racli` binary first (e.g. `cargo bench --bench search`)")
}

/// Resolves racli's own repo root, used as the benchmarked workspace.
pub fn racli_workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Polls until `path` exists or `timeout` elapses.
fn wait_for_socket_path(path: &Path, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for Unix socket at {}", path.display());
}

/// Blocks on a throwaway current-thread Tokio runtime, retrying the async `probe` every 100ms
/// until it returns `true`, panicking with `message` if `timeout` elapses first.
pub fn poll_until<F, Fut>(timeout: Duration, mut probe: F, message: &str)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime for readiness probe");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if rt.block_on(probe()) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("{message}");
}

/// Sends SIGTERM on Unix (graceful for `racli server`), waits, then SIGKILL if still running.
fn shutdown_server_process(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        if pid > 0 {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Spawns `racli server` on a temp socket rooted at `workspace`, then waits until it is ready.
pub struct RacliServer {
    child: Child,
    _tmpdir: TempDir,
    socket: PathBuf,
}

impl RacliServer {
    /// Starts the server child with `RACLI_UNIX_SOCKET` set to a file inside a temp directory,
    /// then calls `ready` with the socket path to block until the caller's own RPCs succeed.
    pub fn start(workspace: &Path, ready: impl FnOnce(&Path)) -> Self {
        let target_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&target_dir).expect("create target/ for bench sockets");
        let tmpdir = TempDir::new_in(&target_dir).expect("temp directory for Unix socket");
        let socket = tmpdir.path().join("racli.sock");
        let mut cmd = Command::new(racli_executable());
        cmd.arg("server")
            .env("RACLI_UNIX_SOCKET", &socket)
            .current_dir(workspace)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn().expect("spawn `racli server`");
        let server = RacliServer {
            child,
            _tmpdir: tmpdir,
            socket,
        };
        wait_for_socket_path(&server.socket, Duration::from_secs(30));
        ready(&server.socket);
        server
    }

    /// Filesystem path passed to the `racli` CLI via `RACLI_UNIX_SOCKET`.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for RacliServer {
    /// Stops the server with SIGTERM (Unix) so tonic and rust-analyzer shut down cleanly.
    fn drop(&mut self) {
        shutdown_server_process(&mut self.child);
    }
}

/// Starts a [`RacliMcp`] only when the `RACLI_BENCH_MCP` environment variable is set to a
/// non-empty value other than `0` or `false`; the MCP benches are off by default.
pub fn start_mcp_if_enabled(workspace: &Path) -> Option<RacliMcp> {
    let enabled = std::env::var("RACLI_BENCH_MCP")
        .is_ok_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"));
    if !enabled {
        eprintln!("skipping the MCP benches; set RACLI_BENCH_MCP=1 to run them");
    }
    enabled.then(|| RacliMcp::start(workspace))
}

/// A `racli mcp` session rooted at a workspace, kept open across calls, as an MCP host keeps it.
pub struct RacliMcp {
    rt: tokio::runtime::Runtime,
    service: Option<RunningService<RoleClient, ()>>,
}

impl RacliMcp {
    /// Spawns `racli mcp` with `workspace` as its working directory, completes the MCP handshake,
    /// then waits until `get_version`'s `_meta` says rust-analyzer is ready (ADR 006).
    pub fn start(workspace: &Path) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime for racli mcp");
        let command = tokio::process::Command::new(racli_executable()).configure(|cmd| {
            cmd.arg("mcp").current_dir(workspace);
        });
        let service = rt.block_on(async {
            let (transport, _stderr) = TokioChildProcess::builder(command)
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn `racli mcp`");
            tokio::time::timeout(Duration::from_secs(30), ().serve(transport))
                .await
                .expect("`racli mcp` didn't finish the MCP handshake in time")
                .expect("`racli mcp` MCP handshake")
        });
        let mcp = RacliMcp {
            rt,
            service: Some(service),
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let result = mcp
                .call_tool("get_version", Value::Null)
                .expect("racli mcp get_version");
            if result["_meta"]["racli/backend"] == "ready" {
                return mcp;
            }
            assert!(
                Instant::now() < deadline,
                "racli mcp never reported `racli/backend: ready` in time"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Calls the tool `name` with `arguments` (a JSON object, or `null` for none) and returns the
    /// result as JSON, or the protocol error as text.
    fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, String> {
        let peer = self.service.as_ref().expect("racli mcp is running").peer();
        let mut params = CallToolRequestParams::new(name.to_string());
        if let Value::Object(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        let result = self
            .rt
            .block_on(peer.call_tool(params))
            .map_err(|e| e.to_string())?;
        serde_json::to_value(result).map_err(|e| e.to_string())
    }

    /// Calls the tool `name` with `arguments` and returns its first text block, or `None` if the
    /// call failed or the result has `isError`.
    pub fn try_call(&self, name: &str, arguments: Value) -> Option<String> {
        let result = self.call_tool(name, arguments).ok()?;
        if result["isError"] == true {
            return None;
        }
        result["content"][0]["text"].as_str().map(str::to_string)
    }

    /// Calls the tool `name` with `arguments` and returns its first text block, panicking if the
    /// call failed or the result has `isError`, so a stale query fails loudly.
    pub fn call(&self, name: &str, arguments: Value) -> String {
        let result = self
            .call_tool(name, arguments.clone())
            .unwrap_or_else(|e| panic!("racli mcp {name} {arguments} failed: {e}"));
        assert!(
            result["isError"] != true,
            "racli mcp {name} {arguments} returned isError: {}",
            result["content"]
        );
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// Calls the tool `name` with `arguments` every 100ms until its first text block satisfies
    /// `done`, panicking with `message` after 60 seconds.
    pub fn wait_for(
        &self,
        name: &str,
        arguments: Value,
        done: impl Fn(&str) -> bool,
        message: &str,
    ) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if self
                .try_call(name, arguments.clone())
                .is_some_and(|text| done(&text))
            {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("{message}");
    }
}

impl Drop for RacliMcp {
    /// Closes the MCP session so `racli mcp` shuts rust-analyzer down cleanly.
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            let _ = self.rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(30), service.cancel()).await
            });
        }
    }
}

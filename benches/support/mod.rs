//! Shared helpers for spawning and tearing down a background `racli server` in benchmarks.

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

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

//! Compares plain `grep` against `racli search` for substring queries (workspace: this crate).

use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::black_box;
use tempfile::TempDir;

/// Substrings exercised against the repo (`.rs` tree for `grep`, LSP symbols for `racli`).
const QUERIES: &[&str] = &["racli", "test", "symbol", "search", "lsp"];

/// Resolves the `racli` binary built by Cargo, or `target/{release,debug}/racli` under the manifest.
fn racli_executable() -> PathBuf {
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

/// Blocks until `racli::client::search` succeeds against `sock` (indexes rust-analyzer in the background).
fn wait_for_search_ready(sock: &Path) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime for readiness probe");
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        let probe = rt.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(10),
                racli::client::search(sock, "racli"),
            )
            .await
        });
        if matches!(probe, Ok(Ok(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("racli server did not accept search RPCs in time");
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

/// Spawns `racli server` on a temp socket under `workspace`, then waits until search works.
struct RacliServer {
    child: Child,
    _tmpdir: TempDir,
    socket: PathBuf,
}

impl RacliServer {
    /// Starts the server child with `RACLI_UNIX_SOCKET` set to a file inside a temp directory.
    fn start(workspace: &Path) -> Self {
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
        wait_for_search_ready(server.socket());
        server
    }

    /// Filesystem path passed to `racli search` via `RACLI_UNIX_SOCKET`.
    fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for RacliServer {
    /// Stops the server with SIGTERM (Unix) so tonic and rust-analyzer shut down cleanly.
    fn drop(&mut self) {
        shutdown_server_process(&mut self.child);
    }
}

/// Runs `grep -r` with a fixed substring over `*.rs` under `root` (discards output).
fn grep_rs_substring(root: &Path, query: &str) {
    let status = Command::new("grep")
        .arg("-r")
        .arg("-n")
        .arg("-F")
        .arg("--include=*.rs")
        .arg(query)
        .arg(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn grep");
    assert!(
        status.success() || status.code() == Some(1),
        "grep failed with {status:?}"
    );
}

/// Runs `racli search` for `query` against `socket` (discards output).
fn racli_search_cli(racli: &Path, socket: &Path, query: &str) {
    let status = Command::new(racli)
        .arg("search")
        .arg(query)
        .env("RACLI_UNIX_SOCKET", socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn racli search");
    assert!(status.success(), "racli search failed with {status:?}");
}

/// Registers Criterion benches comparing `grep` and `racli search` per query string.
fn bench_grep_vs_racli(c: &mut Criterion, racli: &Path, socket: &Path, workspace: &Path) {
    let mut group = c.benchmark_group("grep_vs_racli_search");
    for query in QUERIES {
        group.bench_function(BenchmarkId::new("grep", *query), |b| {
            b.iter(|| {
                grep_rs_substring(black_box(workspace), black_box(query));
            });
        });
        group.bench_function(BenchmarkId::new("racli_search", *query), |b| {
            b.iter(|| {
                racli_search_cli(black_box(racli), black_box(socket), black_box(query));
            });
        });
    }
    group.finish();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("bench `search` requires Unix (racli uses Unix domain sockets)");
}

#[cfg(unix)]
fn main() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let racli = racli_executable();
    let server = RacliServer::start(workspace.as_path());
    let socket = server.socket().to_path_buf();

    let mut criterion = Criterion::default()
        .measurement_time(Duration::from_secs(30))
        .configure_from_args();
    bench_grep_vs_racli(&mut criterion, &racli, &socket, &workspace);
    criterion.final_summary();
}

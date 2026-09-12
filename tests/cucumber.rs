//! Cucumber end-to-end tests: spawns a `racli server` against `fixtures/queue` once,
//! runs every scenario under `features/` against the compiled `racli` binary, then stops it.

use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::ExitCode;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use cucumber::World as _;
use cucumber::gherkin::Step;
use cucumber::then;
use cucumber::when;
use cucumber::writer::Stats;
use jsonpath_rust::JsonPath;
use serde_json::Value;
use tempfile::TempDir;

/// Unix socket of the shared `racli server`, set once in `main` before any scenario runs.
static SOCKET_PATH: OnceLock<PathBuf> = OnceLock::new();

fn socket_path() -> &'static Path {
    SOCKET_PATH
        .get()
        .expect("SOCKET_PATH not set: racli server was not started")
}

/// Captured result of a `racli` invocation, populated by the `When` step and read by `Then` steps.
#[derive(Debug, Default)]
struct CommandOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Default, cucumber::World)]
pub struct RacliWorld {
    command_output: Option<CommandOutput>,
}

#[when(regex = "^the following command is run:$")]
async fn the_following_command_is_run(world: &mut RacliWorld, step: &Step) {
    let docstring = step.docstring.as_ref().expect("expected a docstring");
    let command_line = docstring.trim();
    let mut parts = command_line.split_whitespace();
    let program = parts.next().expect("expected a command name");
    assert_eq!(
        program, "racli",
        "only `racli` commands are supported by this step"
    );
    let args: Vec<&str> = parts.collect();

    let output = Command::new(env!("CARGO_BIN_EXE_racli"))
        .args(&args)
        .env("RACLI_UNIX_SOCKET", socket_path())
        .output()
        .unwrap_or_else(|e| panic!("failed to execute `racli {}`: {e}", args.join(" ")));

    world.command_output = Some(CommandOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    });
}

#[then(expr = "it should exit with status code {int}")]
async fn it_should_exit_with_status_code(world: &mut RacliWorld, expected: i32) {
    let output = world
        .command_output
        .as_ref()
        .expect("no command has been run");
    assert_eq!(
        output.exit_code, expected,
        "expected exit code {expected}, got {}.\nstdout: {}\nstderr: {}",
        output.exit_code, output.stdout, output.stderr
    );
}

#[then(expr = "the output should contain {string}")]
async fn the_output_should_contain(world: &mut RacliWorld, expected: String) {
    let output = world
        .command_output
        .as_ref()
        .expect("no command has been run");
    assert!(
        output.stdout.contains(&expected),
        "expected stdout to contain {expected:?}.\nstdout: {}\nstderr: {}",
        output.stdout,
        output.stderr
    );
}

#[then(regex = "^the output should be$")]
async fn the_output_should_be(world: &mut RacliWorld, step: &Step) {
    let expected = step.docstring.as_ref().expect("expected a docstring");
    let output = world
        .command_output
        .as_ref()
        .expect("no command has been run");
    assert_eq!(
        output.stdout.trim(),
        expected.trim(),
        "stdout mismatch.\nstderr: {}",
        output.stderr
    );
}

fn stdout_as_json(output: &CommandOutput) -> Value {
    serde_json::from_str(&output.stdout)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\nstdout: {}", output.stdout))
}

#[then(expr = "the JSON output should match JSONPath {string}")]
async fn json_output_should_match(world: &mut RacliWorld, path: String) {
    let output = world
        .command_output
        .as_ref()
        .expect("no command has been run");
    let value = stdout_as_json(output);
    let matches = value
        .query(&path)
        .unwrap_or_else(|e| panic!("invalid JSONPath {path:?}: {e:?}"));
    assert!(
        !matches.is_empty(),
        "expected JSONPath {path:?} to match at least one value.\nstdout: {}",
        output.stdout
    );
}

#[then(expr = "the JSON output should match JSONPath {string} with a value ending with {string}")]
async fn json_output_should_match_ending_with(
    world: &mut RacliWorld,
    path: String,
    suffix: String,
) {
    let output = world
        .command_output
        .as_ref()
        .expect("no command has been run");
    let value = stdout_as_json(output);
    let matches = value
        .query(&path)
        .unwrap_or_else(|e| panic!("invalid JSONPath {path:?}: {e:?}"));
    let found = matches
        .iter()
        .any(|v| v.as_str().is_some_and(|s| s.ends_with(&suffix)));
    assert!(
        found,
        "expected JSONPath {path:?} to yield a value ending with {suffix:?}, got {matches:?}.\nstdout: {}",
        output.stdout
    );
}

/// `true` if `rust-analyzer --version` runs successfully (same skip guard as `tests/mcp_stdio.rs`).
fn rust_analyzer_available() -> bool {
    Command::new("rust-analyzer")
        .arg("--version")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn queue_workspace_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("queue")
}

/// Spawns `racli server` on a temp socket rooted at `workspace`, then waits until it's ready.
///
/// Mirrors `benches/search.rs`'s `RacliServer`, adapted to the already-async harness `main`.
struct RacliServer {
    child: Child,
    _tmpdir: TempDir,
    socket: PathBuf,
}

impl RacliServer {
    async fn start(workspace: &Path) -> Self {
        let target_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&target_dir).expect("create target/ for test sockets");
        let tmpdir = TempDir::new_in(&target_dir).expect("temp directory for Unix socket");
        let socket = tmpdir.path().join("racli.sock");

        let child = Command::new(env!("CARGO_BIN_EXE_racli"))
            .arg("server")
            .env("RACLI_UNIX_SOCKET", &socket)
            .current_dir(workspace)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn `racli server`");

        let server = RacliServer {
            child,
            _tmpdir: tmpdir,
            socket,
        };
        server.wait_for_socket(Duration::from_secs(30)).await;
        server.wait_for_search_ready(Duration::from_secs(120)).await;
        server
            .wait_for_find_definition_ready(workspace, Duration::from_secs(60))
            .await;
        server
    }

    async fn wait_for_socket(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.socket.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "timed out waiting for Unix socket at {}",
            self.socket.display()
        );
    }

    /// Blocks until `racli::client::search` for `mkfifo` returns a non-empty result (rust-analyzer
    /// has actually finished indexing `fixtures/queue`, not merely accepted the RPC).
    async fn wait_for_search_ready(&self, timeout: Duration) {
        use racli::proto::racli::lsp_workspace_symbol_response::Payload;

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let probe = tokio::time::timeout(
                Duration::from_secs(10),
                racli::client::search(&self.socket, "mkfifo"),
            )
            .await;
            let found = match probe {
                Ok(Ok(resp)) => match resp.workspace_symbol_response.and_then(|ws| ws.payload) {
                    Some(Payload::Flat(list)) => !list.items.is_empty(),
                    Some(Payload::Nested(list)) => !list.items.is_empty(),
                    None => false,
                },
                _ => false,
            };
            if found {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("racli server did not index `fixtures/queue` (no `mkfifo` symbol found) in time");
    }

    /// Blocks until `racli::client::find_definition` for the known `mkfifo` call site succeeds.
    ///
    /// Workspace symbol search (above) can come back non-empty before rust-analyzer is fully
    /// settled for per-file analysis, which briefly surfaces a transient LSP "content modified"
    /// error on `textDocument/definition`/`references`; warming that path up here avoids flaking.
    async fn wait_for_find_definition_ready(&self, workspace: &Path, timeout: Duration) {
        let main_rs = workspace
            .join("src")
            .join("main.rs")
            .canonicalize()
            .expect("canonicalize fixtures/queue/src/main.rs")
            .display()
            .to_string();

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let probe = tokio::time::timeout(
                Duration::from_secs(10),
                racli::client::find_definition(&self.socket, &main_rs, 80, 21),
            )
            .await;
            if matches!(probe, Ok(Ok(resp)) if !resp.locations.is_empty()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("racli server did not resolve the `mkfifo` definition in time");
    }
}

impl Drop for RacliServer {
    /// Sends `SIGTERM` (Unix) so tonic and rust-analyzer shut down cleanly, then escalates.
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let pid = self.child.id();
            if pid > 0 {
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }
        let deadline = Instant::now() + Duration::from_secs(45);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    if !rust_analyzer_available() {
        eprintln!("skip: rust-analyzer not on PATH or --version failed");
        return ExitCode::SUCCESS;
    }

    let server = RacliServer::start(&queue_workspace_dir()).await;
    SOCKET_PATH
        .set(server.socket.clone())
        .expect("SOCKET_PATH set exactly once");

    let writer = RacliWorld::cucumber().run("features").await;

    if writer.execution_has_failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

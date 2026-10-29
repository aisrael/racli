//! Cucumber end-to-end tests: spawns a `racli server` against `fixtures/queue` once,
//! runs every scenario under `features/` against the compiled `racli` binary, then stops it.
//! MCP scenarios share one `racli mcp`, also in `fixtures/queue`, started by the first of them.

use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::ExitCode;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use cucumber::World as _;
use cucumber::gherkin::Step;
use cucumber::then;
use cucumber::when;
use cucumber::writer::Stats;
use jsonpath_rust::JsonPath;
use rmcp::RoleClient;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResult;
use rmcp::service::Peer;
use rmcp::service::RunningService;
use rmcp::transport::ConfigureCommandExt;
use rmcp::transport::TokioChildProcess;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::OnceCell;

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

/// An MCP tool result, populated by the MCP `When` steps: whether it reported `isError`, and its
/// `_meta`. Its text blocks go in `command_output`.
#[derive(Debug)]
struct McpCall {
    is_error: bool,
    meta: Value,
}

#[derive(Debug, Default, cucumber::World)]
pub struct RacliWorld {
    command_output: Option<CommandOutput>,
    mcp_call: Option<McpCall>,
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

#[then(expr = "the error output should contain {string}")]
async fn the_error_output_should_contain(world: &mut RacliWorld, expected: String) {
    let output = world
        .command_output
        .as_ref()
        .expect("no command has been run");
    assert!(
        output.stderr.contains(&expected),
        "expected stderr to contain {expected:?}.\nstdout: {}\nstderr: {}",
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

#[then(expr = "the JSON output should not match JSONPath {string}")]
async fn json_output_should_not_match(world: &mut RacliWorld, path: String) {
    let output = world
        .command_output
        .as_ref()
        .expect("no command has been run");
    let value = stdout_as_json(output);
    let matches = value
        .query(&path)
        .unwrap_or_else(|e| panic!("invalid JSONPath {path:?}: {e:?}"));
    assert!(
        matches.is_empty(),
        "expected JSONPath {path:?} to match nothing, got {matches:?}.\nstdout: {}",
        output.stdout
    );
}

#[then(expr = "the JSON output should be an array of {int} item(s)")]
async fn json_output_should_be_an_array_of(world: &mut RacliWorld, expected: usize) {
    let output = world
        .command_output
        .as_ref()
        .expect("no command has been run");
    let value = stdout_as_json(output);
    let items = value
        .as_array()
        .unwrap_or_else(|| panic!("expected a JSON array.\nstdout: {}", output.stdout));
    assert_eq!(
        items.len(),
        expected,
        "expected {expected} item(s).\nstdout: {}",
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

/// The shared `racli mcp` session's peer, or why it didn't start, set by the first MCP step.
static MCP_PEER: OnceCell<Result<Peer<RoleClient>, String>> = OnceCell::const_new();

/// The shared `racli mcp` session itself, kept so `main` can shut it down after the run.
static MCP_SERVICE: Mutex<Option<RunningService<RoleClient, ()>>> = Mutex::new(None);

/// The shared `racli mcp` session's peer, starting it on first use. A session that failed to
/// start fails every MCP step with the same message, without trying again.
async fn mcp_peer() -> Peer<RoleClient> {
    MCP_PEER
        .get_or_init(start_mcp)
        .await
        .clone()
        .unwrap_or_else(|e| panic!("{e}"))
}

/// Spawns `racli mcp` with `fixtures/queue` as its working directory, then waits until it's ready.
async fn start_mcp() -> Result<Peer<RoleClient>, String> {
    let command = tokio::process::Command::new(env!("CARGO_BIN_EXE_racli")).configure(|cmd| {
        cmd.arg("mcp").current_dir(queue_workspace_dir());
    });
    let (transport, _stderr) = TokioChildProcess::builder(command)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn `racli mcp`: {e}"))?;
    let service = tokio::time::timeout(Duration::from_secs(30), ().serve(transport))
        .await
        .map_err(|_| "`racli mcp` didn't finish the MCP handshake in time".to_string())?
        .map_err(|e| format!("`racli mcp` MCP handshake failed: {e}"))?;
    let peer = service.peer().clone();
    *MCP_SERVICE.lock().expect("MCP_SERVICE lock") = Some(service);
    wait_for_mcp_ready(&peer, Duration::from_secs(120)).await?;
    Ok(peer)
}

/// Polls `get_version` until its `_meta` says rust-analyzer is `ready` (ADR 006), then
/// `find_definition` at the `mkfifo` call site until it resolves, as `RacliServer` does.
async fn wait_for_mcp_ready(peer: &Peer<RoleClient>, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let result = peer
            .call_tool(CallToolRequestParams::new("get_version"))
            .await
            .map_err(|e| format!("`racli mcp` get_version failed: {e}"))?;
        let backend = result
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get("racli/backend"))
            .and_then(Value::as_str);
        if backend == Some("ready") {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "`racli mcp` never reported `racli/backend: ready` in get_version's `_meta` (last: {backend:?})"
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let arguments = json!({"path": "src/main.rs", "line": 81, "character": 22});
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    loop {
        let params =
            CallToolRequestParams::new("find_definition").with_arguments(arguments.clone());
        if let Ok(result) = peer.call_tool(params).await
            && result.is_error != Some(true)
            && tool_texts(&result)
                .first()
                .is_some_and(|t| t.contains("sys.rs"))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("`racli mcp` did not resolve the `mkfifo` definition in time".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The text of each text content block in `result`, in order.
fn tool_texts(result: &CallToolResult) -> Vec<String> {
    let value = serde_json::to_value(result).expect("serialize CallToolResult");
    value["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str().map(str::to_string))
        .collect()
}

/// Calls the MCP tool `name` with `arguments`. The first text block becomes the output, as a
/// command's stdout, and the rest the error output, as its stderr (ADR 006 decision 1).
async fn call_mcp_tool(
    world: &mut RacliWorld,
    name: String,
    arguments: Option<serde_json::Map<String, Value>>,
) {
    let peer = mcp_peer().await;
    let mut params = CallToolRequestParams::new(name.clone());
    if let Some(arguments) = arguments {
        params = params.with_arguments(arguments);
    }
    let result = tokio::time::timeout(Duration::from_secs(60), peer.call_tool(params))
        .await
        .unwrap_or_else(|_| panic!("MCP tool {name} timed out after 60 seconds"))
        .unwrap_or_else(|e| panic!("MCP tool {name} failed: {e}"));
    let texts = tool_texts(&result);
    let (stdout, stderr) = match texts.split_first() {
        Some((first, rest)) => (first.clone(), rest.join("\n")),
        None => (String::new(), String::new()),
    };
    world.command_output = Some(CommandOutput {
        exit_code: 0,
        stdout,
        stderr,
    });
    world.mcp_call = Some(McpCall {
        is_error: result.is_error == Some(true),
        meta: result
            .meta
            .map(|meta| Value::Object(meta.0))
            .unwrap_or(Value::Null),
    });
}

#[when(regex = r#"^the MCP tool "([^"]+)" is called$"#)]
async fn the_mcp_tool_is_called(world: &mut RacliWorld, name: String) {
    call_mcp_tool(world, name, None).await;
}

#[when(regex = r#"^the MCP tool "([^"]+)" is called with:$"#)]
async fn the_mcp_tool_is_called_with(world: &mut RacliWorld, name: String, step: &Step) {
    let docstring = step.docstring.as_ref().expect("expected a docstring");
    let arguments = match serde_json::from_str(docstring.trim()) {
        Ok(Value::Object(arguments)) => arguments,
        other => panic!("expected a JSON object of arguments, got {other:?}"),
    };
    call_mcp_tool(world, name, Some(arguments)).await;
}

/// The outcome of the last MCP tool call.
fn mcp_call(world: &RacliWorld) -> &McpCall {
    world
        .mcp_call
        .as_ref()
        .expect("no MCP tool has been called")
}

#[then(expr = "the tool result should not be an error")]
async fn the_tool_result_should_not_be_an_error(world: &mut RacliWorld) {
    match mcp_call(world) {
        McpCall {
            is_error: false, ..
        } => {}
        other => panic!(
            "expected a successful tool result, got {other:?}\n{:?}",
            world.command_output
        ),
    }
}

#[then(expr = "the tool result should be an error")]
async fn the_tool_result_should_be_an_error(world: &mut RacliWorld) {
    match mcp_call(world) {
        McpCall { is_error: true, .. } => {}
        other => panic!(
            "expected a tool result with isError, got {other:?}\n{:?}",
            world.command_output
        ),
    }
}

#[then(expr = "the tool result's _meta should have {string} set to {string}")]
async fn the_tool_result_meta_should_have(world: &mut RacliWorld, key: String, expected: String) {
    match mcp_call(world) {
        McpCall { meta, .. } if meta[&key] == Value::String(expected.clone()) => {}
        other => panic!("expected _meta {key:?} to be {expected:?}, got {other:?}"),
    }
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
                racli::client::Client::new(&self.socket)
                    .unwrap()
                    .search("mkfifo"),
            )
            .await;
            let found = match probe {
                Ok(Ok(resp)) => match resp
                    .value
                    .workspace_symbol_response
                    .and_then(|ws| ws.payload)
                {
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
                racli::client::Client::new(&self.socket)
                    .unwrap()
                    .find_definition(&main_rs, 80, 21),
            )
            .await;
            if matches!(probe, Ok(Ok(resp)) if !resp.value.locations.is_empty()) {
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

    let mcp = MCP_SERVICE.lock().expect("MCP_SERVICE lock").take();
    if let Some(mcp) = mcp {
        let _ = tokio::time::timeout(Duration::from_secs(30), mcp.cancel()).await;
    }

    if writer.execution_has_failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

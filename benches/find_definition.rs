//! Compares a naive `grep`-based approximation of go-to-definition against `racli find-definition`
//! (the CLI calling a `racli server`) and the `racli mcp` `find_definition` tool, each from a
//! position and by symbol name, with and without `--hover`.

mod support;

use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use criterion::BenchmarkId;
use criterion::Criterion;
use racli::backend_status::BackendStatus;
use serde_json::json;

/// A `(file, 0-based line, 0-based UTF-16 character, bare symbol name)` case exercised by both baselines.
struct DefinitionQuery {
    /// Path to the file holding the reference, relative to the fixture workspace root.
    file: &'static str,
    /// 0-based LSP line of the reference passed to `racli find-definition`.
    line: u32,
    /// 0-based UTF-16 LSP character of the reference passed to `racli find-definition`.
    character: u32,
    /// Bare identifier used to build the `grep` pattern and the benchmark label.
    symbol: &'static str,
}

/// Reference sites in racli's own `src/` tree, one per most-frequent symbol, each resolving to
/// that symbol's declaration elsewhere in the crate.
const QUERIES: &[DefinitionQuery] = &[
    DefinitionQuery {
        file: "src/server.rs",
        line: 36,
        character: 17,
        symbol: "RustAnalyzerSession",
    },
    DefinitionQuery {
        file: "src/grpc_server.rs",
        line: 118,
        character: 17,
        symbol: "RacliCore",
    },
    DefinitionQuery {
        file: "src/racli_backend.rs",
        line: 59,
        character: 22,
        symbol: "Client",
    },
    DefinitionQuery {
        file: "src/cli.rs",
        line: 106,
        character: 23,
        symbol: "client_unix_socket_path",
    },
    DefinitionQuery {
        file: "src/rust_analyzer.rs",
        line: 338,
        character: 35,
        symbol: "LspError",
    },
];

/// Approximates "go to definition of `symbol`" by grepping for its `fn`/`struct`/`enum`
/// declaration under `root`.
///
/// This is a naive textual stand-in for real go-to-definition, not a correctness-equivalent
/// implementation: it matches any of those three declaration forms under `src/` regardless of
/// module or `impl` block, would misfire on overloaded/shadowed names, and does not handle
/// multi-line signatures. The benchmark compares raw speed, not whether the two approaches agree
/// on the resulting location.
fn grep_definition_approx(root: &Path, symbol: &str) {
    let status = Command::new("grep")
        .arg("-r")
        .arg("-n")
        .arg("-F")
        .arg("--include=*.rs")
        .arg("-e")
        .arg(format!("fn {symbol}"))
        .arg("-e")
        .arg(format!("struct {symbol}"))
        .arg("-e")
        .arg(format!("enum {symbol}"))
        .arg(root.join("src"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn grep");
    assert!(
        status.success() || status.code() == Some(1),
        "grep failed with {status:?}"
    );
}

/// Runs `racli find-definition` for `(path, line, character)` against `socket`, with `--hover` if
/// `hover` (discards output).
fn racli_find_definition_cli(
    racli: &Path,
    socket: &Path,
    path: &Path,
    line: u32,
    character: u32,
    hover: bool,
) {
    let mut command = Command::new(racli);
    command
        .arg("find-definition")
        .arg("--path")
        .arg(path)
        // The CLI takes 1-based positions; `line` and `character` are LSP's 0-based ones.
        .arg("--line")
        .arg((line + 1).to_string())
        .arg("--character")
        .arg((character + 1).to_string())
        .arg("--text");
    if hover {
        command.arg("--hover");
    }
    let status = command
        .env("RACLI_UNIX_SOCKET", socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn racli find-definition");
    assert!(
        status.success(),
        "racli find-definition failed with {status:?}"
    );
}

/// Runs `racli find-definition <name>` against `socket`, with `--hover` if `hover` (discards
/// output).
fn racli_find_definition_by_name_cli(racli: &Path, socket: &Path, name: &str, hover: bool) {
    let mut command = Command::new(racli);
    command.arg("find-definition").arg(name).arg("--text");
    if hover {
        command.arg("--hover");
    }
    let status = command
        .env("RACLI_UNIX_SOCKET", socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn racli find-definition");
    assert!(
        status.success(),
        "racli find-definition {name} failed with {status:?}"
    );
}

/// Calls the MCP `find_definition` tool for `(path, line, character)`, with `hover` (discards
/// output).
fn mcp_find_definition(
    mcp: &support::RacliMcp,
    path: &Path,
    line: u32,
    character: u32,
    hover: bool,
) {
    // The tool takes 1-based positions; `line` and `character` are LSP's 0-based ones.
    mcp.call(
        "find_definition",
        json!({"path": path, "line": line + 1, "character": character + 1, "hover": hover}),
    );
}

/// Calls the MCP `find_definition` tool for the symbol `name`, with `hover` (discards output).
fn mcp_find_definition_by_name(mcp: &support::RacliMcp, name: &str, hover: bool) {
    mcp.call("find_definition", json!({"names": [name], "hover": hover}));
}

/// Registers Criterion benches comparing the grep approximation with `racli find-definition` and
/// the MCP `find_definition` tool, from a position and by name, with and without hover.
fn bench_grep_vs_racli_find_definition(
    c: &mut Criterion,
    racli: &Path,
    socket: &Path,
    mcp: Option<&support::RacliMcp>,
    workspace: &Path,
) {
    let mut group = c.benchmark_group("grep_vs_racli_find_definition");
    for query in QUERIES {
        let abs_path = workspace
            .join(query.file)
            .canonicalize()
            .expect("canonicalize query file");
        group.bench_function(BenchmarkId::new("grep", query.symbol), |b| {
            b.iter(|| {
                grep_definition_approx(black_box(workspace), black_box(query.symbol));
            });
        });
        group.bench_function(
            BenchmarkId::new("racli_find_definition", query.symbol),
            |b| {
                b.iter(|| {
                    racli_find_definition_cli(
                        black_box(racli),
                        black_box(socket),
                        black_box(&abs_path),
                        black_box(query.line),
                        black_box(query.character),
                        false,
                    );
                });
            },
        );
        group.bench_function(
            BenchmarkId::new("racli_find_definition_hover", query.symbol),
            |b| {
                b.iter(|| {
                    racli_find_definition_cli(
                        black_box(racli),
                        black_box(socket),
                        black_box(&abs_path),
                        black_box(query.line),
                        black_box(query.character),
                        true,
                    );
                });
            },
        );
        for hover in [false, true] {
            let suffix = if hover { "_hover" } else { "" };
            group.bench_function(
                BenchmarkId::new(
                    format!("racli_find_definition_by_name{suffix}"),
                    query.symbol,
                ),
                |b| {
                    b.iter(|| {
                        racli_find_definition_by_name_cli(
                            black_box(racli),
                            black_box(socket),
                            black_box(query.symbol),
                            hover,
                        );
                    });
                },
            );
            if let Some(mcp) = mcp {
                group.bench_function(
                    BenchmarkId::new(format!("mcp_find_definition{suffix}"), query.symbol),
                    |b| {
                        b.iter(|| {
                            mcp_find_definition(
                                black_box(mcp),
                                black_box(&abs_path),
                                black_box(query.line),
                                black_box(query.character),
                                hover,
                            );
                        });
                    },
                );
            }
            if let Some(mcp) = mcp {
                group.bench_function(
                    BenchmarkId::new(format!("mcp_find_definition_by_name{suffix}"), query.symbol),
                    |b| {
                        b.iter(|| {
                            mcp_find_definition_by_name(
                                black_box(mcp),
                                black_box(query.symbol),
                                hover,
                            );
                        });
                    },
                );
            }
        }
    }
    group.finish();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("bench `find_definition` requires Unix (racli uses Unix domain sockets)");
}

#[cfg(unix)]
fn main() {
    let workspace = support::racli_workspace();
    let racli = support::racli_executable();
    let server = support::RacliServer::start(&workspace, |sock| {
        support::poll_until(
            Duration::from_secs(120),
            || async {
                matches!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        racli::client::Client::new(sock)
                            .unwrap()
                            .search("RustAnalyzerSession")
                    )
                    .await,
                    Ok(Ok(_))
                )
            },
            "racli server did not accept search RPCs in time",
        );

        let first = &QUERIES[0];
        let first_file = workspace
            .join(first.file)
            .canonicalize()
            .expect("canonicalize the first query's file")
            .display()
            .to_string();
        support::poll_until(
            Duration::from_secs(60),
            || async {
                matches!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        racli::client::Client::new(sock).unwrap().find_definition(&first_file, first.line, first.character),
                    )
                    .await,
                    Ok(Ok(resp)) if !resp.value.locations.is_empty()
                )
            },
            "racli server did not resolve the first query's definition in time",
        );
        // Hovers taken while rust-analyzer is still indexing can come back empty, and faster.
        support::poll_until(
            Duration::from_secs(120),
            || async {
                matches!(
                    racli::client::Client::new(sock).unwrap().get_version().await,
                    Ok(reply) if reply.backend == BackendStatus::Ready
                )
            },
            "rust-analyzer did not finish loading the workspace in time",
        );
    });
    let socket = server.socket().to_path_buf();
    // `RacliMcp::start` waits for rust-analyzer to be ready, so hovers aren't taken while indexing.
    let mcp = support::start_mcp_if_enabled(&workspace);
    if let Some(mcp) = &mcp {
        mcp.wait_for(
            "find_definition",
            json!({"names": [QUERIES[0].symbol], "hover": true}),
            |text| text.lines().count() > 2,
            "racli mcp did not resolve the first query's definition in time",
        );
    }

    let mut criterion = Criterion::default()
        .measurement_time(Duration::from_secs(60))
        .configure_from_args();
    bench_grep_vs_racli_find_definition(&mut criterion, &racli, &socket, mcp.as_ref(), &workspace);
    criterion.final_summary();
}

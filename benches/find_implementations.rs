//! Compares a naive `grep`-based approximation of find-implementations against
//! `racli find-implementations` (the CLI calling a `racli server`) and the `racli mcp`
//! `find_implementations` tool, each from a position and by symbol name.

mod support;

use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use criterion::BenchmarkId;
use criterion::Criterion;
use serde_json::json;

/// A `(file, 0-based line, 0-based UTF-16 character, bare symbol name)` case exercised by both baselines.
struct ImplementationQuery {
    /// Path to the file holding the symbol's declaration, relative to the fixture workspace root.
    file: &'static str,
    /// 0-based LSP line of the declaration passed to `racli find-implementations`.
    line: u32,
    /// 0-based UTF-16 LSP character of the declaration passed to `racli find-implementations`.
    character: u32,
    /// Bare identifier used to build the `grep` pattern and the benchmark label.
    symbol: &'static str,
}

/// Trait/struct declaration sites in racli's own `src/` tree, each with at least one `impl` block.
const QUERIES: &[ImplementationQuery] = &[
    ImplementationQuery {
        file: "src/call_hierarchy.rs",
        line: 85,
        character: 10,
        symbol: "CallHierarchyBackend",
    },
    ImplementationQuery {
        file: "src/rust_analyzer.rs",
        line: 355,
        character: 11,
        symbol: "RustAnalyzerSession",
    },
    ImplementationQuery {
        file: "src/racli_session.rs",
        line: 124,
        character: 11,
        symbol: "RacliSession",
    },
    ImplementationQuery {
        file: "src/client.rs",
        line: 111,
        character: 11,
        symbol: "Client",
    },
];

/// Approximates "find all implementations of `symbol`" by grepping for `impl <symbol>` occurrences
/// under `root/src`.
///
/// This is a naive textual stand-in for real find-implementations, not a correctness-equivalent
/// implementation: it matches any occurrence of the literal text (comments, strings, unrelated
/// shadowed bindings) rather than only true `impl` blocks, and does not resolve the declaration
/// itself. The benchmark compares raw speed, not whether the two approaches agree on the
/// resulting locations.
fn grep_implementations_approx(root: &Path, symbol: &str) {
    let pattern = format!("impl {symbol}");
    let status = Command::new("grep")
        .arg("-r")
        .arg("-n")
        .arg("-F")
        .arg("--include=*.rs")
        .arg(&pattern)
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

/// Runs `racli find-implementations` for `(path, line, character)` against `socket` (discards output).
fn racli_find_implementations_cli(
    racli: &Path,
    socket: &Path,
    path: &Path,
    line: u32,
    character: u32,
) {
    let status = Command::new(racli)
        .arg("find-implementations")
        .arg("--path")
        .arg(path)
        // The CLI takes 1-based positions; `line` and `character` are LSP's 0-based ones.
        .arg("--line")
        .arg((line + 1).to_string())
        .arg("--character")
        .arg((character + 1).to_string())
        .arg("--text")
        .env("RACLI_UNIX_SOCKET", socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn racli find-implementations");
    assert!(
        status.success(),
        "racli find-implementations failed with {status:?}"
    );
}

/// Runs `racli find-implementations <name>` against `socket` (discards output).
fn racli_find_implementations_by_name_cli(racli: &Path, socket: &Path, name: &str) {
    let status = Command::new(racli)
        .arg("find-implementations")
        .arg(name)
        .arg("--text")
        .env("RACLI_UNIX_SOCKET", socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn racli find-implementations");
    assert!(
        status.success(),
        "racli find-implementations {name} failed with {status:?}"
    );
}

/// Calls the MCP `find_implementations` tool for `(path, line, character)` (discards output).
fn mcp_find_implementations(mcp: &support::RacliMcp, path: &Path, line: u32, character: u32) {
    // The tool takes 1-based positions; `line` and `character` are LSP's 0-based ones.
    mcp.call(
        "find_implementations",
        json!({"path": path, "line": line + 1, "character": character + 1}),
    );
}

/// Calls the MCP `find_implementations` tool for the symbol `name` (discards output).
fn mcp_find_implementations_by_name(mcp: &support::RacliMcp, name: &str) {
    mcp.call("find_implementations", json!({"names": [name]}));
}

/// Registers Criterion benches comparing the grep approximation with `racli find-implementations` and the MCP
/// `find_implementations` tool, from a position and by name.
fn bench_grep_vs_racli_find_implementations(
    c: &mut Criterion,
    racli: &Path,
    socket: &Path,
    mcp: &support::RacliMcp,
    workspace: &Path,
) {
    let mut group = c.benchmark_group("grep_vs_racli_find_implementations");
    for query in QUERIES {
        let abs_path = workspace
            .join(query.file)
            .canonicalize()
            .expect("canonicalize query file");
        group.bench_function(BenchmarkId::new("grep", query.symbol), |b| {
            b.iter(|| {
                grep_implementations_approx(black_box(workspace), black_box(query.symbol));
            });
        });
        group.bench_function(
            BenchmarkId::new("racli_find_implementations", query.symbol),
            |b| {
                b.iter(|| {
                    racli_find_implementations_cli(
                        black_box(racli),
                        black_box(socket),
                        black_box(&abs_path),
                        black_box(query.line),
                        black_box(query.character),
                    );
                });
            },
        );
        group.bench_function(
            BenchmarkId::new("racli_find_implementations_by_name", query.symbol),
            |b| {
                b.iter(|| {
                    racli_find_implementations_by_name_cli(
                        black_box(racli),
                        black_box(socket),
                        black_box(query.symbol),
                    );
                });
            },
        );
        group.bench_function(
            BenchmarkId::new("mcp_find_implementations", query.symbol),
            |b| {
                b.iter(|| {
                    mcp_find_implementations(
                        black_box(mcp),
                        black_box(&abs_path),
                        black_box(query.line),
                        black_box(query.character),
                    );
                });
            },
        );
        group.bench_function(
            BenchmarkId::new("mcp_find_implementations_by_name", query.symbol),
            |b| {
                b.iter(|| {
                    mcp_find_implementations_by_name(black_box(mcp), black_box(query.symbol));
                });
            },
        );
    }
    group.finish();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("bench `find_implementations` requires Unix (racli uses Unix domain sockets)");
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
                        racli::client::Client::new(sock).unwrap().find_implementations(&first_file, first.line, first.character),
                    )
                    .await,
                    Ok(Ok(resp)) if !resp.value.locations.is_empty()
                )
            },
            "racli server did not answer the first query in time",
        );
    });
    let socket = server.socket().to_path_buf();
    let mcp = support::RacliMcp::start(&workspace);
    mcp.wait_for(
        "find_implementations",
        json!({"names": [QUERIES[0].symbol]}),
        |text| text.lines().count() > 1,
        "racli mcp did not answer the first query in time",
    );

    let mut criterion = Criterion::default()
        .measurement_time(Duration::from_secs(60))
        .configure_from_args();
    bench_grep_vs_racli_find_implementations(&mut criterion, &racli, &socket, &mcp, &workspace);
    criterion.final_summary();
}

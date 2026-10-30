//! Compares a naive `grep`-based approximation of call-hierarchy incoming calls against `racli
//! call-hierarchy --direction incoming` (the CLI calling a `racli server`) and the `racli mcp`
//! `call_hierarchy` tool, each from a position and by symbol name.

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
struct CallHierarchyQuery {
    /// Path to the file holding the symbol's declaration, relative to the fixture workspace root.
    file: &'static str,
    /// 0-based LSP line of the declaration passed to `racli call-hierarchy`.
    line: u32,
    /// 0-based UTF-16 LSP character of the declaration passed to `racli call-hierarchy`.
    character: u32,
    /// Bare identifier used to build the `grep` pattern and the benchmark label.
    symbol: &'static str,
}

/// Declaration sites in racli's own `src/` tree, one per most-frequent symbol, each with multiple
/// call sites elsewhere in the crate.
const QUERIES: &[CallHierarchyQuery] = &[
    CallHierarchyQuery {
        file: "src/rust_analyzer.rs",
        line: 871,
        character: 7,
        symbol: "document_uri_from_path",
    },
    CallHierarchyQuery {
        file: "src/lsp_map.rs",
        line: 36,
        character: 7,
        symbol: "workspace_symbol_response_to_proto",
    },
];

/// Approximates "callers of `symbol`" by grepping for its literal occurrences under `root/src`.
///
/// This is a naive textual stand-in for real call-hierarchy, not a correctness-equivalent
/// implementation: it matches any occurrence of the identifier (comments, strings, unrelated
/// shadowed bindings) rather than only true call sites, and does not resolve the declaration
/// itself. The benchmark compares raw speed, not whether the two approaches agree on the
/// resulting locations.
fn grep_callers_approx(root: &Path, symbol: &str) {
    let status = Command::new("grep")
        .arg("-r")
        .arg("-n")
        .arg("-F")
        .arg("--include=*.rs")
        .arg(symbol)
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

/// Runs `racli call-hierarchy --direction incoming` for `(path, line, character)` against `socket` (discards output).
fn racli_call_hierarchy_cli(racli: &Path, socket: &Path, path: &Path, line: u32, character: u32) {
    let status = Command::new(racli)
        .arg("call-hierarchy")
        .arg("--path")
        .arg(path)
        // The CLI takes 1-based positions; `line` and `character` are LSP's 0-based ones.
        .arg("--line")
        .arg((line + 1).to_string())
        .arg("--character")
        .arg((character + 1).to_string())
        .arg("--direction")
        .arg("incoming")
        .arg("--text")
        .env("RACLI_UNIX_SOCKET", socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn racli call-hierarchy");
    assert!(
        status.success(),
        "racli call-hierarchy failed with {status:?}"
    );
}

/// Runs `racli call-hierarchy <name> --direction incoming` against `socket` (discards output).
fn racli_call_hierarchy_by_name_cli(racli: &Path, socket: &Path, name: &str) {
    let status = Command::new(racli)
        .arg("call-hierarchy")
        .arg(name)
        .arg("--direction")
        .arg("incoming")
        .arg("--text")
        .env("RACLI_UNIX_SOCKET", socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn racli call-hierarchy");
    assert!(
        status.success(),
        "racli call-hierarchy {name} failed with {status:?}"
    );
}

/// Calls the MCP `call_hierarchy` tool for `(path, line, character)` (discards output).
fn mcp_call_hierarchy(mcp: &support::RacliMcp, path: &Path, line: u32, character: u32) {
    // The tool takes 1-based positions; `line` and `character` are LSP's 0-based ones.
    mcp.call(
        "call_hierarchy",
        json!({"path": path, "line": line + 1, "character": character + 1, "direction": "incoming"}),
    );
}

/// Calls the MCP `call_hierarchy` tool for the symbol `name` (discards output).
fn mcp_call_hierarchy_by_name(mcp: &support::RacliMcp, name: &str) {
    mcp.call(
        "call_hierarchy",
        json!({"names": [name], "direction": "incoming"}),
    );
}

/// Registers Criterion benches comparing the grep approximation with `racli call-hierarchy` and the MCP
/// `call_hierarchy` tool, from a position and by name.
fn bench_grep_vs_racli_call_hierarchy(
    c: &mut Criterion,
    racli: &Path,
    socket: &Path,
    mcp: Option<&support::RacliMcp>,
    workspace: &Path,
) {
    let mut group = c.benchmark_group("grep_vs_racli_call_hierarchy");
    for query in QUERIES {
        let abs_path = workspace
            .join(query.file)
            .canonicalize()
            .expect("canonicalize query file");
        group.bench_function(BenchmarkId::new("grep", query.symbol), |b| {
            b.iter(|| {
                grep_callers_approx(black_box(workspace), black_box(query.symbol));
            });
        });
        group.bench_function(
            BenchmarkId::new("racli_call_hierarchy", query.symbol),
            |b| {
                b.iter(|| {
                    racli_call_hierarchy_cli(
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
            BenchmarkId::new("racli_call_hierarchy_by_name", query.symbol),
            |b| {
                b.iter(|| {
                    racli_call_hierarchy_by_name_cli(
                        black_box(racli),
                        black_box(socket),
                        black_box(query.symbol),
                    );
                });
            },
        );
        if let Some(mcp) = mcp {
            group.bench_function(BenchmarkId::new("mcp_call_hierarchy", query.symbol), |b| {
                b.iter(|| {
                    mcp_call_hierarchy(
                        black_box(mcp),
                        black_box(&abs_path),
                        black_box(query.line),
                        black_box(query.character),
                    );
                });
            });
        }
        if let Some(mcp) = mcp {
            group.bench_function(
                BenchmarkId::new("mcp_call_hierarchy_by_name", query.symbol),
                |b| {
                    b.iter(|| {
                        mcp_call_hierarchy_by_name(black_box(mcp), black_box(query.symbol));
                    });
                },
            );
        }
    }
    group.finish();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("bench `call_hierarchy` requires Unix (racli uses Unix domain sockets)");
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
                            .search("document_uri_from_path")
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
                        racli::client::Client::new(sock).unwrap().prepare_call_hierarchy(&first_file, first.line, first.character),
                    )
                    .await,
                    Ok(Ok(resp)) if !resp.value.items.is_empty()
                )
            },
            "racli server did not answer the first query in time",
        );
    });
    let socket = server.socket().to_path_buf();
    let mcp = support::start_mcp_if_enabled(&workspace);
    if let Some(mcp) = &mcp {
        mcp.wait_for(
            "call_hierarchy",
            json!({"names": [QUERIES[0].symbol], "direction": "incoming"}),
            |text| text.contains("<- callers:") && text.lines().count() > 2,
            "racli mcp did not answer the first query in time",
        );
    }

    let mut criterion = Criterion::default()
        .measurement_time(Duration::from_secs(60))
        .configure_from_args();
    bench_grep_vs_racli_call_hierarchy(&mut criterion, &racli, &socket, mcp.as_ref(), &workspace);
    criterion.final_summary();
}

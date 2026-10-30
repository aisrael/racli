//! Compares a naive `grep`-based approximation of a file outline against `racli document-symbols`
//! (the CLI calling a `racli server`) and the `racli mcp` `document_symbols` tool.

mod support;

use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use criterion::BenchmarkId;
use criterion::Criterion;
use serde_json::json;

/// Files in racli's own `src/` tree to outline, relative to the workspace root: some of its
/// largest, with many nested symbols.
const QUERIES: &[&str] = &[
    "src/rust_analyzer.rs",
    "src/symbol.rs",
    "src/call_hierarchy.rs",
];

/// Approximates "outline `file`" by grepping it for lines that start a declaration.
///
/// This is a naive textual stand-in for a real outline, not a correctness-equivalent
/// implementation: it doesn't nest symbols, misses fields, variants and macro-generated items, and
/// matches declarations inside comments or strings. The benchmark compares raw speed, not whether
/// the two approaches agree on the resulting symbols.
fn grep_outline_approx(file: &Path) {
    let status = Command::new("grep")
        .arg("-n")
        .arg("-E")
        .arg(r"^\s*(pub(\(crate\))? )?(async )?(fn|struct|enum|trait|impl|mod|const|type) ")
        .arg(file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn grep");
    assert!(
        status.success() || status.code() == Some(1),
        "grep failed with {status:?}"
    );
}

/// Runs `racli document-symbols <file> --text` against `socket` (discards output).
fn racli_document_symbols_cli(racli: &Path, socket: &Path, file: &Path) {
    let status = Command::new(racli)
        .arg("document-symbols")
        .arg(file)
        .arg("--text")
        .env("RACLI_UNIX_SOCKET", socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn racli document-symbols");
    assert!(
        status.success(),
        "racli document-symbols failed with {status:?}"
    );
}

/// Calls the MCP `document_symbols` tool for `file` (discards output).
fn mcp_document_symbols(mcp: &support::RacliMcp, file: &Path) {
    mcp.call("document_symbols", json!({"path": file}));
}

/// Registers Criterion benches comparing the grep approximation with `racli document-symbols` and
/// the MCP `document_symbols` tool.
fn bench_grep_vs_racli_document_symbols(
    c: &mut Criterion,
    racli: &Path,
    socket: &Path,
    mcp: Option<&support::RacliMcp>,
    workspace: &Path,
) {
    let mut group = c.benchmark_group("grep_vs_racli_document_symbols");
    for file in QUERIES {
        let abs_path = workspace
            .join(file)
            .canonicalize()
            .expect("canonicalize query file");
        group.bench_function(BenchmarkId::new("grep", file), |b| {
            b.iter(|| {
                grep_outline_approx(black_box(&abs_path));
            });
        });
        group.bench_function(BenchmarkId::new("racli_document_symbols", file), |b| {
            b.iter(|| {
                racli_document_symbols_cli(
                    black_box(racli),
                    black_box(socket),
                    black_box(&abs_path),
                );
            });
        });
        if let Some(mcp) = mcp {
            group.bench_function(BenchmarkId::new("mcp_document_symbols", file), |b| {
                b.iter(|| {
                    mcp_document_symbols(black_box(mcp), black_box(&abs_path));
                });
            });
        }
    }
    group.finish();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("bench `document_symbols` requires Unix (racli uses Unix domain sockets)");
}

#[cfg(unix)]
fn main() {
    let workspace = support::racli_workspace();
    let racli = support::racli_executable();
    let first_file = workspace
        .join(QUERIES[0])
        .canonicalize()
        .expect("canonicalize the first query's file")
        .display()
        .to_string();
    let server = support::RacliServer::start(&workspace, |sock| {
        support::poll_until(
            Duration::from_secs(120),
            || async {
                matches!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        racli::client::Client::new(sock).unwrap().document_symbols(&first_file),
                    )
                    .await,
                    Ok(Ok(resp)) if !resp.value.symbols.is_empty()
                )
            },
            "racli server did not outline the first query's file in time",
        );
    });
    let socket = server.socket().to_path_buf();
    let mcp = support::start_mcp_if_enabled(&workspace);
    if let Some(mcp) = &mcp {
        mcp.wait_for(
            "document_symbols",
            json!({"path": first_file}),
            |text| text.lines().count() > 1,
            "racli mcp did not outline the first query's file in time",
        );
    }

    let mut criterion = Criterion::default()
        .measurement_time(Duration::from_secs(60))
        .configure_from_args();
    bench_grep_vs_racli_document_symbols(&mut criterion, &racli, &socket, mcp.as_ref(), &workspace);
    criterion.final_summary();
}

//! Compares a naive `grep`-based approximation of go-to-definition against `racli find-definition`.

mod support;

use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use criterion::BenchmarkId;
use criterion::Criterion;

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
        file: "src/racli_session.rs",
        line: 38,
        character: 29,
        symbol: "RustAnalyzerSession",
    },
    DefinitionQuery {
        file: "src/grpc_server.rs",
        line: 121,
        character: 17,
        symbol: "RacliSession",
    },
    DefinitionQuery {
        file: "src/racli_session.rs",
        line: 36,
        character: 10,
        symbol: "Core",
    },
    DefinitionQuery {
        file: "src/cli.rs",
        line: 68,
        character: 45,
        symbol: "effective_unix_socket_path",
    },
    DefinitionQuery {
        file: "src/rust_analyzer.rs",
        line: 88,
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

/// Runs `racli find-definition` for `(path, line, character)` against `socket` (discards output).
fn racli_find_definition_cli(racli: &Path, socket: &Path, path: &Path, line: u32, character: u32) {
    let status = Command::new(racli)
        .arg("find-definition")
        .arg(path)
        .arg("--line")
        .arg(line.to_string())
        .arg("--character")
        .arg(character.to_string())
        .arg("--text")
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

/// Registers Criterion benches comparing the grep approximation and real `racli find-definition`.
fn bench_grep_vs_racli_find_definition(
    c: &mut Criterion,
    racli: &Path,
    socket: &Path,
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
                    );
                });
            },
        );
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
                        racli::client::search(sock, "RustAnalyzerSession")
                    )
                    .await,
                    Ok(Ok(_))
                )
            },
            "racli server did not accept search RPCs in time",
        );

        let racli_session_rs = workspace
            .join("src")
            .join("racli_session.rs")
            .canonicalize()
            .expect("canonicalize src/racli_session.rs")
            .display()
            .to_string();
        support::poll_until(
            Duration::from_secs(60),
            || async {
                matches!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        racli::client::find_definition(sock, &racli_session_rs, 38, 29),
                    )
                    .await,
                    Ok(Ok(resp)) if !resp.locations.is_empty()
                )
            },
            "racli server did not resolve the `RustAnalyzerSession` definition in time",
        );
    });
    let socket = server.socket().to_path_buf();

    let mut criterion = Criterion::default()
        .measurement_time(Duration::from_secs(60))
        .configure_from_args();
    bench_grep_vs_racli_find_definition(&mut criterion, &racli, &socket, &workspace);
    criterion.final_summary();
}

//! Compares plain `grep` against `racli search` for substring queries (workspace: racli's own source tree).

mod support;

use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use criterion::BenchmarkId;
use criterion::Criterion;

/// The most frequent real symbols in racli's own `src/` tree (`.rs` tree for `grep`, LSP symbols for `racli`).
const QUERIES: &[&str] = &[
    "RustAnalyzerSession",
    "RacliSession",
    "Core",
    "effective_unix_socket_path",
    "LspError",
];

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
    });
    let socket = server.socket().to_path_buf();

    let mut criterion = Criterion::default()
        .measurement_time(Duration::from_secs(60))
        .configure_from_args();
    bench_grep_vs_racli(&mut criterion, &racli, &socket, &workspace);
    criterion.final_summary();
}

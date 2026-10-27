//! `racli find-implementations`: CLI arguments and formatting for LSP go-to-implementation results.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use serde::Serialize;

use crate::client;
use crate::proto::racli::FindImplementationsResponse;
use crate::symbol;
use crate::symbol::SymbolArgs;
use crate::symbol::SymbolResultJson;
use crate::symbol::print_symbol_results_json;
use crate::utils::client_unix_socket_path;

/// Arguments for `racli find-implementations` (LSP `textDocument/implementation`).
#[derive(Parser)]
pub struct FindImplementationsArgs {
    /// Symbol names to look up, e.g. `mkfifo`, `sys::mkfifo` or `Fifo::name`.
    #[arg(value_name = "NAME", required_unless_present = "path")]
    pub names: Vec<String>,
    /// Rust source file of a position to run from, with `--line` and `--character` (absolute or
    /// relative to the current directory).
    #[arg(long, value_name = "FILE", requires_all = ["line", "character"])]
    pub path: Option<PathBuf>,
    /// 0-based line of the position in `--path` (LSP `Position.line`).
    #[arg(long)]
    pub line: Option<u32>,
    /// 0-based UTF-16 character offset on the line (LSP `Position.character`).
    #[arg(long)]
    pub character: Option<u32>,
    #[command(flatten)]
    pub symbol: SymbolArgs,
    /// Print one human-readable line per location instead of JSON.
    #[arg(long)]
    pub text: bool,
}

/// Runs the find-implementations RPC from each definition the symbol names find and from the
/// `--path` position, if given, and prints locations as JSON or plain text. On a trait or type, returns its `impl`
/// blocks; on a trait method, returns the per-`impl` overrides; invoked from inside an `impl` block
/// itself, this typically returns nothing.
pub async fn run_cli_find_implementations(args: FindImplementationsArgs) {
    let sock = client_unix_socket_path();
    let sock_display = sock.display().to_string();

    let specs = symbol::classify(
        &args.names,
        args.path.as_deref(),
        args.line,
        args.character,
        &args.symbol,
    )
    .unwrap_or_else(|e| symbol::exit_with_usage_error(&e));
    let targets = match tokio::time::timeout(
        Duration::from_secs(60),
        symbol::targets(&sock, &args.symbol, specs),
    )
    .await
    {
        Ok(Ok(targets)) => targets,
        Ok(Err(err)) => {
            eprintln!("racli find-implementations ({sock_display}): {err}");
            return;
        }
        Err(_elapsed) => {
            eprintln!(
                "racli find-implementations ({sock_display}): request timed out after 60 seconds"
            );
            return;
        }
    };
    for err in &targets.errors {
        eprintln!("racli find-implementations ({sock_display}): {err}");
    }
    let grouped = targets.grouped;

    let mut results = Vec::with_capacity(targets.targets.len());
    for target in targets.targets {
        match tokio::time::timeout(
            Duration::from_secs(60),
            client::find_implementations(&sock, &target.file_path, target.line, target.character),
        )
        .await
        {
            Ok(Ok(resp)) => results.push((target.symbol, resp)),
            Ok(Err(err)) => {
                eprintln!("racli find-implementations ({sock_display}): {err}");
                return;
            }
            Err(_elapsed) => {
                eprintln!(
                    "racli find-implementations ({sock_display}): request timed out after 60 seconds"
                );
                return;
            }
        }
    }

    if args.text {
        for (symbol, resp) in &results {
            match symbol {
                Some(symbol) => {
                    println!("{}", symbol.header());
                    print_find_implementations_text(resp, "  ");
                }
                None => print_find_implementations_text(resp, ""),
            }
        }
    } else if grouped {
        let grouped: Vec<_> = results
            .iter()
            .filter_map(|(symbol, resp)| {
                Some(SymbolResultJson {
                    symbol: symbol.as_ref()?,
                    result: find_implementations_json_rows(resp),
                })
            })
            .collect();
        print_symbol_results_json("find-implementations", &grouped);
    } else if let Some((_, resp)) = results.first() {
        print_find_implementations_json(resp);
    }
}

/// Prints a [`FindImplementationsResponse`] as JSON (array of `uri` + `range`).
fn print_find_implementations_json(resp: &FindImplementationsResponse) {
    let rows = find_implementations_json_rows(resp);
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = serde_json::to_writer(&mut stdout, &rows) {
        eprintln!("racli find-implementations: failed to serialize JSON: {e}");
        return;
    }
    let _ = writeln!(stdout);
}

/// The JSON rows for a [`FindImplementationsResponse`]: one `uri` + `range` per location.
fn find_implementations_json_rows(
    resp: &FindImplementationsResponse,
) -> Vec<ImplementationLocationJson> {
    resp.locations
        .iter()
        .map(|loc| ImplementationLocationJson {
            uri: loc.uri.clone(),
            range: proto_lsp_range_to_json(loc.range.as_ref().unwrap_or(&default_empty_range())),
        })
        .collect()
}

fn default_empty_range() -> crate::proto::racli::LspRange {
    crate::proto::racli::LspRange {
        start: None,
        end: None,
    }
}

/// Single implementation row in JSON output.
#[derive(Serialize)]
struct ImplementationLocationJson {
    uri: String,
    range: ImplementationRangeJson,
}

/// LSP range as JSON with `start` and `end` positions.
#[derive(Serialize)]
struct ImplementationRangeJson {
    start: ImplementationPositionJson,
    end: ImplementationPositionJson,
}

/// LSP zero-based line and UTF-16 character offset.
#[derive(Serialize)]
struct ImplementationPositionJson {
    line: u32,
    character: u32,
}

fn proto_lsp_range_to_json(r: &crate::proto::racli::LspRange) -> ImplementationRangeJson {
    let (sl, sc) = r
        .start
        .as_ref()
        .map(|p| (p.line, p.character))
        .unwrap_or((0, 0));
    let (el, ec) = r
        .end
        .as_ref()
        .map(|p| (p.line, p.character))
        .unwrap_or((0, 0));
    ImplementationRangeJson {
        start: ImplementationPositionJson {
            line: sl,
            character: sc,
        },
        end: ImplementationPositionJson {
            line: el,
            character: ec,
        },
    }
}

/// Prints locations as one line each, after `indent`: `uri` plus `startLine:startChar-endLine:endChar`.
fn print_find_implementations_text(resp: &FindImplementationsResponse, indent: &str) {
    if resp.locations.is_empty() {
        println!("{indent}(no implementations)");
        return;
    }
    for loc in &resp.locations {
        let range = loc
            .range
            .as_ref()
            .map(lsp_range_line)
            .filter(|s| !s.is_empty())
            .map(|s| format!(" {s}"))
            .unwrap_or_default();
        println!("{indent}{}{}", loc.uri, range);
    }
}

/// Formats an LSP range as `startLine:startChar-endLine:endChar` for plain-text output.
fn lsp_range_line(r: &crate::proto::racli::LspRange) -> String {
    let (sl, sc) = r
        .start
        .as_ref()
        .map(|p| (p.line, p.character))
        .unwrap_or((0, 0));
    let (el, ec) = r
        .end
        .as_ref()
        .map(|p| (p.line, p.character))
        .unwrap_or((0, 0));
    format!("{sl}:{sc}-{el}:{ec}")
}

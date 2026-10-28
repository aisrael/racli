//! `racli find-references`: CLI arguments and formatting for LSP find-references results.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use serde::Serialize;

use crate::client::Client;
use crate::client::Reply;
use crate::proto::racli::FindReferencesResponse;
use crate::symbol;
use crate::symbol::SymbolArgs;
use crate::symbol::SymbolResultJson;
use crate::symbol::print_symbol_results_json;
use crate::text_position::SourceLines;
use crate::text_position::parse_one_based;
use crate::utils::client_unix_socket_path;

/// Arguments for `racli find-references` (LSP `textDocument/references`, declaration included).
#[derive(Parser)]
pub struct FindReferencesArgs {
    /// Symbol names to look up, e.g. `mkfifo`, `sys::mkfifo` or `Fifo::name`.
    #[arg(value_name = "NAME", required_unless_present = "path")]
    pub names: Vec<String>,
    /// Rust source file of a position to run from, with `--line` and `--character` (absolute or
    /// relative to the current directory).
    #[arg(long, value_name = "FILE", requires_all = ["line", "character"])]
    pub path: Option<PathBuf>,
    /// 1-based line of the position in `--path`.
    #[arg(long, value_parser = parse_one_based)]
    pub line: Option<u32>,
    /// 1-based column of the position on the line, in UTF-16 units as LSP counts them.
    #[arg(long, value_parser = parse_one_based)]
    pub character: Option<u32>,
    #[command(flatten)]
    pub symbol: SymbolArgs,
    /// Print one human-readable line per location instead of JSON.
    #[arg(long)]
    pub text: bool,
}

/// Runs the find-references RPC from each definition the symbol names find and from the
/// `--path` position, if given, and prints locations as JSON or plain text.
pub async fn run_cli_find_references(args: FindReferencesArgs) {
    let sock = client_unix_socket_path();
    let sock_display = sock.display().to_string();
    let client = match Client::new(&sock) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("racli find-references ({sock_display}): {err}");
            return;
        }
    };

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
        symbol::targets(&client, &args.symbol, specs),
    )
    .await
    {
        Ok(Ok(targets)) => targets,
        Ok(Err(err)) => {
            eprintln!("racli find-references ({sock_display}): {err}");
            return;
        }
        Err(_elapsed) => {
            eprintln!("racli find-references ({sock_display}): request timed out after 60 seconds");
            return;
        }
    };
    for err in &targets.errors {
        eprintln!("racli find-references ({sock_display}): {err}");
    }
    let grouped = targets.grouped;

    let mut results = Vec::with_capacity(targets.targets.len());
    for target in targets.targets {
        match tokio::time::timeout(
            Duration::from_secs(60),
            client.find_references(&target.file_path, target.line, target.character),
        )
        .await
        {
            Ok(Ok(Reply { value: resp, .. })) => results.push((target.symbol, resp)),
            Ok(Err(err)) => {
                eprintln!("racli find-references ({sock_display}): {err}");
                return;
            }
            Err(_elapsed) => {
                eprintln!(
                    "racli find-references ({sock_display}): request timed out after 60 seconds"
                );
                return;
            }
        }
    }

    if args.text {
        let mut sources = SourceLines::default();
        for (symbol, resp) in &results {
            match symbol {
                Some(symbol) => {
                    println!("{}", symbol.header());
                    print_find_references_text(resp, "  ", &mut sources);
                }
                None => print_find_references_text(resp, "", &mut sources),
            }
        }
    } else if grouped {
        let grouped: Vec<_> = results
            .iter()
            .filter_map(|(symbol, resp)| {
                Some(SymbolResultJson {
                    symbol: symbol.as_ref()?,
                    result: find_references_json_rows(resp),
                })
            })
            .collect();
        print_symbol_results_json("find-references", &grouped);
    } else if let Some((_, resp)) = results.first() {
        print_find_references_json(resp);
    }
}

/// Prints a [`FindReferencesResponse`] as JSON (array of `uri` + `range`).
fn print_find_references_json(resp: &FindReferencesResponse) {
    let rows = find_references_json_rows(resp);
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = serde_json::to_writer(&mut stdout, &rows) {
        eprintln!("racli find-references: failed to serialize JSON: {e}");
        return;
    }
    let _ = writeln!(stdout);
}

/// The JSON rows for a [`FindReferencesResponse`]: one `uri` + `range` per location.
fn find_references_json_rows(resp: &FindReferencesResponse) -> Vec<ReferenceLocationJson> {
    resp.locations
        .iter()
        .map(|loc| ReferenceLocationJson {
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

/// Single reference row in JSON output.
#[derive(Serialize)]
struct ReferenceLocationJson {
    uri: String,
    range: ReferenceRangeJson,
}

/// LSP range as JSON with `start` and `end` positions.
#[derive(Serialize)]
struct ReferenceRangeJson {
    start: ReferencePositionJson,
    end: ReferencePositionJson,
}

/// LSP zero-based line and UTF-16 character offset.
#[derive(Serialize)]
struct ReferencePositionJson {
    line: u32,
    character: u32,
}

fn proto_lsp_range_to_json(r: &crate::proto::racli::LspRange) -> ReferenceRangeJson {
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
    ReferenceRangeJson {
        start: ReferencePositionJson {
            line: sl,
            character: sc,
        },
        end: ReferencePositionJson {
            line: el,
            character: ec,
        },
    }
}

/// Prints locations as one line each, after `indent`: `path:line:col: source line`, with the path
/// relative to the current directory and 1-based line and column.
fn print_find_references_text(
    resp: &FindReferencesResponse,
    indent: &str,
    sources: &mut SourceLines,
) {
    if resp.locations.is_empty() {
        println!("{indent}(no references)");
        return;
    }
    for loc in &resp.locations {
        let (line, character) = loc
            .range
            .as_ref()
            .and_then(|r| r.start.as_ref())
            .map(|p| (p.line, p.character))
            .unwrap_or((0, 0));
        println!("{indent}{}", sources.location(&loc.uri, line, character));
    }
}

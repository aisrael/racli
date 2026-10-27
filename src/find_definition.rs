//! `racli find-definition`: CLI arguments and formatting for LSP go-to-definition results.

use std::io::Write;
use std::time::Duration;

use clap::Parser;
use serde::Serialize;

use crate::client;
use crate::proto::racli::FindDefinitionResponse;
use crate::symbol;
use crate::symbol::SymbolArgs;
use crate::symbol::SymbolResultJson;
use crate::symbol::print_symbol_results_json;
use crate::utils::client_unix_socket_path;

/// Arguments for `racli find-definition` (LSP `textDocument/definition`).
#[derive(Parser)]
pub struct FindDefinitionArgs {
    /// Symbol names to look up (e.g. `mkfifo`, `sys::mkfifo`, `Fifo::name`), and/or one Rust
    /// source file for `--line` and `--character`. An argument that isn't a Rust name is a file.
    #[arg(required = true, value_name = "NAME|PATH")]
    pub targets: Vec<String>,
    /// 0-based line of the position in the file (LSP `Position.line`).
    #[arg(long, requires = "character")]
    pub line: Option<u32>,
    /// 0-based UTF-16 character offset on the line (LSP `Position.character`).
    #[arg(long, requires = "line")]
    pub character: Option<u32>,
    #[command(flatten)]
    pub symbol: SymbolArgs,
    /// Print one human-readable line per location instead of JSON.
    #[arg(long)]
    pub text: bool,
}

/// Runs the find-definition RPC from each definition the symbol names find and from the
/// position, if given, and prints locations as JSON or plain text.
pub async fn run_cli_find_definition(args: FindDefinitionArgs) {
    let sock = client_unix_socket_path();
    let sock_display = sock.display().to_string();

    let specs = symbol::classify(&args.targets, args.line, args.character, &args.symbol)
        .unwrap_or_else(|e| symbol::exit_with_usage_error(&e));
    let targets = match tokio::time::timeout(
        Duration::from_secs(60),
        symbol::targets(&sock, &args.symbol, specs),
    )
    .await
    {
        Ok(Ok(targets)) => targets,
        Ok(Err(err)) => {
            eprintln!("racli find-definition ({sock_display}): {err}");
            return;
        }
        Err(_elapsed) => {
            eprintln!("racli find-definition ({sock_display}): request timed out after 60 seconds");
            return;
        }
    };
    for err in &targets.errors {
        eprintln!("racli find-definition ({sock_display}): {err}");
    }
    let grouped = targets.grouped;

    let mut results = Vec::with_capacity(targets.targets.len());
    for target in targets.targets {
        match tokio::time::timeout(
            Duration::from_secs(60),
            client::find_definition(&sock, &target.file_path, target.line, target.character),
        )
        .await
        {
            Ok(Ok(resp)) => results.push((target.symbol, resp)),
            Ok(Err(err)) => {
                eprintln!("racli find-definition ({sock_display}): {err}");
                return;
            }
            Err(_elapsed) => {
                eprintln!(
                    "racli find-definition ({sock_display}): request timed out after 60 seconds"
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
                    print_find_definition_text(resp, "  ");
                }
                None => print_find_definition_text(resp, ""),
            }
        }
    } else if grouped {
        let grouped: Vec<_> = results
            .iter()
            .filter_map(|(symbol, resp)| {
                Some(SymbolResultJson {
                    symbol: symbol.as_ref()?,
                    result: find_definition_json_rows(resp),
                })
            })
            .collect();
        print_symbol_results_json("find-definition", &grouped);
    } else if let Some((_, resp)) = results.first() {
        print_find_definition_json(resp);
    }
}

/// Prints a [`FindDefinitionResponse`] as JSON (array of `uri` + `range`).
fn print_find_definition_json(resp: &FindDefinitionResponse) {
    let rows = find_definition_json_rows(resp);
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = serde_json::to_writer(&mut stdout, &rows) {
        eprintln!("racli find-definition: failed to serialize JSON: {e}");
        return;
    }
    let _ = writeln!(stdout);
}

/// The JSON rows for a [`FindDefinitionResponse`]: one `uri` + `range` per location.
fn find_definition_json_rows(resp: &FindDefinitionResponse) -> Vec<DefinitionLocationJson> {
    resp.locations
        .iter()
        .map(|loc| DefinitionLocationJson {
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

/// Single definition row in JSON output.
#[derive(Serialize)]
struct DefinitionLocationJson {
    uri: String,
    range: DefinitionRangeJson,
}

/// LSP range as JSON with `start` and `end` positions.
#[derive(Serialize)]
struct DefinitionRangeJson {
    start: DefinitionPositionJson,
    end: DefinitionPositionJson,
}

/// LSP zero-based line and UTF-16 character offset.
#[derive(Serialize)]
struct DefinitionPositionJson {
    line: u32,
    character: u32,
}

fn proto_lsp_range_to_json(r: &crate::proto::racli::LspRange) -> DefinitionRangeJson {
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
    DefinitionRangeJson {
        start: DefinitionPositionJson {
            line: sl,
            character: sc,
        },
        end: DefinitionPositionJson {
            line: el,
            character: ec,
        },
    }
}

/// Prints locations as one line each, after `indent`: `uri` plus `startLine:startChar-endLine:endChar`.
fn print_find_definition_text(resp: &FindDefinitionResponse, indent: &str) {
    if resp.locations.is_empty() {
        println!("{indent}(no definition)");
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

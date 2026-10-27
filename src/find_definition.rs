//! `racli find-definition`: CLI arguments and formatting for LSP go-to-definition results.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use serde::Serialize;

use crate::client;
use crate::proto::racli::FindDefinitionResponse;
use crate::symbol;
use crate::symbol::SymbolArgs;
use crate::symbol::SymbolResultJson;
use crate::symbol::print_symbol_results_json;
use crate::text_position::SourceLines;
use crate::text_position::parse_one_based;
use crate::utils::client_unix_socket_path;

/// Arguments for `racli find-definition` (LSP `textDocument/definition`).
#[derive(Parser)]
pub struct FindDefinitionArgs {
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

/// Runs the find-definition RPC from each definition the symbol names find and from the
/// `--path` position, if given, and prints locations as JSON or plain text.
pub async fn run_cli_find_definition(args: FindDefinitionArgs) {
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
        let mut sources = SourceLines::default();
        for (symbol, resp) in &results {
            match symbol {
                Some(symbol) => {
                    println!("{}", symbol.header());
                    print_find_definition_text(resp, "  ", &mut sources);
                }
                None => print_find_definition_text(resp, "", &mut sources),
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

/// Prints locations as one line each, after `indent`: `path:line:col: source line`, with the path
/// relative to the current directory and 1-based line and column.
fn print_find_definition_text(
    resp: &FindDefinitionResponse,
    indent: &str,
    sources: &mut SourceLines,
) {
    if resp.locations.is_empty() {
        println!("{indent}(no definition)");
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

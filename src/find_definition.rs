//! `racli find-definition`: CLI arguments and formatting for LSP go-to-definition results.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use serde::Serialize;

use crate::client::Client;
use crate::client::Reply;
use crate::client::client_unix_socket_path;
use crate::definition_details::DefinitionDetails;
use crate::definition_details::DefinitionSource;
use crate::definition_details::definition_details;
use crate::definition_details::hover_text;
use crate::definition_details::numbered_source;
use crate::proto::racli::FindDefinitionResponse;
use crate::symbol;
use crate::symbol::SymbolArgs;
use crate::symbol::SymbolResultJson;
use crate::symbol::print_symbol_results_json;
use crate::text_position::SourceLines;
use crate::text_position::parse_one_based;

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
    /// Also print each definition's signature and doc comment (LSP hover).
    #[arg(long)]
    pub hover: bool,
    /// Also print each definition's whole source, including its doc comment.
    #[arg(long)]
    pub source: bool,
    /// Print one human-readable line per location instead of JSON.
    #[arg(long)]
    pub text: bool,
}

/// Runs the find-definition RPC from each definition the symbol names find and from the
/// `--path` position, if given, and prints locations as JSON or plain text.
pub async fn run_cli_find_definition(args: FindDefinitionArgs) {
    let sock = client_unix_socket_path();
    let sock_display = sock.display().to_string();
    let client = match Client::new(&sock) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("racli find-definition ({sock_display}): {err}");
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

    let mut sources = SourceLines::default();
    let mut results = Vec::with_capacity(targets.targets.len());
    for target in targets.targets {
        match tokio::time::timeout(
            Duration::from_secs(60),
            client.find_definition(&target.file_path, target.line, target.character),
        )
        .await
        {
            Ok(Ok(Reply { value: resp, .. })) => {
                let mut details = Vec::new();
                if args.hover || args.source {
                    for loc in &resp.locations {
                        let (found, errors) =
                            definition_details(&client, loc, args.hover, args.source, &mut sources)
                                .await;
                        for err in errors {
                            eprintln!("racli find-definition ({sock_display}): {err}");
                        }
                        details.push(found);
                    }
                }
                results.push((target.symbol, resp, details));
            }
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
        for (symbol, resp, details) in &results {
            match symbol {
                Some(symbol) => {
                    println!("{}", symbol.header());
                    print_find_definition_text(resp, details, "  ", &mut sources);
                }
                None => print_find_definition_text(resp, details, "", &mut sources),
            }
        }
    } else if grouped {
        let grouped: Vec<_> = results
            .iter()
            .filter_map(|(symbol, resp, details)| {
                Some(SymbolResultJson {
                    symbol: symbol.as_ref()?,
                    result: find_definition_json_rows(resp, details),
                })
            })
            .collect();
        print_symbol_results_json("find-definition", &grouped);
    } else if let Some((_, resp, details)) = results.first() {
        print_find_definition_json(resp, details);
    }
}

/// Prints a [`FindDefinitionResponse`] as JSON (array of `uri` + `range`, plus any details).
fn print_find_definition_json(resp: &FindDefinitionResponse, details: &[DefinitionDetails]) {
    let rows = find_definition_json_rows(resp, details);
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = serde_json::to_writer(&mut stdout, &rows) {
        eprintln!("racli find-definition: failed to serialize JSON: {e}");
        return;
    }
    let _ = writeln!(stdout);
}

/// The JSON rows for a [`FindDefinitionResponse`]: one `uri` + `range` per location, with its
/// `hover` and `source` when `--hover` or `--source` fetched them.
fn find_definition_json_rows<'a>(
    resp: &FindDefinitionResponse,
    details: &'a [DefinitionDetails],
) -> Vec<DefinitionLocationJson<'a>> {
    resp.locations
        .iter()
        .enumerate()
        .map(|(i, loc)| {
            let detail = details.get(i);
            DefinitionLocationJson {
                uri: loc.uri.clone(),
                range: proto_lsp_range_to_json(
                    loc.range.as_ref().unwrap_or(&default_empty_range()),
                ),
                hover: detail.and_then(|d| d.hover.as_deref()),
                source: detail.and_then(|d| d.source.as_ref()),
            }
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
struct DefinitionLocationJson<'a> {
    uri: String,
    range: DefinitionRangeJson,
    /// rust-analyzer's hover as Markdown, with `--hover`.
    #[serde(skip_serializing_if = "Option::is_none")]
    hover: Option<&'a str>,
    /// The definition's whole source, with `--source`.
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'a DefinitionSource>,
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
/// relative to the current directory and 1-based line and column. A location's hover and source
/// follow it, indented two more spaces.
fn print_find_definition_text(
    resp: &FindDefinitionResponse,
    details: &[DefinitionDetails],
    indent: &str,
    sources: &mut SourceLines,
) {
    if resp.locations.is_empty() {
        println!("{indent}(no definition)");
        return;
    }
    for (i, loc) in resp.locations.iter().enumerate() {
        let (line, character) = loc
            .range
            .as_ref()
            .and_then(|r| r.start.as_ref())
            .map(|p| (p.line, p.character))
            .unwrap_or((0, 0));
        println!("{indent}{}", sources.location(&loc.uri, line, character));
        let Some(detail) = details.get(i) else {
            continue;
        };
        if let Some(hover) = &detail.hover {
            for line in hover_text(hover).lines() {
                if line.is_empty() {
                    println!();
                } else {
                    println!("{indent}  {line}");
                }
            }
        }
        if let Some(source) = &detail.source {
            for line in numbered_source(&source.text, source.range.start.line) {
                println!("{indent}  {line}");
            }
        }
    }
}

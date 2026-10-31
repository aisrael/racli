//! `racli find-references`: CLI arguments and formatting for LSP find-references results.

use std::path::PathBuf;

use clap::Parser;
use serde::Serialize;

use crate::client::Client;
use crate::client::client_unix_socket_path;
use crate::command_output::CommandError;
use crate::command_output::CommandOutput;
use crate::command_output::within_timeout;
use crate::proto::racli::FindReferencesResponse;
use crate::racli_backend::RacliBackend;
use crate::symbol;
use crate::symbol::SymbolArgs;
use crate::symbol::SymbolResultJson;
use crate::symbol::write_symbol_results_json;
use crate::text_position::SourceLines;
use crate::text_position::display_path;
use crate::text_position::parse_one_based;

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
    /// Print one human-readable line per location (the default).
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
    /// Print JSON instead of text.
    #[arg(long, conflicts_with = "text")]
    pub json: bool,
}

/// Runs the find-references RPC from each definition the symbol names find and from the
/// `--path` position, if given, and writes locations as JSON or plain text to `out`.
pub async fn run_find_references(
    backend: &dyn RacliBackend,
    args: &FindReferencesArgs,
    out: &mut CommandOutput,
) -> Result<(), CommandError> {
    let targets = symbol::command_targets(
        backend,
        &args.names,
        args.path.as_deref(),
        args.line,
        args.character,
        &args.symbol,
        out,
    )
    .await?;
    let grouped = targets.grouped;

    let mut results = Vec::with_capacity(targets.targets.len());
    for target in targets.targets {
        let resp = within_timeout(backend.find_references(
            &target.file_path,
            target.line,
            target.character,
        ))
        .await?
        .map_err(CommandError::Failed)?;
        out.mark_answered();
        results.push((target.symbol, resp));
    }

    if !args.json {
        let mut sources = SourceLines::default();
        for (symbol, resp) in &results {
            match symbol {
                Some(symbol) => {
                    out.println(symbol.header());
                    write_find_references_text(out, resp, "  ", &mut sources);
                }
                None => write_find_references_text(out, resp, "", &mut sources),
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
        write_symbol_results_json(out, &grouped);
    } else if let Some((_, resp)) = results.first() {
        write_find_references_json(out, resp);
    }
    Ok(())
}

/// Runs `racli find-references` against the project's server and prints what [`run_find_references`] writes.
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
    let mut out = CommandOutput::default();
    let result = run_find_references(&client, &args, &mut out).await;
    out.print_cli("find-references", &sock_display, result);
}

/// Writes a [`FindReferencesResponse`] as JSON (array of `uri` + `range`).
fn write_find_references_json(out: &mut CommandOutput, resp: &FindReferencesResponse) {
    match serde_json::to_string(&find_references_json_rows(resp)) {
        Ok(json) => out.println(json),
        Err(e) => out.note(format!("failed to serialize JSON: {e}")),
    }
}

/// The JSON rows for a [`FindReferencesResponse`]: one `path` + `range` per location.
fn find_references_json_rows(resp: &FindReferencesResponse) -> Vec<ReferenceLocationJson> {
    resp.locations
        .iter()
        .map(|loc| ReferenceLocationJson {
            path: display_path(&loc.uri),
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
    path: String,
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

/// Writes locations as one line each, after `indent`: `path:line:col: source line`, with the path
/// relative to the current directory and 1-based line and column.
fn write_find_references_text(
    out: &mut CommandOutput,
    resp: &FindReferencesResponse,
    indent: &str,
    sources: &mut SourceLines,
) {
    if resp.locations.is_empty() {
        out.println(format!("{indent}(no references)"));
        return;
    }
    for loc in &resp.locations {
        let (line, character) = loc
            .range
            .as_ref()
            .and_then(|r| r.start.as_ref())
            .map(|p| (p.line, p.character))
            .unwrap_or((0, 0));
        out.println(format!(
            "{indent}{}",
            sources.location(&loc.uri, line, character)
        ));
    }
}

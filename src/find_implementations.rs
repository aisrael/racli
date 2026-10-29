//! `racli find-implementations`: CLI arguments and formatting for LSP go-to-implementation results.

use std::path::PathBuf;

use clap::Parser;
use serde::Serialize;

use crate::client::Client;
use crate::client::client_unix_socket_path;
use crate::command_output::CommandError;
use crate::command_output::CommandOutput;
use crate::command_output::within_timeout;
use crate::proto::racli::FindImplementationsResponse;
use crate::racli_backend::RacliBackend;
use crate::symbol;
use crate::symbol::SymbolArgs;
use crate::symbol::SymbolResultJson;
use crate::symbol::write_symbol_results_json;
use crate::text_position::SourceLines;
use crate::text_position::parse_one_based;

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

/// Runs the find-implementations RPC from each definition the symbol names find and from the
/// `--path` position, if given, and writes locations as JSON or plain text to `out`. On a trait or
/// type, returns its `impl` blocks; on a trait method, returns the per-`impl` overrides; invoked
/// from inside an `impl` block itself, this typically returns nothing.
pub async fn run_find_implementations(
    backend: &dyn RacliBackend,
    args: &FindImplementationsArgs,
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
        let resp = within_timeout(backend.find_implementations(
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
                    write_find_implementations_text(out, resp, "  ", &mut sources);
                }
                None => write_find_implementations_text(out, resp, "", &mut sources),
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
        write_symbol_results_json(out, &grouped);
    } else if let Some((_, resp)) = results.first() {
        write_find_implementations_json(out, resp);
    }
    Ok(())
}

/// Runs `racli find-implementations` against the project's server and prints what [`run_find_implementations`] writes.
pub async fn run_cli_find_implementations(args: FindImplementationsArgs) {
    let sock = client_unix_socket_path();
    let sock_display = sock.display().to_string();
    let client = match Client::new(&sock) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("racli find-implementations ({sock_display}): {err}");
            return;
        }
    };
    let mut out = CommandOutput::default();
    let result = run_find_implementations(&client, &args, &mut out).await;
    out.print_cli("find-implementations", &sock_display, result);
}

/// Writes a [`FindImplementationsResponse`] as JSON (array of `uri` + `range`).
fn write_find_implementations_json(out: &mut CommandOutput, resp: &FindImplementationsResponse) {
    match serde_json::to_string(&find_implementations_json_rows(resp)) {
        Ok(json) => out.println(json),
        Err(e) => out.note(format!("failed to serialize JSON: {e}")),
    }
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

/// Writes locations as one line each, after `indent`: `path:line:col: source line`, with the path
/// relative to the current directory and 1-based line and column.
fn write_find_implementations_text(
    out: &mut CommandOutput,
    resp: &FindImplementationsResponse,
    indent: &str,
    sources: &mut SourceLines,
) {
    if resp.locations.is_empty() {
        out.println(format!("{indent}(no implementations)"));
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

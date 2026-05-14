//! `racli find-definition`: CLI arguments and formatting for LSP go-to-definition results.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use serde::Serialize;

use crate::client;
use crate::effective_unix_socket_path;
use crate::proto::racli::FindDefinitionResponse;

/// Arguments for `racli find-definition` (LSP `textDocument/definition`).
#[derive(Parser)]
pub struct FindDefinitionArgs {
    /// Rust source file (absolute or relative to the current directory).
    pub path: PathBuf,
    /// 0-based line (LSP `Position.line`).
    #[arg(long)]
    pub line: u32,
    /// 0-based UTF-16 character offset on the line (LSP `Position.character`).
    #[arg(long)]
    pub character: u32,
    /// Print one human-readable line per location instead of JSON.
    #[arg(long)]
    pub text: bool,
}

/// Runs the find-definition RPC and prints locations as JSON or plain text.
pub async fn run_cli_find_definition(args: FindDefinitionArgs) {
    let sock = effective_unix_socket_path();
    let sock_display = sock.display().to_string();

    let abs = match args.path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "racli find-definition: cannot canonicalize {}: {e}",
                args.path.display()
            );
            return;
        }
    };

    let file_path = abs.display().to_string();

    match tokio::time::timeout(
        Duration::from_secs(60),
        client::find_definition(&sock, &file_path, args.line, args.character),
    )
    .await
    {
        Ok(Ok(resp)) => {
            if args.text {
                print_find_definition_text(&resp);
            } else {
                print_find_definition_json(&resp);
            }
        }
        Ok(Err(err)) => {
            eprintln!("racli find-definition ({sock_display}): {err}");
        }
        Err(_elapsed) => {
            eprintln!("racli find-definition ({sock_display}): request timed out after 60 seconds");
        }
    }
}

/// Prints a [`FindDefinitionResponse`] as JSON (array of `uri` + `range`).
fn print_find_definition_json(resp: &FindDefinitionResponse) {
    let rows: Vec<DefinitionLocationJson> = resp
        .locations
        .iter()
        .map(|loc| DefinitionLocationJson {
            uri: loc.uri.clone(),
            range: proto_lsp_range_to_json(loc.range.as_ref().unwrap_or(&default_empty_range())),
        })
        .collect();
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = serde_json::to_writer_pretty(&mut stdout, &rows) {
        eprintln!("racli find-definition: failed to serialize JSON: {e}");
        return;
    }
    let _ = writeln!(stdout);
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

/// Prints locations as one line each: `uri` plus `startLine:startChar-endLine:endChar`.
fn print_find_definition_text(resp: &FindDefinitionResponse) {
    if resp.locations.is_empty() {
        println!("(no definition)");
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
        println!("{}{}", loc.uri, range);
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

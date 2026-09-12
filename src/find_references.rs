//! `racli find-references`: CLI arguments and formatting for LSP find-references results.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use serde::Serialize;

use crate::client;
use crate::effective_unix_socket_path;
use crate::proto::racli::FindReferencesResponse;

/// Arguments for `racli find-references` (LSP `textDocument/references`, declaration included).
#[derive(Parser)]
pub struct FindReferencesArgs {
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

/// Runs the find-references RPC and prints locations as JSON or plain text.
pub async fn run_cli_find_references(args: FindReferencesArgs) {
    let sock = effective_unix_socket_path();
    let sock_display = sock.display().to_string();

    let abs = match args.path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "racli find-references: cannot canonicalize {}: {e}",
                args.path.display()
            );
            return;
        }
    };

    let file_path = abs.display().to_string();

    match tokio::time::timeout(
        Duration::from_secs(60),
        client::find_references(&sock, &file_path, args.line, args.character),
    )
    .await
    {
        Ok(Ok(resp)) => {
            if args.text {
                print_find_references_text(&resp);
            } else {
                print_find_references_json(&resp);
            }
        }
        Ok(Err(err)) => {
            eprintln!("racli find-references ({sock_display}): {err}");
        }
        Err(_elapsed) => {
            eprintln!("racli find-references ({sock_display}): request timed out after 60 seconds");
        }
    }
}

/// Prints a [`FindReferencesResponse`] as JSON (array of `uri` + `range`).
fn print_find_references_json(resp: &FindReferencesResponse) {
    let rows: Vec<ReferenceLocationJson> = resp
        .locations
        .iter()
        .map(|loc| ReferenceLocationJson {
            uri: loc.uri.clone(),
            range: proto_lsp_range_to_json(loc.range.as_ref().unwrap_or(&default_empty_range())),
        })
        .collect();
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = serde_json::to_writer_pretty(&mut stdout, &rows) {
        eprintln!("racli find-references: failed to serialize JSON: {e}");
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

/// Prints locations as one line each: `uri` plus `startLine:startChar-endLine:endChar`.
fn print_find_references_text(resp: &FindReferencesResponse) {
    if resp.locations.is_empty() {
        println!("(no references)");
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

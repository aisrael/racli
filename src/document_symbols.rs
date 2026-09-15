//! `racli document-symbols`: CLI arguments and formatting for LSP document-symbol outline results.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use clap::ValueEnum;
use serde::Serialize;

use crate::client;
use crate::effective_unix_socket_path;
use crate::proto::racli::DocumentSymbolsResponse;
use crate::proto::racli::LspDocumentSymbol;
use crate::proto::racli::LspRange;

/// How `racli document-symbols` prints results (default is JSON).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum DocumentSymbolsOutputFormat {
    /// Indented tree, one symbol per line (legacy plain-text layout).
    Text,
    /// Hierarchical JSON array of symbol objects.
    Json,
}

/// Arguments for `racli document-symbols` (LSP `textDocument/documentSymbol`; file-scoped, no line/character).
#[derive(Parser)]
pub struct DocumentSymbolsArgs {
    /// Rust source file (absolute or relative to the current directory).
    pub path: PathBuf,
    /// Print an indented tree instead of JSON (equivalent to `--output-format text`).
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
    /// Print results as JSON (equivalent to `--output-format json`; same as the default).
    #[arg(long, conflicts_with = "text")]
    pub json: bool,
    /// Select how the outline is printed (default: json).
    #[arg(long, value_enum)]
    pub output_format: Option<DocumentSymbolsOutputFormat>,
}

/// Plain text or JSON after resolving CLI flags for `racli document-symbols`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DocumentSymbolsPrintKind {
    Text,
    Json,
}

impl DocumentSymbolsArgs {
    /// Picks plain text or JSON; explicit `--output-format` wins over `--text` / `--json`.
    fn print_kind(&self) -> DocumentSymbolsPrintKind {
        if let Some(fmt) = self.output_format {
            match fmt {
                DocumentSymbolsOutputFormat::Text => DocumentSymbolsPrintKind::Text,
                DocumentSymbolsOutputFormat::Json => DocumentSymbolsPrintKind::Json,
            }
        } else if self.json {
            DocumentSymbolsPrintKind::Json
        } else if self.text {
            DocumentSymbolsPrintKind::Text
        } else {
            DocumentSymbolsPrintKind::Json
        }
    }
}

/// Runs the document-symbols RPC and prints the outline in the format selected by `args`.
pub async fn run_cli_document_symbols(args: DocumentSymbolsArgs) {
    let sock = effective_unix_socket_path();
    let sock_display = sock.display().to_string();

    let abs = match args.path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "racli document-symbols: cannot canonicalize {}: {e}",
                args.path.display()
            );
            return;
        }
    };

    let file_path = abs.display().to_string();

    match tokio::time::timeout(
        Duration::from_secs(60),
        client::document_symbols(&sock, &file_path),
    )
    .await
    {
        Ok(Ok(resp)) => match args.print_kind() {
            DocumentSymbolsPrintKind::Text => print_document_symbols_text(&resp),
            DocumentSymbolsPrintKind::Json => print_document_symbols_json(&resp),
        },
        Ok(Err(err)) => {
            eprintln!("racli document-symbols ({sock_display}): {err}");
        }
        Err(_elapsed) => {
            eprintln!(
                "racli document-symbols ({sock_display}): request timed out after 60 seconds"
            );
        }
    }
}

/// One document symbol row in JSON output, recursively nesting `children`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentSymbolJson {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    kind: String,
    range: DocumentSymbolRangeJson,
    selection_range: DocumentSymbolRangeJson,
    children: Vec<DocumentSymbolJson>,
}

/// LSP range serialized as JSON with `start` and `end` positions.
#[derive(Serialize)]
struct DocumentSymbolRangeJson {
    start: DocumentSymbolPositionJson,
    end: DocumentSymbolPositionJson,
}

/// LSP zero-based line and UTF-16 character offset in JSON output.
#[derive(Serialize)]
struct DocumentSymbolPositionJson {
    line: u32,
    character: u32,
}

fn default_empty_range() -> LspRange {
    LspRange {
        start: None,
        end: None,
    }
}

fn proto_lsp_range_to_json(r: &LspRange) -> DocumentSymbolRangeJson {
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
    DocumentSymbolRangeJson {
        start: DocumentSymbolPositionJson {
            line: sl,
            character: sc,
        },
        end: DocumentSymbolPositionJson {
            line: el,
            character: ec,
        },
    }
}

/// Recursively converts a protobuf [`LspDocumentSymbol`] into its JSON row, mapping `children`.
fn document_symbol_to_json(s: &LspDocumentSymbol) -> DocumentSymbolJson {
    DocumentSymbolJson {
        name: s.name.clone(),
        detail: s.detail.clone(),
        kind: s.kind.clone(),
        range: proto_lsp_range_to_json(s.range.as_ref().unwrap_or(&default_empty_range())),
        selection_range: proto_lsp_range_to_json(
            s.selection_range.as_ref().unwrap_or(&default_empty_range()),
        ),
        children: s.children.iter().map(document_symbol_to_json).collect(),
    }
}

/// Prints a [`DocumentSymbolsResponse`] as JSON (hierarchical array of symbol objects).
fn print_document_symbols_json(resp: &DocumentSymbolsResponse) {
    let rows: Vec<DocumentSymbolJson> = resp.symbols.iter().map(document_symbol_to_json).collect();
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = serde_json::to_writer_pretty(&mut stdout, &rows) {
        eprintln!("racli document-symbols: failed to serialize JSON: {e}");
        return;
    }
    let _ = writeln!(stdout);
}

/// Prints a depth-first indented tree: `name (KIND) startLine:startChar-endLine:endChar` per line, two spaces per depth.
fn print_document_symbols_text(resp: &DocumentSymbolsResponse) {
    if resp.symbols.is_empty() {
        println!("(no symbols)");
        return;
    }
    for sym in &resp.symbols {
        print_symbol_tree_line(sym, 0);
    }
}

fn print_symbol_tree_line(sym: &LspDocumentSymbol, depth: usize) {
    let indent = "  ".repeat(depth);
    let range = sym
        .range
        .as_ref()
        .map(lsp_range_line)
        .filter(|s| !s.is_empty())
        .map(|s| format!(" {s}"))
        .unwrap_or_default();
    println!("{indent}{} ({}){range}", sym.name, sym.kind);
    for child in &sym.children {
        print_symbol_tree_line(child, depth + 1);
    }
}

/// Formats an LSP range as `startLine:startChar-endLine:endChar` for plain-text output.
fn lsp_range_line(r: &LspRange) -> String {
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

#[cfg(test)]
mod document_symbols_output_tests {
    use clap::Parser;

    use super::DocumentSymbolsArgs;
    use super::DocumentSymbolsPrintKind;

    #[test]
    fn document_symbols_args_default_print_kind_is_json() {
        let args = DocumentSymbolsArgs::try_parse_from(["racli", "src/main.rs"]).expect("parse");
        assert_eq!(args.print_kind(), DocumentSymbolsPrintKind::Json);
    }

    #[test]
    fn document_symbols_args_text_flag_selects_text() {
        let args =
            DocumentSymbolsArgs::try_parse_from(["racli", "--text", "src/main.rs"]).expect("parse");
        assert_eq!(args.print_kind(), DocumentSymbolsPrintKind::Text);
    }

    #[test]
    fn document_symbols_args_json_flag_selects_json() {
        let args =
            DocumentSymbolsArgs::try_parse_from(["racli", "--json", "src/main.rs"]).expect("parse");
        assert_eq!(args.print_kind(), DocumentSymbolsPrintKind::Json);
    }

    #[test]
    fn document_symbols_args_output_format_wins_over_flags() {
        let args = DocumentSymbolsArgs::try_parse_from([
            "racli",
            "--text",
            "--output-format",
            "json",
            "src/main.rs",
        ])
        .expect("parse");
        assert_eq!(args.print_kind(), DocumentSymbolsPrintKind::Json);
    }

    #[test]
    fn document_symbols_args_text_and_json_conflict() {
        let args =
            DocumentSymbolsArgs::try_parse_from(["racli", "--text", "--json", "src/main.rs"]);
        assert!(args.is_err(), "expected --text and --json to conflict");
    }
}

//! `racli document-symbols`: CLI arguments and formatting for LSP document-symbol outline results.

use std::path::PathBuf;

use clap::Parser;
use clap::ValueEnum;
use serde::Serialize;

use crate::client::Client;
use crate::client::client_unix_socket_path;
use crate::command_output::CommandError;
use crate::command_output::CommandOutput;
use crate::command_output::within_timeout;
use crate::proto::racli::DocumentSymbolsResponse;
use crate::proto::racli::LspDocumentSymbol;
use crate::proto::racli::LspRange;
use crate::racli_backend::RacliBackend;
use crate::text_position::range_text;

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

/// Runs the document-symbols RPC and writes the outline to `out` in the format selected by `args`.
pub async fn run_document_symbols(
    backend: &dyn RacliBackend,
    args: &DocumentSymbolsArgs,
    out: &mut CommandOutput,
) -> Result<(), CommandError> {
    let abs = match args.path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            out.note(format!("cannot canonicalize {}: {e}", args.path.display()));
            return Ok(());
        }
    };

    let file_path = abs.display().to_string();

    let resp = within_timeout(backend.document_symbols(&file_path))
        .await?
        .map_err(CommandError::Failed)?;
    out.mark_answered();
    match args.print_kind() {
        DocumentSymbolsPrintKind::Text => write_document_symbols_text(out, &resp),
        DocumentSymbolsPrintKind::Json => write_document_symbols_json(out, &resp),
    }
    Ok(())
}

/// Runs `racli document-symbols` against the project's server and prints what
/// [`run_document_symbols`] writes.
pub async fn run_cli_document_symbols(args: DocumentSymbolsArgs) {
    let sock = client_unix_socket_path();
    let sock_display = sock.display().to_string();
    let client = match Client::new(&sock) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("racli document-symbols ({sock_display}): {err}");
            return;
        }
    };
    let mut out = CommandOutput::default();
    let result = run_document_symbols(&client, &args, &mut out).await;
    out.print_cli("document-symbols", &sock_display, result);
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

/// Writes a [`DocumentSymbolsResponse`] as JSON (hierarchical array of symbol objects).
fn write_document_symbols_json(out: &mut CommandOutput, resp: &DocumentSymbolsResponse) {
    let rows: Vec<DocumentSymbolJson> = resp.symbols.iter().map(document_symbol_to_json).collect();
    match serde_json::to_string(&rows) {
        Ok(json) => out.println(json),
        Err(e) => out.note(format!("failed to serialize JSON: {e}")),
    }
}

/// Writes a depth-first indented tree: `name (KIND) startLine:startCol-endLine:endCol` per line, with
/// 1-based lines and columns, two spaces per depth.
fn write_document_symbols_text(out: &mut CommandOutput, resp: &DocumentSymbolsResponse) {
    if resp.symbols.is_empty() {
        out.println("(no symbols)");
        return;
    }
    for sym in &resp.symbols {
        write_symbol_tree_line(out, sym, 0);
    }
}

/// Writes `sym` and its children, indented two spaces per `depth`.
fn write_symbol_tree_line(out: &mut CommandOutput, sym: &LspDocumentSymbol, depth: usize) {
    let indent = "  ".repeat(depth);
    let range = sym
        .range
        .as_ref()
        .map(lsp_range_line)
        .filter(|s| !s.is_empty())
        .map(|s| format!(" {s}"))
        .unwrap_or_default();
    out.println(format!("{indent}{} ({}){range}", sym.name, sym.kind));
    for child in &sym.children {
        write_symbol_tree_line(out, child, depth + 1);
    }
}

/// Formats an LSP range as `startLine:startCol-endLine:endCol`, 1-based, for plain-text output.
fn lsp_range_line(r: &LspRange) -> String {
    let position = |p: Option<&crate::proto::racli::LspPosition>| {
        p.map(|p| (p.line, p.character)).unwrap_or((0, 0))
    };
    range_text(position(r.start.as_ref()), position(r.end.as_ref()))
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

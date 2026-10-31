//! `racli search`: CLI arguments and formatting for workspace symbol results.

use std::io::Write;

use clap::Parser;
use clap::ValueEnum;
use serde::Serialize;

use crate::client::Client;
use crate::client::client_unix_socket_path;
use crate::command_output::CommandError;
use crate::command_output::CommandOutput;
use crate::command_output::within_timeout;
use crate::proto::racli::lsp_workspace_symbol_response::Payload;
use crate::racli_backend::RacliBackend;
use crate::rust_analyzer::SymbolSearchKind;
use crate::rust_analyzer::SymbolSearchOptions;
use crate::rust_analyzer::SymbolSearchScope;
use crate::server::split_search_query_into_segments;
use crate::text_position::display_path;
use crate::text_position::position_text;

/// How `racli search` prints results (default is text).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SearchOutputFormat {
    /// One human-readable line per symbol (legacy plain-text layout).
    Text,
    /// One JSON array of objects with `name`, `kind`, `path`, and optional `range`.
    Json,
    /// RFC 4180-style CSV with a header row (`name`, `kind`, `path`, `range`).
    Csv,
}

/// Arguments for `racli search` (query passed to LSP `workspace/symbol`).
#[derive(Parser)]
pub struct SearchArgs {
    /// Print results as plain text, one symbol per line (equivalent to `--output-format text`; the default).
    #[arg(long, conflicts_with_all = ["json", "csv"])]
    pub text: bool,
    /// Print results as JSON (equivalent to `--output-format json`).
    #[arg(long, conflicts_with_all = ["csv", "text"])]
    pub json: bool,
    /// Print results as CSV with headers (equivalent to `--output-format csv`).
    #[arg(long, conflicts_with_all = ["json", "text"])]
    pub csv: bool,
    /// Select how search results are printed (default: text).
    #[arg(long, value_enum)]
    pub output_format: Option<SearchOutputFormat>,
    /// Symbol kinds to return (default: rust-analyzer's `only-types`); `all-symbols` adds functions, methods, constants, and fields.
    #[arg(long, value_enum)]
    pub kind: Option<SymbolSearchKind>,
    /// Crates to search (default: rust-analyzer's `workspace`); `workspace-and-dependencies` adds dependency crates.
    #[arg(long, value_enum)]
    pub scope: Option<SymbolSearchScope>,
    /// Keep only symbols named exactly as a pattern, e.g. `mkfifo` but not `Mkfifo` or `mkfifo_at`. A
    /// pattern's module path (`sys::mkfifo`) is still resolved by rust-analyzer.
    #[arg(long)]
    pub exact: bool,
    /// Workspace symbol query: unescaped `|` separates alternative fuzzy patterns (OR); use `\|` for a literal pipe (not full regex syntax).
    pub query: String,
}

/// Plain text, JSON, or CSV after resolving CLI flags for `racli search`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchPrintKind {
    Text,
    Json,
    Csv,
}

impl SearchArgs {
    /// Picks plain text, JSON, or CSV; explicit `--output-format` wins over `--text` / `--json` / `--csv`.
    fn print_kind(&self) -> SearchPrintKind {
        if let Some(fmt) = self.output_format {
            match fmt {
                SearchOutputFormat::Text => SearchPrintKind::Text,
                SearchOutputFormat::Json => SearchPrintKind::Json,
                SearchOutputFormat::Csv => SearchPrintKind::Csv,
            }
        } else if self.json {
            SearchPrintKind::Json
        } else if self.csv {
            SearchPrintKind::Csv
        } else {
            SearchPrintKind::Text
        }
    }
}

/// Runs the search RPC and writes results to `out` in the format selected by `args`.
pub async fn run_search(
    backend: &dyn RacliBackend,
    args: &SearchArgs,
    out: &mut CommandOutput,
) -> Result<(), CommandError> {
    let options = SymbolSearchOptions {
        kind: args.kind,
        scope: args.scope,
    };
    let mut resp = within_timeout(backend.search(&args.query, options))
        .await?
        .map_err(CommandError::Failed)?;
    out.mark_answered();
    if args.exact {
        keep_exact_names(&mut resp, &args.query);
    }
    if resp.truncated {
        out.note(
            "rust-analyzer had more results than the server's --symbol-search-limit, so some \
             are missing; narrow the query or raise the limit",
        );
    }
    match args.print_kind() {
        SearchPrintKind::Text => write_search_response(out, resp),
        SearchPrintKind::Json => write_search_response_json(out, resp),
        SearchPrintKind::Csv => write_search_response_csv_output(out, resp),
    }
    Ok(())
}

/// Runs `racli search` against the project's server and prints what [`run_search`] writes.
pub async fn run_cli_search(args: SearchArgs) {
    let sock = client_unix_socket_path();
    let sock_display = sock.display().to_string();
    let client = match Client::new(&sock) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("racli search ({sock_display}): {err}");
            return;
        }
    };
    let mut out = CommandOutput::default();
    let result = run_search(&client, &args, &mut out).await;
    out.print_cli("search", &sock_display, result);
}

/// Writes a [`crate::proto::racli::SearchResponse`] as human-readable lines: `name [KIND] path:line:col`.
fn write_search_response(out: &mut CommandOutput, resp: crate::proto::racli::SearchResponse) {
    let Some(ws) = resp.workspace_symbol_response else {
        out.println("(no workspace symbol response)");
        return;
    };
    let Some(payload) = ws.payload else {
        out.println("(empty workspace symbol result)");
        return;
    };
    match payload {
        Payload::Flat(list) => {
            out.println(format!("flat: {} symbol(s)", list.items.len()));
            for it in list.items {
                out.println(format!(
                    "{} [{}] {}",
                    it.name,
                    it.kind,
                    symbol_location_text(&it.uri, it.range.as_ref())
                ));
            }
        }
        Payload::Nested(list) => {
            out.println(format!("nested: {} symbol(s)", list.items.len()));
            for it in list.items {
                out.println(format!(
                    "{} [{}] {}",
                    it.name,
                    it.kind,
                    symbol_location_text(&it.uri, it.range.as_ref())
                ));
            }
        }
    }
}

/// Keeps only the symbols in `resp` named exactly as one of the query's `|` patterns, comparing a
/// qualified pattern such as `sys::mkfifo` by its last segment.
fn keep_exact_names(resp: &mut crate::proto::racli::SearchResponse, query: &str) {
    let segments = split_search_query_into_segments(query);
    let names: Vec<&str> = segments
        .iter()
        .map(|s| s.rsplit("::").next().unwrap_or(s))
        .collect();
    let Some(payload) = resp
        .workspace_symbol_response
        .as_mut()
        .and_then(|ws| ws.payload.as_mut())
    else {
        return;
    };
    match payload {
        Payload::Flat(list) => list.items.retain(|it| names.contains(&it.name.as_str())),
        Payload::Nested(list) => list.items.retain(|it| names.contains(&it.name.as_str())),
    }
}

/// A symbol's location for plain-text output: `path:line:col` with 1-based line and column, or
/// just the path when it has no range.
fn symbol_location_text(uri: &str, range: Option<&crate::proto::racli::LspRange>) -> String {
    match range.and_then(|r| r.start.as_ref()) {
        Some(start) => position_text(uri, start.line, start.character),
        None => display_path(uri),
    }
}

/// Single symbol row emitted in JSON search output (merged from flat or nested workspace symbol payloads).
#[derive(Serialize)]
struct SearchSymbolJson {
    name: String,
    kind: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<SearchRangeJson>,
}

/// LSP range serialized as JSON with `start` and `end` positions.
#[derive(Serialize)]
struct SearchRangeJson {
    start: SearchPositionJson,
    end: SearchPositionJson,
}

/// LSP zero-based line and UTF-16 character offset in JSON output.
#[derive(Serialize)]
struct SearchPositionJson {
    line: u32,
    character: u32,
}

/// Collects all symbols from a search RPC response into one list for JSON encoding.
fn search_response_to_json_rows(
    resp: crate::proto::racli::SearchResponse,
) -> Vec<SearchSymbolJson> {
    let mut rows = Vec::new();
    let Some(ws) = resp.workspace_symbol_response else {
        return rows;
    };
    let Some(payload) = ws.payload else {
        return rows;
    };
    match payload {
        Payload::Flat(list) => {
            for it in list.items {
                rows.push(SearchSymbolJson {
                    name: it.name,
                    kind: it.kind,
                    path: display_path(&it.uri),
                    range: it.range.as_ref().map(proto_lsp_range_to_json),
                });
            }
        }
        Payload::Nested(list) => {
            for it in list.items {
                rows.push(SearchSymbolJson {
                    name: it.name,
                    kind: it.kind,
                    path: display_path(&it.uri),
                    range: it.range.as_ref().map(proto_lsp_range_to_json),
                });
            }
        }
    }
    rows
}

/// Maps a protobuf [`crate::proto::racli::LspRange`] into the JSON row `range` object.
fn proto_lsp_range_to_json(r: &crate::proto::racli::LspRange) -> SearchRangeJson {
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
    SearchRangeJson {
        start: SearchPositionJson {
            line: sl,
            character: sc,
        },
        end: SearchPositionJson {
            line: el,
            character: ec,
        },
    }
}

/// Writes search results as a compact JSON array of symbol rows.
fn write_search_response_json(out: &mut CommandOutput, resp: crate::proto::racli::SearchResponse) {
    let rows = search_response_to_json_rows(resp);
    match serde_json::to_string(&rows) {
        Ok(s) => out.println(s),
        Err(e) => out.note(format!("failed to encode JSON: {e}")),
    }
}

/// Formats a JSON range value as `startLine:startChar-endLine:endChar` (same shape as plain-text search output).
fn search_range_json_to_line(r: &SearchRangeJson) -> String {
    format!(
        "{}:{}-{}:{}",
        r.start.line, r.start.character, r.end.line, r.end.character
    )
}

/// Quotes and escapes `s` when it contains CSV metacharacters (RFC 4180).
fn csv_escape_field(s: &str) -> String {
    if s.contains('"') || s.contains(',') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Writes search results as CSV with header `name,kind,path,range` to `w`; the `path` column is
/// the same path as `--text` output.
fn write_search_response_csv(
    mut w: impl Write,
    resp: crate::proto::racli::SearchResponse,
) -> std::io::Result<()> {
    writeln!(w, "name,kind,path,range")?;
    for row in search_response_to_json_rows(resp) {
        let range_s = row
            .range
            .as_ref()
            .map(search_range_json_to_line)
            .unwrap_or_default();
        writeln!(
            w,
            "{},{},{},{}",
            csv_escape_field(&row.name),
            csv_escape_field(&row.kind),
            csv_escape_field(&row.path),
            csv_escape_field(&range_s),
        )?;
    }
    Ok(())
}

/// Writes search results as CSV with a header row.
fn write_search_response_csv_output(
    out: &mut CommandOutput,
    resp: crate::proto::racli::SearchResponse,
) {
    let mut csv = Vec::new();
    if let Err(e) = write_search_response_csv(&mut csv, resp) {
        out.note(format!("failed to write CSV: {e}"));
    }
    out.print(String::from_utf8_lossy(&csv));
}

#[cfg(test)]
mod search_output_tests {
    use clap::Parser;

    use super::SearchArgs;
    use super::SearchPrintKind;
    use super::csv_escape_field;
    use super::keep_exact_names;
    use super::search_response_to_json_rows;
    use super::write_search_response_csv;
    use crate::proto::racli::LspPosition;
    use crate::proto::racli::LspRange;
    use crate::proto::racli::LspSymbolInformation;
    use crate::proto::racli::LspSymbolInformationList;
    use crate::proto::racli::LspWorkspaceSymbol;
    use crate::proto::racli::LspWorkspaceSymbolList;
    use crate::proto::racli::LspWorkspaceSymbolResponse;
    use crate::proto::racli::SearchResponse;
    use crate::proto::racli::lsp_workspace_symbol_response::Payload;

    #[test]
    fn json_rows_merge_flat_and_include_range() {
        let resp = SearchResponse {
            truncated: false,
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Flat(LspSymbolInformationList {
                    items: vec![LspSymbolInformation {
                        name: "foo".into(),
                        kind: "FUNCTION".into(),
                        uri: "file:///a.rs".into(),
                        container_name: None,
                        range: Some(LspRange {
                            start: Some(LspPosition {
                                line: 1,
                                character: 2,
                            }),
                            end: Some(LspPosition {
                                line: 3,
                                character: 4,
                            }),
                        }),
                    }],
                })),
            }),
        };
        let rows = search_response_to_json_rows(resp);
        let v = serde_json::to_value(&rows).expect("serialize");
        assert_eq!(v[0]["name"], "foo");
        assert_eq!(v[0]["kind"], "FUNCTION");
        assert_eq!(v[0]["path"], "/a.rs");
        assert_eq!(v[0]["range"]["start"]["line"], 1);
        assert_eq!(v[0]["range"]["end"]["character"], 4);
    }

    #[test]
    fn json_rows_omit_range_when_absent() {
        let resp = SearchResponse {
            truncated: false,
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Nested(LspWorkspaceSymbolList {
                    items: vec![LspWorkspaceSymbol {
                        name: "bar".into(),
                        kind: "MODULE".into(),
                        uri: "file:///b.rs".into(),
                        container_name: None,
                        range: None,
                    }],
                })),
            }),
        };
        let rows = search_response_to_json_rows(resp);
        let v = serde_json::to_value(&rows).expect("serialize");
        assert_eq!(v[0]["name"], "bar");
        assert!(v[0].get("range").is_none());
    }

    #[test]
    fn csv_writes_header_and_rows() {
        let resp = SearchResponse {
            truncated: false,
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Flat(LspSymbolInformationList {
                    items: vec![LspSymbolInformation {
                        name: "x".into(),
                        kind: "CLASS".into(),
                        uri: "file:///c.rs".into(),
                        container_name: None,
                        range: None,
                    }],
                })),
            }),
        };
        let mut buf = Vec::new();
        write_search_response_csv(&mut buf, resp).expect("csv");
        let s = String::from_utf8(buf).expect("utf8");
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "name,kind,path,range");
        assert_eq!(lines[1], "x,CLASS,/c.rs,");
    }

    #[test]
    fn csv_escapes_commas_in_fields() {
        let resp = SearchResponse {
            truncated: false,
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Flat(LspSymbolInformationList {
                    items: vec![LspSymbolInformation {
                        name: "a,b".into(),
                        kind: "FN".into(),
                        uri: "file:///d.rs".into(),
                        container_name: None,
                        range: Some(LspRange {
                            start: Some(LspPosition {
                                line: 0,
                                character: 0,
                            }),
                            end: Some(LspPosition {
                                line: 0,
                                character: 1,
                            }),
                        }),
                    }],
                })),
            }),
        };
        let mut buf = Vec::new();
        write_search_response_csv(&mut buf, resp).expect("csv");
        let s = String::from_utf8(buf).expect("utf8");
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[1], "\"a,b\",FN,/d.rs,0:0-0:1");
    }

    #[test]
    fn csv_escape_quotes() {
        assert_eq!(csv_escape_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn json_rows_empty_when_no_payload() {
        let resp = SearchResponse {
            truncated: false,
            workspace_symbol_response: None,
        };
        assert!(search_response_to_json_rows(resp).is_empty());
    }

    #[test]
    fn search_args_default_print_kind_is_text() {
        let args = SearchArgs::try_parse_from(["racli", "q"]).expect("parse");
        assert_eq!(args.print_kind(), SearchPrintKind::Text);
    }

    #[test]
    fn search_args_json_flag_selects_json() {
        let args = SearchArgs::try_parse_from(["racli", "--json", "q"]).expect("parse");
        assert_eq!(args.print_kind(), SearchPrintKind::Json);
    }

    #[test]
    fn search_args_text_flag_selects_plain_text() {
        let args = SearchArgs::try_parse_from(["racli", "--text", "q"]).expect("parse");
        assert_eq!(args.print_kind(), SearchPrintKind::Text);
    }

    #[test]
    fn keep_exact_names_drops_fuzzy_matches() {
        let symbol = |name: &str| LspSymbolInformation {
            name: name.into(),
            kind: "FUNCTION".into(),
            uri: "file:///a.rs".into(),
            container_name: None,
            range: None,
        };
        let mut resp = SearchResponse {
            truncated: false,
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Flat(LspSymbolInformationList {
                    items: ["mkfifo", "mkfifo_at", "Mkfifo", "unlink", "Named"]
                        .into_iter()
                        .map(symbol)
                        .collect(),
                })),
            }),
        };
        keep_exact_names(&mut resp, "mkfifo|sys::unlink");
        let Some(Payload::Flat(list)) = resp.workspace_symbol_response.and_then(|ws| ws.payload)
        else {
            panic!("expected a flat payload");
        };
        let names: Vec<&str> = list.items.iter().map(|it| it.name.as_str()).collect();
        assert_eq!(names, ["mkfifo", "unlink"]);
    }

    #[test]
    fn search_args_exact_flag_parses() {
        let args = SearchArgs::try_parse_from(["racli", "--exact", "q"]).expect("parse");
        assert!(args.exact);
    }
}

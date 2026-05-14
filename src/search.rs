//! `racli search`: CLI arguments and formatting for workspace symbol results.

use std::io::Write;
use std::time::Duration;

use clap::Parser;
use clap::ValueEnum;
use serde::Serialize;

use crate::client;
use crate::effective_unix_socket_path;
use crate::proto::racli::lsp_workspace_symbol_response::Payload;

/// How `racli search` prints results (default is JSON).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SearchOutputFormat {
    /// One human-readable line per symbol (legacy plain-text layout).
    Text,
    /// One JSON array of objects with `name`, `kind`, `uri`, and optional `range`.
    Json,
    /// RFC 4180-style CSV with a header row (`name`, `kind`, `uri`, `range`).
    Csv,
}

/// Arguments for `racli search` (query passed to LSP `workspace/symbol`).
#[derive(Parser)]
pub struct SearchArgs {
    /// Print results as plain text, one symbol per line (equivalent to `--output-format text`).
    #[arg(long, conflicts_with_all = ["json", "csv"])]
    pub text: bool,
    /// Print results as JSON (equivalent to `--output-format json`; same as the default).
    #[arg(long, conflicts_with_all = ["csv", "text"])]
    pub json: bool,
    /// Print results as CSV with headers (equivalent to `--output-format csv`).
    #[arg(long, conflicts_with_all = ["json", "text"])]
    pub csv: bool,
    /// Select how search results are printed (default: json).
    #[arg(long, value_enum)]
    pub output_format: Option<SearchOutputFormat>,
    /// Workspace symbol query: unescaped `|` separates alternative substring patterns (OR); use `\|` for a literal pipe (not full regex syntax).
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
        } else if self.text {
            SearchPrintKind::Text
        } else {
            SearchPrintKind::Json
        }
    }
}

/// Runs the search RPC and prints results in the format selected by `args`.
pub async fn run_cli_search(args: SearchArgs) {
    let sock = effective_unix_socket_path();
    let sock_display = sock.display().to_string();
    match tokio::time::timeout(Duration::from_secs(60), client::search(&sock, &args.query)).await {
        Ok(Ok(resp)) => match args.print_kind() {
            SearchPrintKind::Text => print_search_response(resp),
            SearchPrintKind::Json => print_search_response_json(resp),
            SearchPrintKind::Csv => print_search_response_csv(resp),
        },
        Ok(Err(err)) => {
            eprintln!("racli search ({sock_display}): {err}");
        }
        Err(_elapsed) => {
            eprintln!("racli search ({sock_display}): request timed out after 60 seconds");
        }
    }
}

/// Prints a [`crate::proto::racli::SearchResponse`] as human-readable lines.
fn print_search_response(resp: crate::proto::racli::SearchResponse) {
    let Some(ws) = resp.workspace_symbol_response else {
        println!("(no workspace symbol response)");
        return;
    };
    let Some(payload) = ws.payload else {
        println!("(empty workspace symbol result)");
        return;
    };
    match payload {
        Payload::Flat(list) => {
            println!("flat: {} symbol(s)", list.items.len());
            for it in list.items {
                let range = it
                    .range
                    .as_ref()
                    .map(lsp_range_line)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" {s}"))
                    .unwrap_or_default();
                println!("{} [{}] {}{}", it.name, it.kind, it.uri, range);
            }
        }
        Payload::Nested(list) => {
            println!("nested: {} symbol(s)", list.items.len());
            for it in list.items {
                let range = it
                    .range
                    .as_ref()
                    .map(lsp_range_line)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" {s}"))
                    .unwrap_or_default();
                println!("{} [{}] {}{}", it.name, it.kind, it.uri, range);
            }
        }
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

/// Single symbol row emitted in JSON search output (merged from flat or nested workspace symbol payloads).
#[derive(Serialize)]
struct SearchSymbolJson {
    name: String,
    kind: String,
    uri: String,
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
                    uri: it.uri,
                    range: it.range.as_ref().map(proto_lsp_range_to_json),
                });
            }
        }
        Payload::Nested(list) => {
            for it in list.items {
                rows.push(SearchSymbolJson {
                    name: it.name,
                    kind: it.kind,
                    uri: it.uri,
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

/// Prints search results as a pretty-printed JSON array of symbol rows.
fn print_search_response_json(resp: crate::proto::racli::SearchResponse) {
    let rows = search_response_to_json_rows(resp);
    match serde_json::to_string_pretty(&rows) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("racli search: failed to encode JSON: {e}"),
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

/// Writes search results as CSV with header `name,kind,uri,range` to `w`.
fn write_search_response_csv(
    mut w: impl Write,
    resp: crate::proto::racli::SearchResponse,
) -> std::io::Result<()> {
    writeln!(w, "name,kind,uri,range")?;
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
            csv_escape_field(&row.uri),
            csv_escape_field(&range_s),
        )?;
    }
    Ok(())
}

/// Prints search results as CSV with a header row.
fn print_search_response_csv(resp: crate::proto::racli::SearchResponse) {
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = write_search_response_csv(&mut stdout, resp) {
        eprintln!("racli search: failed to write CSV: {e}");
    }
}

#[cfg(test)]
mod search_output_tests {
    use clap::Parser;

    use super::SearchArgs;
    use super::SearchPrintKind;
    use super::csv_escape_field;
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
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Flat(LspSymbolInformationList {
                    items: vec![LspSymbolInformation {
                        name: "foo".into(),
                        kind: "FUNCTION".into(),
                        uri: "file:///a.rs".into(),
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
        assert_eq!(v[0]["uri"], "file:///a.rs");
        assert_eq!(v[0]["range"]["start"]["line"], 1);
        assert_eq!(v[0]["range"]["end"]["character"], 4);
    }

    #[test]
    fn json_rows_omit_range_when_absent() {
        let resp = SearchResponse {
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Nested(LspWorkspaceSymbolList {
                    items: vec![LspWorkspaceSymbol {
                        name: "bar".into(),
                        kind: "MODULE".into(),
                        uri: "file:///b.rs".into(),
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
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Flat(LspSymbolInformationList {
                    items: vec![LspSymbolInformation {
                        name: "x".into(),
                        kind: "CLASS".into(),
                        uri: "file:///c.rs".into(),
                        range: None,
                    }],
                })),
            }),
        };
        let mut buf = Vec::new();
        write_search_response_csv(&mut buf, resp).expect("csv");
        let s = String::from_utf8(buf).expect("utf8");
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "name,kind,uri,range");
        assert_eq!(lines[1], "x,CLASS,file:///c.rs,");
    }

    #[test]
    fn csv_escapes_commas_in_fields() {
        let resp = SearchResponse {
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Flat(LspSymbolInformationList {
                    items: vec![LspSymbolInformation {
                        name: "a,b".into(),
                        kind: "FN".into(),
                        uri: "file:///d.rs".into(),
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
        assert_eq!(lines[1], "\"a,b\",FN,file:///d.rs,0:0-0:1");
    }

    #[test]
    fn csv_escape_quotes() {
        assert_eq!(csv_escape_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn json_rows_empty_when_no_payload() {
        let resp = SearchResponse {
            workspace_symbol_response: None,
        };
        assert!(search_response_to_json_rows(resp).is_empty());
    }

    #[test]
    fn search_args_default_print_kind_is_json() {
        let args = SearchArgs::try_parse_from(["racli", "q"]).expect("parse");
        assert_eq!(args.print_kind(), SearchPrintKind::Json);
    }

    #[test]
    fn search_args_text_flag_selects_plain_text() {
        let args = SearchArgs::try_parse_from(["racli", "--text", "q"]).expect("parse");
        assert_eq!(args.print_kind(), SearchPrintKind::Text);
    }
}

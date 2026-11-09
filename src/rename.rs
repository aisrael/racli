//! `racli rename`: CLI arguments and formatting for the edits of an LSP rename. The edits are
//! only reported, never written (ADR 008).

use std::path::PathBuf;

use clap::Parser;
use serde::Serialize;

use crate::client::Client;
use crate::client::client_unix_socket_path;
use crate::command_output::CommandError;
use crate::command_output::CommandOutput;
use crate::command_output::within_timeout;
use crate::proto::racli::LspFileEdits;
use crate::proto::racli::LspRange;
use crate::proto::racli::RenameResponse;
use crate::racli_backend::RacliBackend;
use crate::symbol;
use crate::symbol::SymbolArgs;
use crate::symbol::Target;
use crate::text_position::SourceLines;
use crate::text_position::display_path;
use crate::text_position::parse_one_based;

/// Arguments for `racli rename` (LSP `textDocument/rename`): reports the edits, writes nothing.
#[derive(Parser)]
pub struct RenameArgs {
    /// The symbol to rename, e.g. `mkfifo`, `sys::mkfifo` or `Fifo::name`. It must have exactly
    /// one definition.
    #[arg(value_name = "NAME", required_unless_present = "path")]
    pub name: Option<String>,
    /// The new name for the symbol.
    #[arg(long, value_name = "NEW_NAME")]
    pub new_name: String,
    /// Rust source file of a position to rename from, with `--line` and `--character` (absolute
    /// or relative to the current directory).
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
    /// Print one human-readable line per edit (the default).
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
    /// Print JSON instead of text.
    #[arg(long, conflicts_with = "text")]
    pub json: bool,
}

/// Asks for the edits that rename the one definition the arguments find, and writes them as JSON
/// or plain text to `out`. Fails with a usage error if there is more than one definition.
pub async fn run_rename(
    backend: &dyn RacliBackend,
    args: &RenameArgs,
    out: &mut CommandOutput,
) -> Result<(), CommandError> {
    let found = symbol::command_targets(
        backend,
        args.name.as_slice(),
        args.path.as_deref(),
        args.line,
        args.character,
        &args.symbol,
        out,
    )
    .await?;
    let mut targets = found.targets;
    if targets.len() > 1 {
        return Err(CommandError::Usage(ambiguity_message(&targets)));
    }
    let Some(target) = targets.pop() else {
        return Err(CommandError::Failed("no symbol to rename".into()));
    };

    let resp = within_timeout(backend.rename(
        &target.file_path,
        target.line,
        target.character,
        &args.new_name,
    ))
    .await?
    .map_err(CommandError::Failed)?;
    out.mark_answered();

    if args.json {
        write_rename_json(out, &resp);
    } else {
        let mut sources = SourceLines::default();
        match &target.symbol {
            Some(symbol) => {
                out.println(symbol.header());
                write_rename_text(out, &resp, "  ", &mut sources);
            }
            None => write_rename_text(out, &resp, "", &mut sources),
        }
    }
    Ok(())
}

/// Runs `racli rename` against the project's server and prints what [`run_rename`] writes.
pub async fn run_cli_rename(args: RenameArgs) {
    let sock = client_unix_socket_path();
    let sock_display = sock.display().to_string();
    let client = match Client::new(&sock) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("racli rename ({sock_display}): {err}");
            return;
        }
    };
    let mut out = CommandOutput::default();
    let result = run_rename(&client, &args, &mut out).await;
    out.print_cli("rename", &sock_display, result);
}

/// The usage error for a rename with several definitions: lists them and says how to pick one.
fn ambiguity_message(targets: &[Target]) -> String {
    let mut message = format!(
        "cannot rename: {} definitions match; the rename needs exactly one",
        targets.len()
    );
    for target in targets {
        let line = match &target.symbol {
            Some(symbol) => symbol.header(),
            None => format!(
                "{}:{}:{}",
                target.file_path,
                target.line + 1,
                target.character + 1
            ),
        };
        message.push_str("\n  ");
        message.push_str(&line);
    }
    message.push_str(
        "\nqualify the name (`Type::name`, `module::name`), narrow it with --kind or --glob, or use --path, --line and --character",
    );
    message
}

/// Writes the edits after `indent`, one line each: `path:line:col: old text => new text`, with
/// the path relative to the current directory and a 1-based line and column. File renames follow
/// as `old/path.rs => new/path.rs`.
fn write_rename_text(
    out: &mut CommandOutput,
    resp: &RenameResponse,
    indent: &str,
    sources: &mut SourceLines,
) {
    if resp.files.iter().all(|file| file.edits.is_empty()) && resp.renames.is_empty() {
        out.println(format!("{indent}(no edits)"));
        return;
    }
    for file in &resp.files {
        for edit in &file.edits {
            let (start, end) = range_bounds(edit.range.as_ref());
            let old = replaced_text(sources, &file.uri, start, end);
            out.println(format!(
                "{indent}{}:{}:{}: {old} => {}",
                display_path(&file.uri),
                start.0 + 1,
                start.1 + 1,
                edit.new_text
            ));
        }
    }
    for rename in &resp.renames {
        out.println(format!(
            "{indent}{} => {}",
            display_path(&rename.old_uri),
            display_path(&rename.new_uri)
        ));
    }
}

/// The text a single-line range covers, or `…` if the range spans lines or the file can't be read.
fn replaced_text(
    sources: &mut SourceLines,
    uri: &str,
    start: (u32, u32),
    end: (u32, u32),
) -> String {
    if start.0 != end.0 {
        return "…".to_string();
    }
    let Some(lines) = sources.lines(uri, start.0, start.0) else {
        return "…".to_string();
    };
    let Some(line) = lines.first() else {
        return "…".to_string();
    };
    utf16_slice(line, start.1, end.1).unwrap_or_else(|| "…".to_string())
}

/// The part of `line` between two UTF-16 offsets, or `None` if an offset isn't on a character.
fn utf16_slice(line: &str, start: u32, end: u32) -> Option<String> {
    let mut units = 0u32;
    let mut slice = String::new();
    for c in line.chars() {
        if units >= end {
            break;
        }
        if units >= start {
            slice.push(c);
        }
        units += c.len_utf16() as u32;
    }
    (units >= end || end == start).then_some(slice)
}

/// The 0-based `(line, character)` start and end of an optional protobuf range.
fn range_bounds(range: Option<&LspRange>) -> ((u32, u32), (u32, u32)) {
    let bounds = |position: Option<&crate::proto::racli::LspPosition>| {
        position.map(|p| (p.line, p.character)).unwrap_or((0, 0))
    };
    match range {
        Some(range) => (bounds(range.start.as_ref()), bounds(range.end.as_ref())),
        None => ((0, 0), (0, 0)),
    }
}

/// Writes the edits as JSON: `{"files": [{"path", "edits": [{"range", "new_text"}]}], "renames":
/// [{"old_path", "new_path"}]}`, with paths relative to the current directory and 0-based LSP ranges, as the other commands do.
fn write_rename_json(out: &mut CommandOutput, resp: &RenameResponse) {
    let json = RenameJson {
        files: resp.files.iter().map(file_edits_json).collect(),
        renames: resp
            .renames
            .iter()
            .map(|rename| FileRenameJson {
                old_path: display_path(&rename.old_uri),
                new_path: display_path(&rename.new_uri),
            })
            .collect(),
    };
    match serde_json::to_string(&json) {
        Ok(json) => out.println(json),
        Err(e) => out.note(format!("failed to serialize JSON: {e}")),
    }
}

/// The JSON for one document's edits.
fn file_edits_json(file: &LspFileEdits) -> FileEditsJson {
    FileEditsJson {
        path: display_path(&file.uri),
        edits: file
            .edits
            .iter()
            .map(|edit| {
                let (start, end) = range_bounds(edit.range.as_ref());
                TextEditJson {
                    range: RangeJson {
                        start: PositionJson {
                            line: start.0,
                            character: start.1,
                        },
                        end: PositionJson {
                            line: end.0,
                            character: end.1,
                        },
                    },
                    new_text: edit.new_text.clone(),
                }
            })
            .collect(),
    }
}

/// The JSON output of `racli rename`.
#[derive(Serialize)]
struct RenameJson {
    files: Vec<FileEditsJson>,
    renames: Vec<FileRenameJson>,
}

/// A file rename in JSON.
#[derive(Serialize)]
struct FileRenameJson {
    old_path: String,
    new_path: String,
}

/// The edits in one document, in JSON.
#[derive(Serialize)]
struct FileEditsJson {
    path: String,
    edits: Vec<TextEditJson>,
}

/// One edit in JSON: the range to replace and its replacement.
#[derive(Serialize)]
struct TextEditJson {
    range: RangeJson,
    new_text: String,
}

/// An LSP range in JSON.
#[derive(Serialize)]
struct RangeJson {
    start: PositionJson,
    end: PositionJson,
}

/// A 0-based LSP line and UTF-16 character in JSON.
#[derive(Serialize)]
struct PositionJson {
    line: u32,
    character: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::racli::LspFileRename;

    fn response_with_rename() -> RenameResponse {
        RenameResponse {
            files: vec![],
            renames: vec![LspFileRename {
                old_uri: "file:///tmp/a.rs".into(),
                new_uri: "file:///tmp/b.rs".into(),
            }],
        }
    }

    #[test]
    fn text_output_lists_file_renames() {
        let mut out = CommandOutput::default();
        write_rename_text(
            &mut out,
            &response_with_rename(),
            "",
            &mut SourceLines::default(),
        );
        let text = out.stdout();
        assert!(text.contains("a.rs => ") && text.contains("b.rs"), "{text}");
        assert!(!text.contains("(no edits)"), "{text}");
    }

    #[test]
    fn json_output_lists_file_renames() {
        let mut out = CommandOutput::default();
        write_rename_json(&mut out, &response_with_rename());
        let text = out.stdout();
        assert!(text.contains(r#""renames":[{"old_path":"#), "{text}");
    }

    #[test]
    fn utf16_slice_counts_utf16_units() {
        // "é" is one UTF-16 unit, "😀" is two.
        assert_eq!(utf16_slice("aé😀b", 1, 2).as_deref(), Some("é"));
        assert_eq!(utf16_slice("aé😀b", 2, 4).as_deref(), Some("😀"));
        assert_eq!(utf16_slice("abc", 1, 3).as_deref(), Some("bc"));
    }

    #[test]
    fn utf16_slice_rejects_a_range_past_the_line() {
        assert_eq!(utf16_slice("abc", 1, 9), None);
    }
}

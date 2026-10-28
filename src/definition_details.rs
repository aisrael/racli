//! `find-definition --hover` and `--source`: a definition's signature and doc comment (LSP
//! hover), or its whole source, from its location.

use serde::Serialize;

use crate::client::Client;
use crate::proto::racli::LspLocation;
use crate::symbol::RangeJson;
use crate::symbol::outline_symbol_at;
use crate::symbol::range_json;
use crate::text_position::SourceLines;

/// What `--hover` and `--source` add to one definition location.
#[derive(Debug, Default)]
pub struct DefinitionDetails {
    /// rust-analyzer's hover for the definition, as Markdown.
    pub hover: Option<String>,
    /// The definition's whole source.
    pub source: Option<DefinitionSource>,
}

/// A definition's full range, from its file's outline, and its text.
#[derive(Debug, Serialize)]
pub struct DefinitionSource {
    /// The definition's full range, 0-based as in LSP, including its doc comment.
    pub range: RangeJson,
    /// The lines the range covers.
    pub text: String,
}

/// Fetches the hover and/or the source of the definition at `loc`. Errors are returned as text, to
/// print alongside the location rather than stop the command.
pub async fn definition_details(
    client: &Client,
    loc: &LspLocation,
    hover: bool,
    source: bool,
    sources: &mut SourceLines,
) -> (DefinitionDetails, Vec<String>) {
    let mut details = DefinitionDetails::default();
    let mut errors = Vec::new();
    let Some(range) = loc.range.as_ref() else {
        return (details, errors);
    };
    if hover {
        match hover_markdown(client, &loc.uri, range).await {
            Ok(markdown) => details.hover = markdown,
            Err(e) => errors.push(format!("hover: {e}")),
        }
    }
    if source {
        match definition_source(client, &loc.uri, range, sources).await {
            Ok(text) => details.source = text,
            Err(e) => errors.push(format!("source: {e}")),
        }
    }
    (details, errors)
}

/// rust-analyzer's `textDocument/hover` at the start of `range`, as Markdown, or `None` if it has
/// nothing to show.
async fn hover_markdown(
    client: &Client,
    uri: &str,
    range: &crate::proto::racli::LspRange,
) -> Result<Option<String>, String> {
    let start = range.start.as_ref().ok_or("definition has no range")?;
    let Some(path) = url::Url::parse(uri)
        .ok()
        .and_then(|u| u.to_file_path().ok())
    else {
        return Ok(None);
    };
    let resp = client
        .hover(path.display().to_string(), start.line, start.character)
        .await
        .map(|reply| reply.value)
        .map_err(|e| e.to_string())?;
    Ok(resp.markdown)
}

/// The definition's whole source: the range of the outline symbol whose name is at `range`, or
/// `None` if no outline symbol is there.
async fn definition_source(
    client: &Client,
    uri: &str,
    range: &crate::proto::racli::LspRange,
    sources: &mut SourceLines,
) -> Result<Option<DefinitionSource>, String> {
    let Some(path) = url::Url::parse(uri)
        .ok()
        .and_then(|u| u.to_file_path().ok())
    else {
        return Ok(None);
    };
    let outline = client
        .document_symbols(path.display().to_string())
        .await
        .map(|reply| reply.value)
        .map_err(|e| e.to_string())?;
    let Some(full) = outline_symbol_at(&outline.symbols, range)
        .and_then(|symbol| symbol.range)
        .and_then(range_json)
    else {
        return Ok(None);
    };
    // LSP ranges end before `end`, so an end at column 0 doesn't include that line.
    let last = if full.end.character == 0 && full.end.line > full.start.line {
        full.end.line - 1
    } else {
        full.end.line
    };
    Ok(sources
        .lines(uri, full.start.line, last)
        .map(|lines| DefinitionSource {
            range: full,
            text: lines.join("\n"),
        }))
}

/// Hover Markdown as plain text for `--text`: code fences are dropped, `---` rules become blank
/// lines, and runs of blank lines become one.
pub fn hover_text(markdown: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        let line = if line.trim() == "---" { "" } else { line };
        if line.trim().is_empty() && lines.last().is_none_or(|last| last.trim().is_empty()) {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n").trim_end().to_string()
}

/// Source lines numbered from the 0-based `first_line`, as 1-based `cat -n`-style lines.
pub fn numbered_source(text: &str, first_line: u32) -> Vec<String> {
    let count = text.lines().count() as u32;
    let width = (first_line + count).to_string().len();
    text.lines()
        .enumerate()
        .map(|(i, line)| format!("{:>width$}  {line}", first_line + 1 + i as u32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_text_drops_fences_and_rules_and_extra_blank_lines() {
        let markdown = "\n```rust\nqueue::sys\n```\n\n```rust\npub fn mkfifo(path: &str) -> i32\n```\n\n---\n\nMakes a FIFO.\n";
        assert_eq!(
            hover_text(markdown),
            "queue::sys\n\npub fn mkfifo(path: &str) -> i32\n\nMakes a FIFO."
        );
    }

    #[test]
    fn numbered_source_counts_from_one_and_aligns() {
        assert_eq!(
            numbered_source("fn a() {\n}", 8),
            vec![" 9  fn a() {".to_string(), "10  }".to_string()]
        );
    }
}

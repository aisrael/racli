//! 1-based positions for the CLI: parsing `--line` and `--character`, and printing locations in
//! `--text` output as `path:line:col`, with the path relative to the current directory.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Parses a 1-based `--line` or `--character`, rejecting `0`.
pub fn parse_one_based(value: &str) -> Result<u32, String> {
    match value.parse::<u32>() {
        Ok(0) => Err("positions are 1-based, so the first line or character is 1".into()),
        Ok(n) => Ok(n),
        Err(e) => Err(e.to_string()),
    }
}

/// A URI's path for `--text` output: relative to the current directory when it's under it,
/// absolute otherwise, or the URI itself when it isn't a local file.
pub fn display_path(uri: &str) -> String {
    let Some(path) = url::Url::parse(uri)
        .ok()
        .and_then(|u| u.to_file_path().ok())
    else {
        return uri.to_string();
    };
    match canonical_current_dir().map(|cwd| path.strip_prefix(cwd)) {
        Some(Ok(relative)) => relative.display().to_string(),
        _ => path.display().to_string(),
    }
}

/// The canonical current directory, resolved once per process since `--text` output can print
/// thousands of locations; `None` if it can't be read.
fn canonical_current_dir() -> Option<&'static Path> {
    static CWD: OnceLock<Option<PathBuf>> = OnceLock::new();
    CWD.get_or_init(|| std::env::current_dir().and_then(|d| d.canonicalize()).ok())
        .as_deref()
}

/// `path:line:col` for a 0-based LSP position in `uri`, with 1-based line and column.
pub fn position_text(uri: &str, line: u32, character: u32) -> String {
    format!("{}:{}:{}", display_path(uri), line + 1, character + 1)
}

/// `startLine:startCol-endLine:endCol` for a 0-based LSP range, with 1-based lines and columns.
pub fn range_text(start: (u32, u32), end: (u32, u32)) -> String {
    format!(
        "{}:{}-{}:{}",
        start.0 + 1,
        start.1 + 1,
        end.0 + 1,
        end.1 + 1
    )
}

/// Reads source lines for `--text` output, reading each file once.
#[derive(Default)]
pub struct SourceLines {
    /// Each file's lines, or `None` if it couldn't be read.
    files: HashMap<PathBuf, Option<Vec<String>>>,
}

impl SourceLines {
    /// `path:line:col: source line` for a 0-based LSP position in `uri`, or just `path:line:col`
    /// when the line can't be read.
    pub fn location(&mut self, uri: &str, line: u32, character: u32) -> String {
        let position = position_text(uri, line, character);
        match self.line(uri, line) {
            Some(text) => format!("{position}: {text}"),
            None => position,
        }
    }

    /// The 0-based `line` of the file at `uri`, without its line ending.
    fn line(&mut self, uri: &str, line: u32) -> Option<&str> {
        self.file(uri)?.get(line as usize).map(String::as_str)
    }

    /// The 0-based lines `start` to `end`, inclusive, of the file at `uri`, or as many of them as
    /// the file has.
    pub fn lines(&mut self, uri: &str, start: u32, end: u32) -> Option<&[String]> {
        let lines = self.file(uri)?;
        let start = (start as usize).min(lines.len());
        let end = (end as usize + 1).clamp(start, lines.len());
        Some(&lines[start..end])
    }

    /// The lines of the file at `uri`, read on first use.
    fn file(&mut self, uri: &str) -> Option<&Vec<String>> {
        let path = url::Url::parse(uri).ok()?.to_file_path().ok()?;
        self.files
            .entry(path)
            .or_insert_with_key(|path| {
                std::fs::read_to_string(path)
                    .ok()
                    .map(|text| text.lines().map(String::from).collect())
            })
            .as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_one_based_rejects_zero() {
        assert_eq!(parse_one_based("1"), Ok(1));
        assert_eq!(parse_one_based("42"), Ok(42));
        assert!(parse_one_based("0").expect_err("0").contains("1-based"));
        assert!(parse_one_based("x").is_err());
    }

    #[test]
    fn positions_and_ranges_are_one_based() {
        assert_eq!(position_text("file:///abs/a.rs", 0, 0), "/abs/a.rs:1:1");
        assert_eq!(range_text((2, 7), (2, 13)), "3:8-3:14");
        assert_eq!(display_path("untitled:x"), "untitled:x");
    }

    #[test]
    fn paths_under_the_current_directory_are_relative() {
        let cwd = std::env::current_dir()
            .and_then(|d| d.canonicalize())
            .expect("current dir");
        let uri = url::Url::from_file_path(cwd.join("src/lib.rs")).expect("file URI");
        assert_eq!(display_path(uri.as_str()), "src/lib.rs");
    }

    #[test]
    fn locations_end_with_the_source_line() {
        let cwd = std::env::current_dir()
            .and_then(|d| d.canonicalize())
            .expect("current dir");
        let uri = url::Url::from_file_path(cwd.join("fixtures/queue/src/sys.rs")).expect("URI");
        let mut sources = SourceLines::default();
        assert_eq!(
            sources.location(uri.as_str(), 2, 7),
            "fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {"
        );
        assert_eq!(
            sources.location(uri.as_str(), 9999, 0),
            "fixtures/queue/src/sys.rs:10000:1"
        );
    }
}

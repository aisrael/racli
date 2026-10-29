//! Symbol names as targets: finds the definitions of each name given to a position command
//! (`find-definition`, `find-references`, `find-implementations`, `call-hierarchy`), so it can
//! run from names as well as, or instead of, a file position.

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use serde::Serialize;

use crate::command_output::CommandError;
use crate::command_output::CommandOutput;
use crate::command_output::within_timeout;
use crate::proto::racli::LspDocumentSymbol;
use crate::proto::racli::LspRange;
use crate::proto::racli::SearchResponse;
use crate::proto::racli::lsp_workspace_symbol_response::Payload;
use crate::racli_backend::RacliBackend;
use crate::rust_analyzer::SymbolSearchKind;
use crate::rust_analyzer::SymbolSearchOptions;
use crate::rust_analyzer::SymbolSearchScope;
use crate::text_position::position_text;

/// How many of the closest symbols a "not found" error lists.
const MAX_SUGGESTIONS: usize = 10;

/// What a lookup says when rust-analyzer's results were cut to the server's limit.
const TRUNCATED_NOTE: &str = "rust-analyzer had more results than the server's --symbol-search-limit, \
     so some definitions may be missing; qualify the name or raise the limit";

/// `--kind` and `--glob`: narrow the definitions found for symbol names.
#[derive(clap::Args, Debug, Default)]
pub struct SymbolArgs {
    /// Only use definitions of this kind, as `racli search --text` prints it (e.g. `function`, `method`, `struct`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Only use definitions in files that match this glob, e.g. `src/**/*.rs`. A relative pattern
    /// matches files under the current directory.
    #[arg(long, value_name = "PATTERN", value_parser = parse_glob)]
    pub glob: Option<glob::Pattern>,
}

/// Parses `--glob`, so an invalid pattern is a usage error.
fn parse_glob(pattern: &str) -> Result<glob::Pattern, glob::PatternError> {
    glob::Pattern::new(pattern)
}

/// How `--glob` matches, as in a shell: `*` and `?` stay within one path component, and `**`
/// spans several.
const GLOB_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// A definition found by name: the symbol, and the file and position to run a command from.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolMatch {
    pub name: String,
    pub kind: String,
    /// The item that contains the symbol, e.g. the type of a method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    pub uri: String,
    pub range: RangeJson,
}

/// LSP range as JSON with `start` and `end` positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct RangeJson {
    pub start: PositionJson,
    pub end: PositionJson,
}

/// LSP zero-based line and UTF-16 character offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct PositionJson {
    pub line: u32,
    pub character: u32,
}

impl SymbolMatch {
    /// The local path of the file that defines the symbol, or `None` if its URI isn't a `file:` URI.
    pub fn file_path(&self) -> Option<PathBuf> {
        url::Url::parse(&self.uri).ok()?.to_file_path().ok()
    }

    /// One line describing the symbol, in the layout of `racli search --text`: its name, kind,
    /// container and `path:line:col`.
    pub fn header(&self) -> String {
        let container = self
            .container_name
            .as_ref()
            .map(|c| format!(" in {c}"))
            .unwrap_or_default();
        let start = self.range.start;
        format!(
            "{} [{}]{container} {}",
            self.name,
            self.kind,
            position_text(&self.uri, start.line, start.character)
        )
    }
}

/// A command's result for one definition, as printed in JSON when symbol names were given.
#[derive(Serialize)]
pub struct SymbolResultJson<'a, T: Serialize> {
    pub symbol: &'a SymbolMatch,
    pub result: T,
}

/// Why a symbol name found no definition to run the command from.
#[derive(Debug, thiserror::Error)]
pub enum ResolveSymbolError {
    /// The symbol search RPC failed.
    #[error("{0}")]
    Search(String),
    /// No symbol has that name, or none passes `--kind` and `--glob`.
    #[error("{}", not_found_message(.name, .filtered, .truncated, .suggestions))]
    NotFound {
        name: String,
        /// Whether symbols with the name exist but `--kind` or `--glob` left them all out.
        filtered: bool,
        /// Whether any search was cut to the server's limit, so the name may exist after all.
        truncated: bool,
        /// The closest symbols, to retry with.
        suggestions: Vec<SymbolMatch>,
    },
}

fn not_found_message(
    name: &str,
    filtered: &bool,
    truncated: &bool,
    suggestions: &[SymbolMatch],
) -> String {
    let mut message = if *filtered {
        format!("no symbol named {name} passes --kind and --glob")
    } else {
        format!("no symbol named {name}")
    };
    if *truncated {
        message.push_str(&format!(" ({TRUNCATED_NOTE})"));
    }
    if !suggestions.is_empty() {
        message.push_str(if *filtered { "; found:" } else { "; closest:" });
        for s in suggestions {
            message.push_str("\n  ");
            message.push_str(&s.header());
        }
    }
    message
}

/// What a position command runs from: a symbol name, or a position in a file.
#[derive(Debug, PartialEq)]
pub enum TargetSpec {
    /// A symbol name, e.g. `mkfifo`, `sys::mkfifo` or `Fifo::name`.
    Name(String),
    /// The `--path`, `--line` and `--character` of a position, converted to LSP's 0-based line and
    /// character.
    Position {
        path: PathBuf,
        line: u32,
        character: u32,
    },
}

/// Checks a position command's names and position, and lists them: the names in argument order,
/// then the position.
pub fn classify(
    names: &[String],
    path: Option<&Path>,
    line: Option<u32>,
    character: Option<u32>,
    symbol_args: &SymbolArgs,
) -> Result<Vec<TargetSpec>, String> {
    let mut specs = Vec::with_capacity(names.len() + 1);
    for name in names {
        if !is_rust_path(name) {
            return Err(format!(
                "{name} isn't a Rust name; for a position in a file, use --path {name} --line <N> --character <N>"
            ));
        }
        specs.push(TargetSpec::Name(name.clone()));
    }
    match (path, line, character) {
        // clap's parser has already rejected 0, so the subtraction can't wrap.
        (Some(path), Some(line), Some(character)) => specs.push(TargetSpec::Position {
            path: path.to_path_buf(),
            line: line - 1,
            character: character - 1,
        }),
        (None, None, None) => {}
        _ => return Err("--path, --line and --character are given together".into()),
    }
    if names.is_empty() && (symbol_args.kind.is_some() || symbol_args.glob.is_some()) {
        return Err("--kind and --glob only apply to symbol names, and none was given".into());
    }
    Ok(specs)
}

/// Prints a usage error in clap's style and exits with clap's status for one.
pub fn exit_with_usage_error(message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(2);
}

/// Whether `arg` is a Rust path such as `mkfifo`, `sys::mkfifo`, `Fifo::name` or `r#type`.
pub(crate) fn is_rust_path(arg: &str) -> bool {
    arg.split("::").all(|segment| {
        let ident = segment.strip_prefix("r#").unwrap_or(segment);
        let mut chars = ident.chars();
        chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
            && chars.all(|c| c.is_alphanumeric() || c == '_')
    })
}

/// A position a command runs from: the file and position, plus the symbol it's labelled with.
pub struct Target {
    pub symbol: Option<SymbolMatch>,
    pub file_path: String,
    pub line: u32,
    pub character: u32,
}

/// Where a position command runs from, and whether its results are grouped by symbol.
pub struct Targets {
    pub targets: Vec<Target>,
    /// Whether any symbol name was given. Then every target is labelled with its symbol, and the
    /// results are grouped by it.
    pub grouped: bool,
    /// Names that found no definition, and positions with no symbol, which don't stop the others.
    pub errors: Vec<String>,
}

/// Checks a position command's names and position, then finds where it runs from, within 60
/// seconds. Names that find nothing, and positions with no symbol, go to `out`'s standard error.
pub async fn command_targets(
    backend: &dyn RacliBackend,
    names: &[String],
    path: Option<&Path>,
    line: Option<u32>,
    character: Option<u32>,
    args: &SymbolArgs,
    out: &mut CommandOutput,
) -> Result<Targets, CommandError> {
    let specs = classify(names, path, line, character, args).map_err(CommandError::Usage)?;
    let mut found = within_timeout(targets(backend, args, specs))
        .await?
        .map_err(CommandError::Failed)?;
    for err in found.errors.drain(..) {
        out.backend_error(err);
    }
    Ok(found)
}

/// Finds where a position command runs from: each definition the names find, and the position.
/// A definition found twice, by two names or by a name and the position, is run once.
pub async fn targets(
    backend: &dyn RacliBackend,
    args: &SymbolArgs,
    specs: Vec<TargetSpec>,
) -> Result<Targets, String> {
    let grouped = specs.iter().any(|s| matches!(s, TargetSpec::Name(_)));
    let cwd = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .map_err(|e| format!("cannot read the current directory: {e}"))?;
    let filter = SymbolFilter {
        kind: args.kind.as_deref(),
        glob: args.glob.as_ref(),
        cwd: &cwd,
    };
    let mut result = Targets {
        targets: Vec::new(),
        grouped,
        errors: Vec::new(),
    };
    let mut seen = HashSet::new();
    for spec in specs {
        match spec {
            TargetSpec::Name(name) => match resolve_symbol(backend, &name, &filter).await {
                Ok((matches, truncated)) => {
                    if truncated {
                        result.errors.push(format!("{name}: {TRUNCATED_NOTE}"));
                    }
                    for m in matches {
                        if !seen.insert((m.uri.clone(), m.range)) {
                            continue;
                        }
                        let Some(file_path) = m.file_path() else {
                            result.errors.push(format!("{} isn't a local file", m.uri));
                            continue;
                        };
                        result.targets.push(Target {
                            file_path: file_path.display().to_string(),
                            line: m.range.start.line,
                            character: m.range.start.character,
                            symbol: Some(m),
                        });
                    }
                }
                Err(ResolveSymbolError::Search(e)) => return Err(e.to_string()),
                Err(e) => result.errors.push(e.to_string()),
            },
            TargetSpec::Position {
                path,
                line,
                character,
            } => {
                let file_path = path
                    .canonicalize()
                    .map_err(|e| format!("cannot canonicalize {}: {e}", path.display()))?
                    .display()
                    .to_string();
                let symbol = if grouped {
                    match symbol_at(backend, &file_path, line, character).await? {
                        Some(m) if !seen.insert((m.uri.clone(), m.range)) => continue,
                        Some(m) => Some(m),
                        None => {
                            result.errors.push(format!(
                                "no symbol at {}:{}:{}",
                                path.display(),
                                line + 1,
                                character + 1
                            ));
                            continue;
                        }
                    }
                } else {
                    None
                };
                result.targets.push(Target {
                    symbol,
                    file_path,
                    line,
                    character,
                });
            }
        }
    }
    Ok(result)
}

/// The symbol whose definition the position refers to, to label a position given with names.
async fn symbol_at(
    backend: &dyn RacliBackend,
    file_path: &str,
    line: u32,
    character: u32,
) -> Result<Option<SymbolMatch>, String> {
    let definition = backend.find_definition(file_path, line, character).await?;
    let Some((uri, lsp_range)) = definition
        .locations
        .into_iter()
        .find_map(|loc| Some((loc.uri, loc.range?)))
    else {
        return Ok(None);
    };
    let Some(range) = range_json(lsp_range) else {
        return Ok(None);
    };
    let Some(path) = url::Url::parse(&uri)
        .ok()
        .and_then(|u| u.to_file_path().ok())
    else {
        return Ok(None);
    };
    let outline = backend
        .document_symbols(&path.display().to_string())
        .await?;
    let Some(symbol) = outline_symbol_at(&outline.symbols, &lsp_range) else {
        return Ok(None);
    };
    // Label it as a name lookup would, so its container matches; the outline has no containers.
    for scope in [
        SymbolSearchScope::Workspace,
        SymbolSearchScope::WorkspaceAndDependencies,
    ] {
        let found = exact_matches(search(backend, &symbol.name, scope).await?, &symbol.name);
        if let Some(m) = found.into_iter().find(|m| m.uri == uri && m.range == range) {
            return Ok(Some(m));
        }
    }
    Ok(Some(SymbolMatch {
        name: symbol.name.clone(),
        kind: symbol.kind.clone(),
        container_name: None,
        uri,
        range,
    }))
}

/// The symbol in a file outline whose name is at `range`, searching nested symbols too.
pub(crate) fn outline_symbol_at<'a>(
    symbols: &'a [LspDocumentSymbol],
    range: &LspRange,
) -> Option<&'a LspDocumentSymbol> {
    symbols.iter().find_map(|s| {
        if s.selection_range.as_ref() == Some(range) {
            Some(s)
        } else {
            outline_symbol_at(&s.children, range)
        }
    })
}

/// `--kind` and `--glob`, applied to the definitions found by name.
struct SymbolFilter<'a> {
    kind: Option<&'a str>,
    glob: Option<&'a glob::Pattern>,
    /// The canonical current directory, which a relative `--glob` is relative to.
    cwd: &'a Path,
}

impl SymbolFilter<'_> {
    /// Whether `m` has the kind and lies in a file the glob matches.
    fn keeps(&self, m: &SymbolMatch) -> bool {
        let kind_ok = self.kind.is_none_or(|k| m.kind.eq_ignore_ascii_case(k));
        let glob_ok = self
            .glob
            .is_none_or(|g| m.file_path().is_some_and(|f| self.glob_matches(g, &f)));
        kind_ok && glob_ok
    }

    /// Whether `glob` matches `file`: as is for an absolute pattern, or relative to the current
    /// directory for a relative one.
    fn glob_matches(&self, glob: &glob::Pattern, file: &Path) -> bool {
        if Path::new(glob.as_str()).is_absolute() {
            return glob.matches_path_with(file, GLOB_OPTIONS);
        }
        file.strip_prefix(self.cwd)
            .is_ok_and(|relative| glob.matches_path_with(relative, GLOB_OPTIONS))
    }
}

/// Finds the definitions named `name`, searching the workspace first and then its dependencies.
/// Also says whether the search that found them was cut to the server's limit.
async fn resolve_symbol(
    backend: &dyn RacliBackend,
    name: &str,
    filter: &SymbolFilter<'_>,
) -> Result<(Vec<SymbolMatch>, bool), ResolveSymbolError> {
    let (container, leaf) = split_qualified(name);
    let mut suggestions = Vec::new();
    let mut unfiltered = Vec::new();
    let mut any_truncated = false;
    for scope in [
        SymbolSearchScope::Workspace,
        SymbolSearchScope::WorkspaceAndDependencies,
    ] {
        // rust-analyzer resolves a module path such as `raw::mkfifo` itself.
        let results = search(backend, name, scope)
            .await
            .map_err(ResolveSymbolError::Search)?;
        let mut truncated = results.truncated;
        if suggestions.is_empty() {
            suggestions = all_symbols(results.clone());
        }
        let mut found = exact_matches(results, leaf);
        if found.is_empty()
            && let Some(container) = container
        {
            // It doesn't resolve `Type::member`, so match the container's name instead.
            let leaf_results = search(backend, leaf, scope)
                .await
                .map_err(ResolveSymbolError::Search)?;
            truncated |= leaf_results.truncated;
            if scope == SymbolSearchScope::Workspace {
                suggestions = all_symbols(leaf_results.clone());
            }
            found = exact_matches(leaf_results, leaf)
                .into_iter()
                .filter(|m| m.container_name.as_deref() == Some(last_segment(container)))
                .collect();
        }
        any_truncated |= truncated;
        let kept: Vec<SymbolMatch> = found.iter().filter(|m| filter.keeps(m)).cloned().collect();
        if !kept.is_empty() {
            return Ok((kept, truncated));
        }
        if unfiltered.is_empty() {
            unfiltered = found;
        }
    }
    if !unfiltered.is_empty() {
        return Err(ResolveSymbolError::NotFound {
            name: name.to_string(),
            filtered: true,
            truncated: any_truncated,
            suggestions: unfiltered,
        });
    }
    suggestions.truncate(MAX_SUGGESTIONS);
    Err(ResolveSymbolError::NotFound {
        name: name.to_string(),
        filtered: false,
        truncated: any_truncated,
        suggestions,
    })
}

/// Runs `workspace/symbol` for `query` over all symbol kinds in `scope`.
async fn search(
    backend: &dyn RacliBackend,
    query: &str,
    scope: SymbolSearchScope,
) -> Result<SearchResponse, String> {
    let options = SymbolSearchOptions {
        kind: Some(SymbolSearchKind::AllSymbols),
        scope: Some(scope),
    };
    backend.search(query, options).await
}

/// Splits `a::b::c` into its qualifier `a::b` and its name `c`.
fn split_qualified(name: &str) -> (Option<&str>, &str) {
    match name.rsplit_once("::") {
        Some((qualifier, leaf)) => (Some(qualifier), leaf),
        None => (None, name),
    }
}

/// The last segment of a path such as `a::b`.
fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// The symbols in `resp` named exactly `name`.
fn exact_matches(resp: SearchResponse, name: &str) -> Vec<SymbolMatch> {
    all_symbols(resp)
        .into_iter()
        .filter(|m| m.name == name)
        .collect()
}

/// Every symbol in `resp` that has a range, in rust-analyzer's order, without duplicates.
fn all_symbols(resp: SearchResponse) -> Vec<SymbolMatch> {
    let payload = resp.workspace_symbol_response.and_then(|r| r.payload);
    let symbols: Vec<SymbolMatch> = match payload {
        Some(Payload::Flat(list)) => list
            .items
            .into_iter()
            .filter_map(|it| symbol_match(it.name, it.kind, it.container_name, it.uri, it.range))
            .collect(),
        Some(Payload::Nested(list)) => list
            .items
            .into_iter()
            .filter_map(|it| symbol_match(it.name, it.kind, it.container_name, it.uri, it.range))
            .collect(),
        None => Vec::new(),
    };
    let mut unique: Vec<SymbolMatch> = Vec::with_capacity(symbols.len());
    for s in symbols {
        if !unique.iter().any(|u| u.uri == s.uri && u.range == s.range) {
            unique.push(s);
        }
    }
    unique
}

/// A [`SymbolMatch`] from a symbol's fields, or `None` if it has no range to run a command from.
fn symbol_match(
    name: String,
    kind: String,
    container_name: Option<String>,
    uri: String,
    range: Option<LspRange>,
) -> Option<SymbolMatch> {
    Some(SymbolMatch {
        name,
        kind,
        container_name,
        uri,
        range: range_json(range?)?,
    })
}

/// A [`RangeJson`] from a protobuf range, or `None` if it lacks a start or an end.
pub(crate) fn range_json(range: LspRange) -> Option<RangeJson> {
    let position = |p: Option<crate::proto::racli::LspPosition>| {
        p.map(|p| PositionJson {
            line: p.line,
            character: p.character,
        })
    };
    Some(RangeJson {
        start: position(range.start)?,
        end: position(range.end)?,
    })
}

/// Writes each definition's result as JSON: `[{"symbol": …, "result": …}]`.
pub fn write_symbol_results_json<T: Serialize>(
    out: &mut CommandOutput,
    results: &[SymbolResultJson<'_, T>],
) {
    match serde_json::to_string(results) {
        Ok(json) => out.println(json),
        Err(e) => out.note(format!("failed to serialize JSON: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::racli::LspPosition;
    use crate::proto::racli::LspSymbolInformation;
    use crate::proto::racli::LspSymbolInformationList;
    use crate::proto::racli::LspWorkspaceSymbolResponse;

    fn symbol(name: &str, container: Option<&str>, uri: &str, line: u32) -> LspSymbolInformation {
        LspSymbolInformation {
            name: name.into(),
            kind: "FUNCTION".into(),
            uri: uri.into(),
            range: Some(LspRange {
                start: Some(LspPosition { line, character: 4 }),
                end: Some(LspPosition {
                    line,
                    character: 10,
                }),
            }),
            container_name: container.map(Into::into),
        }
    }

    fn response(items: Vec<LspSymbolInformation>) -> SearchResponse {
        SearchResponse {
            truncated: false,
            workspace_symbol_response: Some(LspWorkspaceSymbolResponse {
                payload: Some(Payload::Flat(LspSymbolInformationList { items })),
            }),
        }
    }

    #[test]
    fn split_qualified_separates_the_last_segment() {
        assert_eq!(split_qualified("mkfifo"), (None, "mkfifo"));
        assert_eq!(split_qualified("raw::mkfifo"), (Some("raw"), "mkfifo"));
        assert_eq!(split_qualified("a::B::c"), (Some("a::B"), "c"));
        assert_eq!(last_segment("a::B"), "B");
    }

    #[test]
    fn exact_matches_drop_fuzzy_hits_and_duplicates() {
        let resp = response(vec![
            symbol("mkfifo", None, "file:///a.rs", 1),
            symbol("mkfifo_raw", None, "file:///a.rs", 2),
            symbol("mkfifo", None, "file:///a.rs", 1),
            symbol("mkfifo", Some("raw"), "file:///a.rs", 3),
        ]);
        let found = exact_matches(resp, "mkfifo");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].range.start.line, 1);
        assert_eq!(found[1].container_name.as_deref(), Some("raw"));
    }

    #[test]
    fn filter_keeps_kind_case_insensitively_and_files_the_glob_matches() {
        let m = exact_matches(
            response(vec![symbol("f", None, "file:///w/src/sys/a.rs", 1)]),
            "f",
        )
        .remove(0);
        let keeps = |kind: Option<&str>, glob: Option<&str>| {
            let glob = glob.map(|g| glob::Pattern::new(g).expect("valid glob"));
            SymbolFilter {
                kind,
                glob: glob.as_ref(),
                cwd: Path::new("/w"),
            }
            .keeps(&m)
        };
        assert!(keeps(None, None));
        assert!(keeps(Some("function"), None));
        assert!(!keeps(Some("method"), None));
        assert!(keeps(None, Some("src/**/*.rs")));
        assert!(keeps(None, Some("src/**")));
        assert!(keeps(None, Some("/w/src/sys/a.rs")));
        assert!(
            !keeps(None, Some("src/*.rs")),
            "`*` stays within one component"
        );
        assert!(!keeps(None, Some("tests/**")));
    }

    #[test]
    fn header_matches_search_text_layout() {
        let m = exact_matches(
            response(vec![symbol("name", Some("Fifo"), "file:///a.rs", 36)]),
            "name",
        )
        .remove(0);
        assert_eq!(m.header(), "name [FUNCTION] in Fifo /a.rs:37:5");
    }

    #[test]
    fn not_found_lists_suggestions() {
        let suggestions = exact_matches(
            response(vec![symbol("mkfifo", None, "file:///a.rs", 1)]),
            "mkfifo",
        );
        let e = ResolveSymbolError::NotFound {
            name: "mkfif".into(),
            filtered: false,
            truncated: false,
            suggestions,
        };
        assert_eq!(
            e.to_string(),
            "no symbol named mkfif; closest:\n  mkfifo [FUNCTION] /a.rs:2:5"
        );
        let e = ResolveSymbolError::NotFound {
            name: "len".into(),
            filtered: false,
            truncated: true,
            suggestions: Vec::new(),
        };
        assert_eq!(
            e.to_string(),
            format!("no symbol named len ({TRUNCATED_NOTE})")
        );
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn rust_paths_are_names_and_anything_else_is_a_file() {
        for name in [
            "mkfifo",
            "sys::mkfifo",
            "Fifo::name",
            "r#type",
            "_private",
            "crate::a",
        ] {
            assert!(is_rust_path(name), "{name} should be a Rust path");
        }
        for file in ["src/sys.rs", "main.rs", "Vec<T>", "a::", "::a", "1st", ""] {
            assert!(!is_rust_path(file), "{file} should not be a Rust path");
        }
    }

    #[test]
    fn classify_lists_names_then_the_position_converted_to_zero_based() {
        let specs = classify(
            &strings(&["mkfifo", "Fifo::name"]),
            Some(Path::new("src/sys.rs")),
            Some(2),
            Some(7),
            &SymbolArgs::default(),
        )
        .expect("valid arguments");
        assert_eq!(
            specs,
            vec![
                TargetSpec::Name("mkfifo".into()),
                TargetSpec::Name("Fifo::name".into()),
                TargetSpec::Position {
                    path: "src/sys.rs".into(),
                    line: 1,
                    character: 6,
                },
            ]
        );
    }

    #[test]
    fn classify_rejects_bad_combinations() {
        let none = SymbolArgs::default();
        let err = |names: &[&str], path: Option<&str>, symbol_args: &SymbolArgs| {
            let line = path.map(|_| 1);
            classify(
                &strings(names),
                path.map(Path::new),
                line,
                line,
                symbol_args,
            )
            .expect_err("should be rejected")
        };
        assert!(err(&["src/sys.rs"], None, &none).contains("use --path src/sys.rs"));
        let kind = SymbolArgs {
            kind: Some("function".into()),
            glob: None,
        };
        assert!(err(&[], Some("src/sys.rs"), &kind).contains("only apply to symbol names"));
        assert!(
            classify(&[], Some(Path::new("a.rs")), Some(1), None, &none)
                .expect_err("should be rejected")
                .contains("given together")
        );
    }

    #[test]
    fn outline_symbol_at_finds_nested_symbols_by_name_range() {
        let range = |line| LspRange {
            start: Some(LspPosition { line, character: 0 }),
            end: Some(LspPosition { line, character: 4 }),
        };
        let symbol = |name: &str, line, children| LspDocumentSymbol {
            name: name.into(),
            detail: None,
            kind: "FUNCTION".into(),
            range: Some(range(line)),
            selection_range: Some(range(line)),
            children,
        };
        let outline = vec![symbol("outer", 1, vec![symbol("inner", 2, vec![])])];
        assert_eq!(
            outline_symbol_at(&outline, &range(2)).map(|s| s.name.as_str()),
            Some("inner")
        );
        assert!(outline_symbol_at(&outline, &range(3)).is_none());
    }
}

//! MCP tool parameters (ADR 006): each tool takes its CLI command's options, and converts them into
//! that command's arguments, so the tool runs the command itself.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::call_hierarchy::CallHierarchyArgs;
use crate::call_hierarchy::Direction;
use crate::document_symbols::DocumentSymbolsArgs;
use crate::document_symbols::DocumentSymbolsOutputFormat;
use crate::find_definition::FindDefinitionArgs;
use crate::find_implementations::FindImplementationsArgs;
use crate::find_references::FindReferencesArgs;
use crate::rust_analyzer::SymbolSearchKind;
use crate::rust_analyzer::SymbolSearchScope;
use crate::search::SearchArgs;
use crate::search::SearchOutputFormat;
use crate::symbol::SymbolArgs;
use crate::symbol::is_rust_path;

/// How a tool returns its command's output.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// What the command prints with `--text`: one line per result, with 1-based `path:line:col`.
    #[default]
    Text,
    /// The command's JSON, with LSP's absolute URIs and 0-based ranges.
    Json,
}

/// Parameters of the `search` tool, as `racli search` takes them.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Fuzzy workspace symbol query; `|` separates alternative patterns, and `\|` is a literal pipe.
    pub query: String,
    /// `only_types` (default: modules, structs, enums, traits, type aliases) or `all_symbols` (also
    /// functions, methods, constants, fields).
    #[serde(default)]
    pub kind: Option<SymbolSearchKind>,
    /// `workspace` (default) or `workspace_and_dependencies` (also dependency crates and std).
    #[serde(default)]
    pub scope: Option<SymbolSearchScope>,
    /// Keep only symbols named exactly as a pattern, dropping rust-analyzer's fuzzy matches.
    #[serde(default)]
    pub exact: bool,
    /// `text` (default) or `json`.
    #[serde(default)]
    pub format: Format,
}

/// Parameters of the `find_references` and `find_implementations` tools, as their commands take
/// them: symbol names, a position, or both.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PositionParams {
    /// Symbol names to look up, e.g. `mkfifo`, `sys::mkfifo` (a module path from the crate root) or
    /// `Fifo::name` (the type or trait that contains it).
    #[serde(default)]
    pub names: Vec<String>,
    /// Rust source file of a position to run from, relative to the workspace root or absolute;
    /// given with `line` and `character`.
    #[serde(default)]
    pub path: Option<String>,
    /// 1-based line of the position in `path`.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub line: Option<u32>,
    /// 1-based column of the position, in UTF-16 units.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub character: Option<u32>,
    /// Only use definitions of this kind, e.g. `function`, `method` or `struct` (names only).
    #[serde(default)]
    pub kind: Option<String>,
    /// Only use definitions in files this glob matches, e.g. `src/**`, relative to the workspace
    /// root (names only).
    #[serde(default)]
    pub glob: Option<String>,
    /// `text` (default) or `json`.
    #[serde(default)]
    pub format: Format,
}

/// Parameters of the `find_definition` tool, as `racli find-definition` takes them.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindDefinitionParams {
    /// Symbol names to look up, e.g. `mkfifo`, `sys::mkfifo` (a module path from the crate root) or
    /// `Fifo::name` (the type or trait that contains it).
    #[serde(default)]
    pub names: Vec<String>,
    /// Rust source file of a position to run from, relative to the workspace root or absolute;
    /// given with `line` and `character`.
    #[serde(default)]
    pub path: Option<String>,
    /// 1-based line of the position in `path`.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub line: Option<u32>,
    /// 1-based column of the position, in UTF-16 units.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub character: Option<u32>,
    /// Only use definitions of this kind, e.g. `function`, `method` or `struct` (names only).
    #[serde(default)]
    pub kind: Option<String>,
    /// Only use definitions in files this glob matches, e.g. `src/**`, relative to the workspace
    /// root (names only).
    #[serde(default)]
    pub glob: Option<String>,
    /// Also return each definition's signature and doc comment (rust-analyzer's hover).
    #[serde(default)]
    pub hover: bool,
    /// Also return each definition's whole source, including its doc comment.
    #[serde(default)]
    pub source: bool,
    /// `text` (default) or `json`.
    #[serde(default)]
    pub format: Format,
}

/// Parameters of the `call_hierarchy` tool, as `racli call-hierarchy` takes them.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CallHierarchyParams {
    /// Symbol names to look up, e.g. `mkfifo`, `sys::mkfifo` (a module path from the crate root) or
    /// `Fifo::name` (the type or trait that contains it).
    #[serde(default)]
    pub names: Vec<String>,
    /// Rust source file of a position to run from, relative to the workspace root or absolute;
    /// given with `line` and `character`.
    #[serde(default)]
    pub path: Option<String>,
    /// 1-based line of the position in `path`.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub line: Option<u32>,
    /// 1-based column of the position, in UTF-16 units.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub character: Option<u32>,
    /// Only use definitions of this kind, e.g. `function` or `method` (names only).
    #[serde(default)]
    pub kind: Option<String>,
    /// Only use definitions in files this glob matches, e.g. `src/**`, relative to the workspace
    /// root (names only).
    #[serde(default)]
    pub glob: Option<String>,
    /// `incoming` (callers), `outgoing` (callees) or `both` (default).
    #[serde(default)]
    pub direction: Option<Direction>,
    /// How many levels of callers or callees to fetch (default `1`).
    #[serde(default)]
    pub depth: Option<u32>,
    /// `text` (default) or `json`.
    #[serde(default)]
    pub format: Format,
}

/// Parameters of the `document_symbols` tool, as `racli document-symbols` takes them.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DocumentSymbolsParams {
    /// Rust source file to outline, relative to the workspace root or absolute.
    pub path: String,
    /// `text` (default) or `json`.
    #[serde(default)]
    pub format: Format,
}

/// The names and position a position tool runs from, checked and converted to CLI arguments.
struct Targets {
    names: Vec<String>,
    path: Option<PathBuf>,
    line: Option<u32>,
    character: Option<u32>,
    symbol: SymbolArgs,
}

/// Checks a position tool's names and position as the CLI does, with errors that name the MCP
/// parameters, and converts them to the CLI's arguments.
fn targets(
    names: Vec<String>,
    path: Option<String>,
    line: Option<u32>,
    character: Option<u32>,
    kind: Option<String>,
    glob: Option<String>,
) -> Result<Targets, String> {
    if line == Some(0) || character == Some(0) {
        return Err("line and character are 1-based, so 0 isn't a position".into());
    }
    let has_position = match (&path, line, character) {
        (Some(_), Some(_), Some(_)) => true,
        (None, None, None) => false,
        _ => return Err("path, line and character are given together".into()),
    };
    if names.is_empty() && !has_position {
        return Err("give symbol names (names), or a position (path, line and character)".into());
    }
    if names.is_empty() && (kind.is_some() || glob.is_some()) {
        return Err("kind and glob only apply to symbol names, and none was given".into());
    }
    if let Some(name) = names.iter().find(|name| !is_rust_path(name)) {
        return Err(format!(
            "{name} isn't a Rust name; for a position in a file, give path, line and character"
        ));
    }
    let glob = glob
        .map(|pattern| {
            glob::Pattern::new(&pattern).map_err(|e| format!("glob {pattern:?} is invalid: {e}"))
        })
        .transpose()?;
    Ok(Targets {
        names,
        path: path.map(PathBuf::from),
        line,
        character,
        symbol: SymbolArgs { kind, glob },
    })
}

impl SearchParams {
    /// The `racli search` arguments these parameters stand for.
    pub fn into_args(self) -> SearchArgs {
        let output_format = match self.format {
            Format::Text => SearchOutputFormat::Text,
            Format::Json => SearchOutputFormat::Json,
        };
        SearchArgs {
            text: false,
            json: false,
            csv: false,
            output_format: Some(output_format),
            kind: self.kind,
            scope: self.scope,
            exact: self.exact,
            query: self.query,
        }
    }
}

impl PositionParams {
    /// The `racli find-references` arguments these parameters stand for.
    pub fn into_find_references_args(self) -> Result<FindReferencesArgs, String> {
        let text = self.format == Format::Text;
        let t = targets(
            self.names,
            self.path,
            self.line,
            self.character,
            self.kind,
            self.glob,
        )?;
        Ok(FindReferencesArgs {
            names: t.names,
            path: t.path,
            line: t.line,
            character: t.character,
            symbol: t.symbol,
            text,
        })
    }

    /// The `racli find-implementations` arguments these parameters stand for.
    pub fn into_find_implementations_args(self) -> Result<FindImplementationsArgs, String> {
        let text = self.format == Format::Text;
        let t = targets(
            self.names,
            self.path,
            self.line,
            self.character,
            self.kind,
            self.glob,
        )?;
        Ok(FindImplementationsArgs {
            names: t.names,
            path: t.path,
            line: t.line,
            character: t.character,
            symbol: t.symbol,
            text,
        })
    }
}

impl FindDefinitionParams {
    /// The `racli find-definition` arguments these parameters stand for.
    pub fn into_args(self) -> Result<FindDefinitionArgs, String> {
        let t = targets(
            self.names,
            self.path,
            self.line,
            self.character,
            self.kind,
            self.glob,
        )?;
        Ok(FindDefinitionArgs {
            names: t.names,
            path: t.path,
            line: t.line,
            character: t.character,
            symbol: t.symbol,
            hover: self.hover,
            source: self.source,
            text: self.format == Format::Text,
        })
    }
}

impl CallHierarchyParams {
    /// The `racli call-hierarchy` arguments these parameters stand for.
    pub fn into_args(self) -> Result<CallHierarchyArgs, String> {
        let t = targets(
            self.names,
            self.path,
            self.line,
            self.character,
            self.kind,
            self.glob,
        )?;
        Ok(CallHierarchyArgs {
            names: t.names,
            path: t.path,
            line: t.line,
            character: t.character,
            symbol: t.symbol,
            direction: self.direction.unwrap_or(Direction::Both),
            depth: self.depth.unwrap_or(1),
            text: self.format == Format::Text,
        })
    }
}

impl DocumentSymbolsParams {
    /// The `racli document-symbols` arguments these parameters stand for.
    pub fn into_args(self) -> DocumentSymbolsArgs {
        let output_format = match self.format {
            Format::Text => DocumentSymbolsOutputFormat::Text,
            Format::Json => DocumentSymbolsOutputFormat::Json,
        };
        DocumentSymbolsArgs {
            path: PathBuf::from(self.path),
            text: false,
            json: false,
            output_format: Some(output_format),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_params_parse_optional_kind_and_scope() {
        let params: SearchParams = serde_json::from_value(serde_json::json!({
            "query": "Foo",
            "kind": "all_symbols",
            "scope": "workspace_and_dependencies"
        }))
        .unwrap();
        assert_eq!(params.kind, Some(SymbolSearchKind::AllSymbols));
        assert_eq!(
            params.scope,
            Some(SymbolSearchScope::WorkspaceAndDependencies)
        );

        let params: SearchParams =
            serde_json::from_value(serde_json::json!({ "query": "Foo" })).unwrap();
        assert_eq!(params.kind, None);
        assert_eq!(params.scope, None);
        assert_eq!(params.format, Format::Text);
    }
}

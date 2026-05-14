//! Server-side building blocks: [`Core`] and future service glue.

use std::collections::HashSet;
use std::hash::Hash;

use lsp_types::OneOf;
use lsp_types::SymbolInformation;
use lsp_types::SymbolKind;
use lsp_types::WorkspaceSymbol;
use lsp_types::WorkspaceSymbolResponse;
use serde_json::Value;

use crate::rust_analyzer::RustAnalyzerError;
use crate::rust_analyzer::RustAnalyzerSession;

/// Holds stateless helpers shared by gRPC handlers (e.g. [`Core::version`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Core {}

impl Core {
    /// Returns [`crate::VERSION`] as an owned string for protobuf responses.
    pub fn version(&self) -> String {
        crate::VERSION.to_string()
    }

    /// Runs LSP `workspace/symbol` on the live rust-analyzer session and returns the raw JSON `result`.
    ///
    /// Unescaped `|` splits the query into multiple patterns (each non-empty segment is searched; results are merged and deduped).
    /// Use `\|` for a literal pipe. This is substring alternation, not full regular-expression syntax.
    pub async fn search(
        &self,
        ra: &mut RustAnalyzerSession,
        query: String,
    ) -> Result<Value, RustAnalyzerError> {
        let segments = split_search_query_into_segments(&query);
        if segments.len() == 1 {
            return ra.workspace_symbol(segments[0].clone()).await;
        }

        search_merge_multiple_patterns(ra, segments).await
    }

    /// Runs LSP `textDocument/definition` on the live rust-analyzer session and returns the raw JSON `result`.
    pub async fn find_definition(
        &self,
        ra: &mut RustAnalyzerSession,
        document_uri: String,
        line: u32,
        character: u32,
    ) -> Result<Value, RustAnalyzerError> {
        ra.text_document_definition(document_uri, line, character)
            .await
    }
}

/// Splits `query` on unescaped ASCII `|` into trimmed segments; `\|` produces a literal `|` in the segment.
fn split_search_query_into_segments(query: &str) -> Vec<String> {
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = query.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('|') => {
                    chars.next();
                    current.push('|');
                }
                _ => current.push('\\'),
            }
        } else if c == '|' {
            segments.push(current);
            current = String::new();
        } else {
            current.push(c);
        }
    }
    segments.push(current);

    let trimmed: Vec<String> = segments
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if trimmed.is_empty() {
        vec![String::new()]
    } else {
        trimmed
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct FlatSymbolDedupKey {
    name: String,
    kind: i32,
    uri: String,
    start_line: u32,
    start_character: u32,
    end_line: u32,
    end_character: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct NestedSymbolDedupKey {
    name: String,
    kind: i32,
    uri: String,
    range: Option<(u32, u32, u32, u32)>,
}

/// Extracts the JSON number backing `SymbolKind` for stable hash keys (the struct field is crate-private).
fn symbol_kind_i32(kind: SymbolKind) -> i32 {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32
}

fn flat_dedup_key(si: &SymbolInformation) -> FlatSymbolDedupKey {
    let r = si.location.range;
    FlatSymbolDedupKey {
        name: si.name.clone(),
        kind: symbol_kind_i32(si.kind),
        uri: si.location.uri.as_str().to_string(),
        start_line: r.start.line,
        start_character: r.start.character,
        end_line: r.end.line,
        end_character: r.end.character,
    }
}

fn nested_dedup_key(ws: &WorkspaceSymbol) -> NestedSymbolDedupKey {
    let (uri, range) = match &ws.location {
        OneOf::Left(loc) => {
            let r = loc.range;
            (
                loc.uri.as_str().to_string(),
                Some((
                    r.start.line,
                    r.start.character,
                    r.end.line,
                    r.end.character,
                )),
            )
        }
        OneOf::Right(wl) => (wl.uri.as_str().to_string(), None),
    };
    NestedSymbolDedupKey {
        name: ws.name.clone(),
        kind: symbol_kind_i32(ws.kind),
        uri,
        range,
    }
}

/// Inserts each `item` into `vec` when `key_fn(&item)` is not already in `seen`.
fn extend_deduped<T, K: Eq + Hash>(
    vec: &mut Vec<T>,
    seen: &mut HashSet<K>,
    items: impl IntoIterator<Item = T>,
    key_fn: impl Fn(&T) -> K,
) {
    for item in items {
        let k = key_fn(&item);
        if seen.insert(k) {
            vec.push(item);
        }
    }
}

/// Deserializes a `workspace/symbol` JSON `result`: `null` becomes `None`.
fn workspace_symbol_value_to_option(
    value: Value,
) -> Result<Option<WorkspaceSymbolResponse>, RustAnalyzerError> {
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value).map(Some).map_err(RustAnalyzerError::from)
}

enum WsMergeState {
    Empty,
    Flat(Vec<SymbolInformation>, HashSet<FlatSymbolDedupKey>),
    Nested(Vec<WorkspaceSymbol>, HashSet<NestedSymbolDedupKey>),
}

/// Runs one `workspace/symbol` per segment and merges lists of the same LSP shape with stable dedupe.
async fn search_merge_multiple_patterns(
    ra: &mut RustAnalyzerSession,
    segments: Vec<String>,
) -> Result<Value, RustAnalyzerError> {
    let mut state = WsMergeState::Empty;

    let segment_count = segments.len();

    for (i, seg) in segments.into_iter().enumerate() {
        let segment_query = seg.clone();
        tracing::debug!(
            rpc = "Racli.Search",
            lsp_method = "workspace/symbol",
            segment_index = i + 1,
            segment_count,
            segment_query = %segment_query,
            "merged multi-pattern search: LSP workspace/symbol segment"
        );
        let value = ra.workspace_symbol(seg).await?;
        let parsed = workspace_symbol_value_to_option(value)?;

        let segment_result_count = match &parsed {
            None => 0,
            Some(WorkspaceSymbolResponse::Flat(items)) => items.len(),
            Some(WorkspaceSymbolResponse::Nested(items)) => items.len(),
        };
        tracing::debug!(
            rpc = "Racli.Search",
            lsp_method = "workspace/symbol",
            segment_index = i + 1,
            segment_count,
            segment_query = %segment_query,
            segment_result_count,
            "merged multi-pattern search: segment workspace/symbol returned"
        );

        state = match (state, parsed) {
            (WsMergeState::Empty, None) => WsMergeState::Empty,
            (WsMergeState::Flat(vec, seen), None) => WsMergeState::Flat(vec, seen),
            (WsMergeState::Nested(vec, seen), None) => WsMergeState::Nested(vec, seen),

            (WsMergeState::Empty, Some(WorkspaceSymbolResponse::Flat(items))) => {
                let mut vec = Vec::new();
                let mut seen: HashSet<FlatSymbolDedupKey> = HashSet::new();
                extend_deduped(&mut vec, &mut seen, items, flat_dedup_key);
                WsMergeState::Flat(vec, seen)
            }
            (WsMergeState::Empty, Some(WorkspaceSymbolResponse::Nested(items))) => {
                let mut vec = Vec::new();
                let mut seen: HashSet<NestedSymbolDedupKey> = HashSet::new();
                extend_deduped(&mut vec, &mut seen, items, nested_dedup_key);
                WsMergeState::Nested(vec, seen)
            }

            (WsMergeState::Flat(mut vec, mut seen), Some(WorkspaceSymbolResponse::Flat(items))) => {
                extend_deduped(&mut vec, &mut seen, items, flat_dedup_key);
                WsMergeState::Flat(vec, seen)
            }
            (
                WsMergeState::Nested(mut vec, mut seen),
                Some(WorkspaceSymbolResponse::Nested(items)),
            ) => {
                extend_deduped(&mut vec, &mut seen, items, nested_dedup_key);
                WsMergeState::Nested(vec, seen)
            }

            (WsMergeState::Flat(..), Some(WorkspaceSymbolResponse::Nested(_))) => {
                return Err(RustAnalyzerError::Rpc(
                    "mixed workspace/symbol response shapes (flat vs nested)".into(),
                ));
            }
            (WsMergeState::Nested(..), Some(WorkspaceSymbolResponse::Flat(_))) => {
                return Err(RustAnalyzerError::Rpc(
                    "mixed workspace/symbol response shapes (nested vs flat)".into(),
                ));
            }
        };
    }

    let merged = match state {
        WsMergeState::Empty => None,
        WsMergeState::Flat(items, _) if items.is_empty() => None,
        WsMergeState::Nested(items, _) if items.is_empty() => None,
        WsMergeState::Flat(items, _) => Some(WorkspaceSymbolResponse::Flat(items)),
        WsMergeState::Nested(items, _) => Some(WorkspaceSymbolResponse::Nested(items)),
    };

    let Some(merged) = merged else {
        tracing::debug!(
            rpc = "Racli.Search",
            lsp_method = "workspace/symbol",
            merged_symbol_count = 0_usize,
            "merged multi-pattern search: aggregated and deduped"
        );
        return Ok(Value::Null);
    };

    let merged_symbol_count = match &merged {
        WorkspaceSymbolResponse::Flat(items) => items.len(),
        WorkspaceSymbolResponse::Nested(items) => items.len(),
    };
    tracing::debug!(
        rpc = "Racli.Search",
        lsp_method = "workspace/symbol",
        merged_symbol_count,
        "merged multi-pattern search: aggregated and deduped"
    );

    serde_json::to_value(merged).map_err(RustAnalyzerError::from)
}

#[cfg(test)]
mod tests {
    use super::split_search_query_into_segments;

    #[test]
    fn split_alternate_after_escaped_pipe_in_segment() {
        assert_eq!(
            split_search_query_into_segments(r"a\|b|c"),
            vec!["a|b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn split_alternates_on_pipe() {
        assert_eq!(
            split_search_query_into_segments("foo|bar"),
            vec!["foo".to_string(), "bar".to_string()]
        );
    }

    #[test]
    fn split_escapes_pipe() {
        assert_eq!(
            split_search_query_into_segments(r"foo\|bar"),
            vec!["foo|bar".to_string()]
        );
    }

    #[test]
    fn split_backslash_then_escaped_pipe() {
        assert_eq!(
            split_search_query_into_segments(r"\\|"),
            vec!["\\|".to_string()]
        );
    }

    #[test]
    fn split_only_pipes_yields_empty_sentinel() {
        assert_eq!(split_search_query_into_segments("|||"), vec![String::new()]);
    }

    #[test]
    fn split_trims_segments() {
        assert_eq!(
            split_search_query_into_segments(" a | b "),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn split_no_pipe_passthrough() {
        assert_eq!(
            split_search_query_into_segments("hello"),
            vec!["hello".to_string()]
        );
    }

    #[test]
    fn split_backslash_not_before_pipe_is_literal() {
        assert_eq!(
            split_search_query_into_segments(r"a\bc"),
            vec![r"a\bc".to_string()]
        );
    }
}

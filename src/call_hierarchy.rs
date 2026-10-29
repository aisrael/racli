//! `racli call-hierarchy`: CLI arguments, and the client-side call hierarchy orchestration
//! (prepare + recursive incoming/outgoing walk) shared between the CLI and the MCP tool.
//!
//! LSP only exposes one level of callers/callees per request; `RacliBackend`
//! abstracts over how that single level is fetched (gRPC over a Unix socket for the CLI, or an
//! in-process `RacliCore` for MCP), and `run_call_hierarchy` drives the depth-bounded,
//! cycle-guarded recursion on top of it so both callers see the same tree shape.

use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use clap::Parser;
use clap::ValueEnum;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::client::Client;
use crate::client::client_unix_socket_path;
use crate::command_output::CommandError;
use crate::command_output::CommandOutput;
use crate::command_output::within_timeout;
use crate::proto::racli::LspCallHierarchyItem;
use crate::proto::racli::LspPosition;
use crate::proto::racli::LspRange;
use crate::racli_backend::RacliBackend;
use crate::symbol;
use crate::symbol::SymbolArgs;
use crate::symbol::SymbolResultJson;
use crate::symbol::write_symbol_results_json;
use crate::text_position::parse_one_based;
use crate::text_position::position_text;

/// Which side of the call graph to walk from the resolved item.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Callers of the item (`callHierarchy/incomingCalls`).
    Incoming,
    /// Callees of the item (`callHierarchy/outgoingCalls`).
    Outgoing,
    /// Both callers and callees.
    Both,
}

/// Arguments for `racli call-hierarchy` (LSP `textDocument/prepareCallHierarchy` plus a
/// depth-bounded walk of `callHierarchy/incomingCalls`/`outgoingCalls`).
#[derive(Parser)]
pub struct CallHierarchyArgs {
    /// Symbol names to look up, e.g. `mkfifo`, `sys::mkfifo` or `Fifo::name`.
    #[arg(value_name = "NAME", required_unless_present = "path")]
    pub names: Vec<String>,
    /// Rust source file of a position to run from, with `--line` and `--character` (absolute or
    /// relative to the current directory).
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
    /// Which side of the call graph to walk.
    #[arg(long, value_enum, default_value = "both")]
    pub direction: Direction,
    /// How many recursive levels of callers/callees to fetch (each level is one more LSP round trip per node).
    #[arg(long, default_value_t = 1)]
    pub depth: u32,
    /// Print an indented outline (the default).
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
    /// Print JSON instead of text.
    #[arg(long, conflicts_with = "text")]
    pub json: bool,
}

/// Failures from [`run_call_hierarchy`].
#[derive(Debug)]
pub enum CallHierarchyError {
    /// `prepareCallHierarchy` resolved no candidate item at the given position.
    NoItem,
    /// A backend RPC (gRPC or in-process LSP) failed.
    Backend(String),
}

/// Cycle-guard key: identifies a call hierarchy item by its `uri` + `selectionRange`, ignoring
/// `detail`/`data` so the same symbol is recognized across repeated fetches.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct VisitKey {
    uri: String,
    start_line: u32,
    start_character: u32,
    end_line: u32,
    end_character: u32,
}

fn visit_key(item: &LspCallHierarchyItem) -> VisitKey {
    let (sl, sc) = item
        .selection_range
        .as_ref()
        .and_then(|r| r.start.as_ref())
        .map(|p| (p.line, p.character))
        .unwrap_or((0, 0));
    let (el, ec) = item
        .selection_range
        .as_ref()
        .and_then(|r| r.end.as_ref())
        .map(|p| (p.line, p.character))
        .unwrap_or((0, 0));
    VisitKey {
        uri: item.uri.clone(),
        start_line: sl,
        start_character: sc,
        end_line: el,
        end_character: ec,
    }
}

#[derive(Debug, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PositionJson {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RangeJson {
    pub start: PositionJson,
    pub end: PositionJson,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyItemJson {
    pub name: String,
    pub kind: String,
    pub uri: String,
    pub range: RangeJson,
    pub selection_range: RangeJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One node in a walked incoming/outgoing call tree: the caller/callee item, the ranges the call
/// appears at, and (if `depth` allowed recursing further) its own callers/callees.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallNodeJson {
    #[serde(flatten)]
    pub item: CallHierarchyItemJson,
    pub call_sites: Vec<RangeJson>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CallNodeJson>,
}

/// Result of [`run_call_hierarchy`]: the resolved item plus the requested side(s) of its call graph.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyOutput {
    pub item: CallHierarchyItemJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incoming: Option<Vec<CallNodeJson>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<Vec<CallNodeJson>>,
    /// Other items `prepareCallHierarchy` resolved at the same position, beyond the first (ambiguous cursor position).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_candidates: Vec<CallHierarchyItemJson>,
    /// Calls that failed during the walk, whose callers or callees are left out.
    #[serde(skip)]
    pub errors: Vec<String>,
}

fn position_to_json(p: &LspPosition) -> PositionJson {
    PositionJson {
        line: p.line,
        character: p.character,
    }
}

fn range_to_json(r: &LspRange) -> RangeJson {
    RangeJson {
        start: r.start.as_ref().map(position_to_json).unwrap_or_default(),
        end: r.end.as_ref().map(position_to_json).unwrap_or_default(),
    }
}

fn item_to_json(item: &LspCallHierarchyItem) -> CallHierarchyItemJson {
    CallHierarchyItemJson {
        name: item.name.clone(),
        kind: crate::lsp_map::symbol_kind_to_string(crate::lsp_map::symbol_kind_from_i32(
            item.kind,
        )),
        uri: item.uri.clone(),
        range: item.range.as_ref().map(range_to_json).unwrap_or_default(),
        selection_range: item
            .selection_range
            .as_ref()
            .map(range_to_json)
            .unwrap_or_default(),
        detail: item.detail.clone(),
    }
}

/// Fetches one level of callers (`incoming: true`) or callees for `item`, then recurses into each
/// newly-visited result while `depth_remaining` allows, guarding cycles via `visited`. A call that
/// fails adds its error to `errors` and contributes no nodes.
fn walk<'a>(
    backend: &'a dyn RacliBackend,
    incoming: bool,
    item: LspCallHierarchyItem,
    depth_remaining: u32,
    visited: &'a mut HashSet<VisitKey>,
    errors: &'a mut Vec<String>,
) -> Pin<Box<dyn Future<Output = Vec<CallNodeJson>> + Send + 'a>> {
    Box::pin(async move {
        if depth_remaining == 0 {
            return Vec::new();
        }

        let steps: Vec<(LspCallHierarchyItem, Vec<LspRange>)> = if incoming {
            match backend.incoming_calls(item).await {
                Ok(calls) => calls
                    .into_iter()
                    .filter_map(|c| c.from.map(|from| (from, c.from_ranges)))
                    .collect(),
                Err(e) => {
                    errors.push(e);
                    return Vec::new();
                }
            }
        } else {
            match backend.outgoing_calls(item).await {
                Ok(calls) => calls
                    .into_iter()
                    .filter_map(|c| c.to.map(|to| (to, c.from_ranges)))
                    .collect(),
                Err(e) => {
                    errors.push(e);
                    return Vec::new();
                }
            }
        };

        let mut nodes = Vec::with_capacity(steps.len());
        for (next_item, ranges) in steps {
            let is_new = visited.insert(visit_key(&next_item));
            let children = if is_new && depth_remaining > 1 {
                walk(
                    backend,
                    incoming,
                    next_item.clone(),
                    depth_remaining - 1,
                    visited,
                    errors,
                )
                .await
            } else {
                Vec::new()
            };
            nodes.push(CallNodeJson {
                item: item_to_json(&next_item),
                call_sites: ranges.iter().map(range_to_json).collect(),
                children,
            });
        }
        nodes
    })
}

/// Resolves the call hierarchy item at `file_path` + LSP position via `backend`, then walks the
/// requested `direction` up to `depth` levels. Used by both the CLI and the MCP `call_hierarchy` tool.
pub async fn run_call_hierarchy(
    backend: &dyn RacliBackend,
    file_path: String,
    line: u32,
    character: u32,
    direction: Direction,
    depth: u32,
) -> Result<CallHierarchyOutput, CallHierarchyError> {
    let mut items = backend
        .prepare_call_hierarchy(&file_path, line, character)
        .await
        .map_err(CallHierarchyError::Backend)?;
    if items.is_empty() {
        return Err(CallHierarchyError::NoItem);
    }
    let root = items.remove(0);
    let other_candidates = items.iter().map(item_to_json).collect();
    let mut errors = Vec::new();

    let incoming = if matches!(direction, Direction::Incoming | Direction::Both) {
        let mut visited = HashSet::new();
        visited.insert(visit_key(&root));
        Some(
            walk(
                backend,
                true,
                root.clone(),
                depth,
                &mut visited,
                &mut errors,
            )
            .await,
        )
    } else {
        None
    };
    let outgoing = if matches!(direction, Direction::Outgoing | Direction::Both) {
        let mut visited = HashSet::new();
        visited.insert(visit_key(&root));
        Some(
            walk(
                backend,
                false,
                root.clone(),
                depth,
                &mut visited,
                &mut errors,
            )
            .await,
        )
    } else {
        None
    };

    Ok(CallHierarchyOutput {
        item: item_to_json(&root),
        incoming,
        outgoing,
        other_candidates,
        errors,
    })
}

/// Runs the call-hierarchy walk from each definition the symbol names find and from the `--path`
/// position, if given, and writes the result to `out` as JSON or `--text`. `backend` finds the
/// definitions and walks from them.
pub async fn run_call_hierarchy_command(
    backend: &dyn RacliBackend,
    args: &CallHierarchyArgs,
    out: &mut CommandOutput,
) -> Result<(), CommandError> {
    let targets = symbol::command_targets(
        backend,
        &args.names,
        args.path.as_deref(),
        args.line,
        args.character,
        &args.symbol,
        out,
    )
    .await?;
    let grouped = targets.grouped;

    let mut results = Vec::with_capacity(targets.targets.len());
    for target in targets.targets {
        let walked = within_timeout(run_call_hierarchy(
            backend,
            target.file_path,
            target.line,
            target.character,
            args.direction,
            args.depth,
        ))
        .await?;
        match walked {
            Ok(result) => {
                out.mark_answered();
                for error in &result.errors {
                    out.note(error.clone());
                }
                for candidate in &result.other_candidates {
                    out.note(format!(
                        "did you mean {} at {}?",
                        candidate.name,
                        item_position_text(candidate)
                    ));
                }
                results.push((target.symbol, result));
            }
            Err(CallHierarchyError::NoItem) => match &target.symbol {
                Some(symbol) => {
                    out.note(format!("no call hierarchy item for {}", symbol.header()));
                }
                None => out.println("(no call hierarchy item at that position)"),
            },
            Err(CallHierarchyError::Backend(e)) => return Err(CommandError::Failed(e)),
        }
    }

    if !args.json {
        // Each tree starts with its item, which already names the symbol.
        for (_, result) in &results {
            write_call_hierarchy_text(out, result);
        }
    } else if grouped {
        let grouped: Vec<_> = results
            .iter()
            .filter_map(|(symbol, result)| {
                Some(SymbolResultJson {
                    symbol: symbol.as_ref()?,
                    result,
                })
            })
            .collect();
        write_symbol_results_json(out, &grouped);
    } else if let Some((_, result)) = results.first() {
        write_call_hierarchy_json(out, result);
    }
    Ok(())
}

/// Runs `racli call-hierarchy` against the project's server and prints what
/// [`run_call_hierarchy_command`] writes.
pub async fn run_cli_call_hierarchy(args: CallHierarchyArgs) {
    let sock = client_unix_socket_path();
    let sock_display = sock.display().to_string();
    let client = match Client::new(&sock) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("racli call-hierarchy ({sock_display}): {err}");
            return;
        }
    };
    let mut out = CommandOutput::default();
    let result = run_call_hierarchy_command(&client, &args, &mut out).await;
    out.print_cli("call-hierarchy", &sock_display, result);
}

/// Writes the call tree as JSON.
fn write_call_hierarchy_json(out: &mut CommandOutput, result: &CallHierarchyOutput) {
    match serde_json::to_string(result) {
        Ok(json) => out.println(json),
        Err(e) => out.note(format!("failed to serialize JSON: {e}")),
    }
}

/// Writes the call tree as an indented outline: `name (KIND) path:line:col` per item, with 1-based
/// line and column.
fn write_call_hierarchy_text(out: &mut CommandOutput, result: &CallHierarchyOutput) {
    out.println(format!(
        "{} ({}) {}",
        result.item.name,
        result.item.kind,
        item_position_text(&result.item)
    ));
    if let Some(incoming) = &result.incoming {
        out.println("<- callers:");
        if incoming.is_empty() {
            out.println("  (none)");
        }
        write_call_nodes_text(out, incoming, 1);
    }
    if let Some(outgoing) = &result.outgoing {
        out.println("-> callees:");
        if outgoing.is_empty() {
            out.println("  (none)");
        }
        write_call_nodes_text(out, outgoing, 1);
    }
}

/// Writes each node and its children, indented two spaces per level from `indent`.
fn write_call_nodes_text(out: &mut CommandOutput, nodes: &[CallNodeJson], indent: usize) {
    for node in nodes {
        out.println(format!(
            "{}{} ({}) {}",
            "  ".repeat(indent),
            node.item.name,
            node.item.kind,
            item_position_text(&node.item)
        ));
        write_call_nodes_text(out, &node.children, indent + 1);
    }
}

/// An item's `path:line:col`, at the start of its name, with 1-based line and column.
fn item_position_text(item: &CallHierarchyItemJson) -> String {
    let start = &item.selection_range.start;
    position_text(&item.uri, start.line, start.character)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::racli::DocumentSymbolsResponse;
    use crate::proto::racli::FindDefinitionResponse;
    use crate::proto::racli::FindImplementationsResponse;
    use crate::proto::racli::FindReferencesResponse;
    use crate::proto::racli::HoverResponse;
    use crate::proto::racli::LspCallHierarchyIncomingCall;
    use crate::proto::racli::LspCallHierarchyOutgoingCall;
    use crate::proto::racli::SearchResponse;
    use crate::rust_analyzer::SymbolSearchOptions;
    use std::collections::HashMap;

    fn test_item(name: &str, line: u32) -> LspCallHierarchyItem {
        let range = Some(LspRange {
            start: Some(LspPosition { line, character: 0 }),
            end: Some(LspPosition { line, character: 4 }),
        });
        LspCallHierarchyItem {
            name: name.to_string(),
            kind: 12, // FUNCTION
            uri: format!("file:///{name}.rs"),
            range,
            selection_range: range,
            detail: None,
            data_json: None,
        }
    }

    /// In-memory [`RacliBackend`] driven by a fixed `name -> callers` adjacency map, so the
    /// depth/cycle-guard logic in [`walk`] can be tested without a live rust-analyzer.
    struct FakeBackend {
        callers: HashMap<&'static str, Vec<(&'static str, u32)>>,
    }

    #[tonic::async_trait]
    impl RacliBackend for FakeBackend {
        async fn search(
            &self,
            _query: &str,
            _options: SymbolSearchOptions,
        ) -> Result<SearchResponse, String> {
            unreachable!("search is not exercised by these tests")
        }

        async fn find_definition(
            &self,
            _file_path: &str,
            _line: u32,
            _character: u32,
        ) -> Result<FindDefinitionResponse, String> {
            unreachable!("find_definition is not exercised by these tests")
        }

        async fn find_references(
            &self,
            _file_path: &str,
            _line: u32,
            _character: u32,
        ) -> Result<FindReferencesResponse, String> {
            unreachable!("find_references is not exercised by these tests")
        }

        async fn find_implementations(
            &self,
            _file_path: &str,
            _line: u32,
            _character: u32,
        ) -> Result<FindImplementationsResponse, String> {
            unreachable!("find_implementations is not exercised by these tests")
        }

        async fn document_symbols(
            &self,
            _file_path: &str,
        ) -> Result<DocumentSymbolsResponse, String> {
            unreachable!("document_symbols is not exercised by these tests")
        }

        async fn hover(
            &self,
            _file_path: &str,
            _line: u32,
            _character: u32,
        ) -> Result<HoverResponse, String> {
            unreachable!("hover is not exercised by these tests")
        }

        async fn prepare_call_hierarchy(
            &self,
            _file_path: &str,
            _line: u32,
            _character: u32,
        ) -> Result<Vec<LspCallHierarchyItem>, String> {
            unreachable!("prepare_call_hierarchy is not exercised by these tests")
        }

        async fn incoming_calls(
            &self,
            item: LspCallHierarchyItem,
        ) -> Result<Vec<LspCallHierarchyIncomingCall>, String> {
            let callers = self
                .callers
                .get(item.name.as_str())
                .cloned()
                .unwrap_or_default();
            Ok(callers
                .into_iter()
                .map(|(name, line)| LspCallHierarchyIncomingCall {
                    from: Some(test_item(name, line)),
                    from_ranges: vec![],
                })
                .collect())
        }

        async fn outgoing_calls(
            &self,
            _item: LspCallHierarchyItem,
        ) -> Result<Vec<LspCallHierarchyOutgoingCall>, String> {
            Ok(vec![])
        }
    }

    #[test]
    fn visit_key_distinguishes_items_by_uri_and_selection_range() {
        let a = test_item("a", 1);
        let b = test_item("b", 2);
        assert_ne!(visit_key(&a), visit_key(&b));
        assert_eq!(visit_key(&a), visit_key(&test_item("a", 1)));
    }

    #[tokio::test]
    async fn walk_depth_one_does_not_recurse_past_the_first_level() {
        let backend = FakeBackend {
            callers: HashMap::from([("root", vec![("a", 1)]), ("a", vec![("root", 0)])]),
        };
        let root = test_item("root", 0);
        let mut visited = HashSet::new();
        visited.insert(visit_key(&root));

        let nodes = walk(&backend, true, root, 1, &mut visited, &mut Vec::new()).await;

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].item.name, "a");
        assert!(
            nodes[0].children.is_empty(),
            "depth 1 must not recurse into `a`'s own callers"
        );
    }

    #[tokio::test]
    async fn walk_guards_cycles_without_infinite_recursion() {
        // root <- a <- b <- a (cycle back to `a`, a common shape for mutually recursive functions).
        let backend = FakeBackend {
            callers: HashMap::from([
                ("root", vec![("a", 1)]),
                ("a", vec![("b", 2)]),
                ("b", vec![("a", 1)]),
            ]),
        };
        let root = test_item("root", 0);
        let mut visited = HashSet::new();
        visited.insert(visit_key(&root));

        let nodes = walk(&backend, true, root, 10, &mut visited, &mut Vec::new()).await;

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].item.name, "a");
        assert_eq!(nodes[0].children.len(), 1);
        assert_eq!(nodes[0].children[0].item.name, "b");
        // `b`'s caller `a` is included (the cyclic back-edge is real data), but since `a` was
        // already visited it must appear as a leaf instead of recursing forever.
        assert_eq!(nodes[0].children[0].children.len(), 1);
        assert_eq!(nodes[0].children[0].children[0].item.name, "a");
        assert!(
            nodes[0].children[0].children[0].children.is_empty(),
            "the cycle back to `a` must be cut off by the visited set"
        );
    }

    #[tokio::test]
    async fn walk_depth_zero_returns_no_nodes() {
        let backend = FakeBackend {
            callers: HashMap::from([("root", vec![("a", 1)])]),
        };
        let root = test_item("root", 0);
        let mut visited = HashSet::new();
        visited.insert(visit_key(&root));

        let nodes = walk(&backend, true, root, 0, &mut visited, &mut Vec::new()).await;

        assert!(nodes.is_empty());
    }
}

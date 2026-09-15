//! `racli call-hierarchy`: CLI arguments, and the client-side call hierarchy orchestration
//! (prepare + recursive incoming/outgoing walk) shared between the CLI and the MCP tool.
//!
//! LSP only exposes one level of callers/callees per request; [`CallHierarchyBackend`]
//! abstracts over how that single level is fetched (gRPC over a Unix socket for the CLI, or an
//! in-process [`RacliSession`] for MCP), and [`run_call_hierarchy`] drives the depth-bounded,
//! cycle-guarded recursion on top of it so both callers see the same tree shape.

use std::collections::HashSet;
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use clap::Parser;
use clap::ValueEnum;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::client;
use crate::effective_unix_socket_path;
use crate::proto::racli::LspCallHierarchyIncomingCall;
use crate::proto::racli::LspCallHierarchyItem;
use crate::proto::racli::LspCallHierarchyOutgoingCall;
use crate::proto::racli::LspPosition;
use crate::proto::racli::LspRange;
use crate::racli_session::RacliSession;

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
    /// Rust source file (absolute or relative to the current directory).
    pub path: PathBuf,
    /// 0-based line (LSP `Position.line`).
    #[arg(long)]
    pub line: u32,
    /// 0-based UTF-16 character offset on the line (LSP `Position.character`).
    #[arg(long)]
    pub character: u32,
    /// Which side of the call graph to walk.
    #[arg(long, value_enum, default_value = "both")]
    pub direction: Direction,
    /// How many recursive levels of callers/callees to fetch (each level is one more LSP round trip per node).
    #[arg(long, default_value_t = 1)]
    pub depth: u32,
    /// Print an indented outline instead of JSON.
    #[arg(long)]
    pub text: bool,
}

/// One RPC round trip's worth of backend access, abstracting over a gRPC socket (CLI) vs. an
/// in-process [`RacliSession`] (MCP) so [`run_call_hierarchy`] can drive both the same way.
#[tonic::async_trait]
pub trait CallHierarchyBackend: Send + Sync {
    /// Runs `textDocument/prepareCallHierarchy`.
    async fn prepare(
        &self,
        file_path: String,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspCallHierarchyItem>, String>;
    /// Runs `callHierarchy/incomingCalls` for `item` (must be the exact item from `prepare`).
    async fn incoming(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyIncomingCall>, String>;
    /// Runs `callHierarchy/outgoingCalls` for `item` (must be the exact item from `prepare`).
    async fn outgoing(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyOutgoingCall>, String>;
}

/// [`CallHierarchyBackend`] over a `racli server` Unix socket, used by the CLI.
struct SocketBackend {
    socket: PathBuf,
}

#[tonic::async_trait]
impl CallHierarchyBackend for SocketBackend {
    async fn prepare(
        &self,
        file_path: String,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspCallHierarchyItem>, String> {
        client::prepare_call_hierarchy(&self.socket, file_path, line, character)
            .await
            .map(|r| r.items)
            .map_err(|e| e.to_string())
    }

    async fn incoming(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyIncomingCall>, String> {
        client::incoming_calls(&self.socket, item)
            .await
            .map(|r| r.calls)
            .map_err(|e| e.to_string())
    }

    async fn outgoing(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyOutgoingCall>, String> {
        client::outgoing_calls(&self.socket, item)
            .await
            .map(|r| r.calls)
            .map_err(|e| e.to_string())
    }
}

/// [`CallHierarchyBackend`] over an in-process [`RacliSession`], used by the MCP `call_hierarchy` tool.
#[tonic::async_trait]
impl CallHierarchyBackend for RacliSession {
    async fn prepare(
        &self,
        file_path: String,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspCallHierarchyItem>, String> {
        self.prepare_call_hierarchy(file_path, line, character)
            .await
            .map(|r| r.items)
            .map_err(|e| e.to_string())
    }

    async fn incoming(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyIncomingCall>, String> {
        self.incoming_calls(item)
            .await
            .map(|r| r.calls)
            .map_err(|e| e.to_string())
    }

    async fn outgoing(
        &self,
        item: LspCallHierarchyItem,
    ) -> Result<Vec<LspCallHierarchyOutgoingCall>, String> {
        self.outgoing_calls(item)
            .await
            .map(|r| r.calls)
            .map_err(|e| e.to_string())
    }
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

/// Formats a [`RangeJson`] as `startLine:startChar-endLine:endChar` for plain-text/stderr output.
fn format_range_json(r: &RangeJson) -> String {
    format!(
        "{}:{}-{}:{}",
        r.start.line, r.start.character, r.end.line, r.end.character
    )
}

/// Fetches one level of callers (`incoming: true`) or callees for `item`, then recurses into each
/// newly-visited result while `depth_remaining` allows, guarding cycles via `visited`.
fn walk<'a>(
    backend: &'a dyn CallHierarchyBackend,
    incoming: bool,
    item: LspCallHierarchyItem,
    depth_remaining: u32,
    visited: &'a mut HashSet<VisitKey>,
) -> Pin<Box<dyn Future<Output = Vec<CallNodeJson>> + Send + 'a>> {
    Box::pin(async move {
        if depth_remaining == 0 {
            return Vec::new();
        }

        let steps: Vec<(LspCallHierarchyItem, Vec<LspRange>)> = if incoming {
            match backend.incoming(item).await {
                Ok(calls) => calls
                    .into_iter()
                    .filter_map(|c| c.from.map(|from| (from, c.from_ranges)))
                    .collect(),
                Err(e) => {
                    eprintln!("racli call-hierarchy: {e}");
                    return Vec::new();
                }
            }
        } else {
            match backend.outgoing(item).await {
                Ok(calls) => calls
                    .into_iter()
                    .filter_map(|c| c.to.map(|to| (to, c.from_ranges)))
                    .collect(),
                Err(e) => {
                    eprintln!("racli call-hierarchy: {e}");
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
    backend: &dyn CallHierarchyBackend,
    file_path: String,
    line: u32,
    character: u32,
    direction: Direction,
    depth: u32,
) -> Result<CallHierarchyOutput, CallHierarchyError> {
    let mut items = backend
        .prepare(file_path, line, character)
        .await
        .map_err(CallHierarchyError::Backend)?;
    if items.is_empty() {
        return Err(CallHierarchyError::NoItem);
    }
    let root = items.remove(0);
    let other_candidates = items.iter().map(item_to_json).collect();

    let incoming = if matches!(direction, Direction::Incoming | Direction::Both) {
        let mut visited = HashSet::new();
        visited.insert(visit_key(&root));
        Some(walk(backend, true, root.clone(), depth, &mut visited).await)
    } else {
        None
    };
    let outgoing = if matches!(direction, Direction::Outgoing | Direction::Both) {
        let mut visited = HashSet::new();
        visited.insert(visit_key(&root));
        Some(walk(backend, false, root.clone(), depth, &mut visited).await)
    } else {
        None
    };

    Ok(CallHierarchyOutput {
        item: item_to_json(&root),
        incoming,
        outgoing,
        other_candidates,
    })
}

/// `CallHierarchyRequest` JSON body for MCP `call_hierarchy`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyRequestJson {
    /// Resolved on the server; same rules as gRPC `PrepareCallHierarchyRequest::file_path`.
    pub file_path: String,
    /// Zero-based line (LSP `Position`).
    pub line: u32,
    /// Zero-based UTF-16 character offset on the line.
    pub character: u32,
    /// Which side of the call graph to walk (default `both`).
    #[serde(default)]
    pub direction: Option<Direction>,
    /// How many recursive levels of callers/callees to fetch (default `1`).
    #[serde(default)]
    pub depth: Option<u32>,
}

/// Runs the call-hierarchy walk over a Unix socket and prints the result as JSON or `--text`.
pub async fn run_cli_call_hierarchy(args: CallHierarchyArgs) {
    let sock = effective_unix_socket_path();
    let sock_display = sock.display().to_string();

    let abs = match args.path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "racli call-hierarchy: cannot canonicalize {}: {e}",
                args.path.display()
            );
            return;
        }
    };
    let file_path = abs.display().to_string();
    let backend = SocketBackend {
        socket: sock.clone(),
    };

    match tokio::time::timeout(
        Duration::from_secs(60),
        run_call_hierarchy(
            &backend,
            file_path,
            args.line,
            args.character,
            args.direction,
            args.depth,
        ),
    )
    .await
    {
        Ok(Ok(result)) => {
            for candidate in &result.other_candidates {
                eprintln!(
                    "racli call-hierarchy: did you mean {} ({}) at {}?",
                    candidate.name,
                    candidate.uri,
                    format_range_json(&candidate.selection_range)
                );
            }
            if args.text {
                print_call_hierarchy_text(&result);
            } else {
                print_call_hierarchy_json(&result);
            }
        }
        Ok(Err(CallHierarchyError::NoItem)) => {
            println!("(no call hierarchy item at that position)");
        }
        Ok(Err(CallHierarchyError::Backend(e))) => {
            eprintln!("racli call-hierarchy ({sock_display}): {e}");
        }
        Err(_elapsed) => {
            eprintln!("racli call-hierarchy ({sock_display}): request timed out after 60 seconds");
        }
    }
}

fn print_call_hierarchy_json(result: &CallHierarchyOutput) {
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = serde_json::to_writer_pretty(&mut stdout, result) {
        eprintln!("racli call-hierarchy: failed to serialize JSON: {e}");
        return;
    }
    let _ = writeln!(stdout);
}

fn print_call_hierarchy_text(result: &CallHierarchyOutput) {
    println!(
        "{} ({}) {}",
        result.item.name, result.item.kind, result.item.uri
    );
    if let Some(incoming) = &result.incoming {
        println!("<- callers:");
        if incoming.is_empty() {
            println!("  (none)");
        }
        print_call_nodes_text(incoming, 1);
    }
    if let Some(outgoing) = &result.outgoing {
        println!("-> callees:");
        if outgoing.is_empty() {
            println!("  (none)");
        }
        print_call_nodes_text(outgoing, 1);
    }
}

fn print_call_nodes_text(nodes: &[CallNodeJson], indent: usize) {
    for node in nodes {
        println!(
            "{}{} ({}) {}",
            "  ".repeat(indent),
            node.item.name,
            node.item.kind,
            node.item.uri
        );
        print_call_nodes_text(&node.children, indent + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// In-memory [`CallHierarchyBackend`] driven by a fixed `name -> callers` adjacency map, so the
    /// depth/cycle-guard logic in [`walk`] can be tested without a live rust-analyzer.
    struct FakeBackend {
        callers: HashMap<&'static str, Vec<(&'static str, u32)>>,
    }

    #[tonic::async_trait]
    impl CallHierarchyBackend for FakeBackend {
        async fn prepare(
            &self,
            _file_path: String,
            _line: u32,
            _character: u32,
        ) -> Result<Vec<LspCallHierarchyItem>, String> {
            unreachable!("prepare is not exercised by these tests")
        }

        async fn incoming(
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

        async fn outgoing(
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

        let nodes = walk(&backend, true, root, 1, &mut visited).await;

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

        let nodes = walk(&backend, true, root, 10, &mut visited).await;

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

        let nodes = walk(&backend, true, root, 0, &mut visited).await;

        assert!(nodes.is_empty());
    }
}

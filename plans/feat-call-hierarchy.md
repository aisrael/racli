# `racli call-hierarchy`

## Goal

Add a `racli call-hierarchy <PATH> --line <N> --character <N>` command that
answers "who calls this" / "what does this call" via LSP call hierarchy,
exposed the same way as `search`, `find-definition`, and `find-references`:
one CLI subcommand, one gRPC surface, one MCP tool, all backed by the same
long-lived rust-analyzer session.

LSP models call hierarchy as two steps:

1. `textDocument/prepareCallHierarchy` — resolve a file position into one or
   more `CallHierarchyItem`s.
2. `callHierarchy/incomingCalls` / `callHierarchy/outgoingCalls` — given an
   item, return its direct callers / callees.

## Key design decisions

- **The server exposes granular RPCs, not one fused one.** `PrepareCallHierarchy`,
  `IncomingCalls`, `OutgoingCalls` — one RPC per LSP method, matching the
  existing convention (`Search`↔`workspace/symbol`, `FindReferences`↔
  `textDocument/references`). The *client* chains them into what looks like
  one command to the end user. The server stays a thin, stateless mirror of
  LSP, consistent with `Core`/`RacliSession` today.
- **`CallHierarchyItem` must round-trip through the client opaquely.** LSP's
  `CallHierarchyItem` carries an optional `data: Value` field rust-analyzer
  uses to resolve the item on the next call. The client can't reconstruct it
  from name/uri/range — it must hand back the *exact* item it received from
  `PrepareCallHierarchy`. The proto item carries an opaque `data_json`
  passthrough field the client never inspects.
- **`prepareCallHierarchy` can return more than one candidate** (ambiguous
  cursor position). MVP: use the first item, print the rest to stderr as a
  "did you mean" note instead of silently dropping them.
- **Depth.** A single LSP call only returns one level of callers/callees.
  `--depth N` (default `1`) recurses client-side, calling
  `IncomingCalls`/`OutgoingCalls` again on each returned item, with a
  `visited` set keyed on `(uri, selection_range)` to guard cycles (recursive
  functions are common in real code).
- **Direction.** `--direction incoming|outgoing|both` (default `both`).

## Proto additions (`proto/racli.proto`)

```protobuf
message LspCallHierarchyItem {
  string name = 1;
  string kind = 2;
  string uri = 3;
  LspRange range = 4;             // whole item (e.g. the function body)
  LspRange selection_range = 5;   // just the name
  optional string detail = 6;
  optional string data_json = 7;  // opaque passthrough, never interpreted client-side
}

message PrepareCallHierarchyRequest {
  string file_path = 1;
  uint32 line = 2;
  uint32 character = 3;
}
message PrepareCallHierarchyResponse {
  repeated LspCallHierarchyItem items = 1;
}

message CallHierarchyCallsRequest {
  LspCallHierarchyItem item = 1;  // the exact item from PrepareCallHierarchy
}
message LspCallHierarchyIncomingCall {
  LspCallHierarchyItem from = 1;
  repeated LspRange from_ranges = 2;
}
message LspCallHierarchyOutgoingCall {
  LspCallHierarchyItem to = 1;
  repeated LspRange from_ranges = 2;
}
message IncomingCallsResponse {
  repeated LspCallHierarchyIncomingCall calls = 1;
}
message OutgoingCallsResponse {
  repeated LspCallHierarchyOutgoingCall calls = 1;
}
```

Service additions:

```protobuf
rpc PrepareCallHierarchy (PrepareCallHierarchyRequest) returns (PrepareCallHierarchyResponse);
rpc IncomingCalls (CallHierarchyCallsRequest) returns (IncomingCallsResponse);
rpc OutgoingCalls (CallHierarchyCallsRequest) returns (OutgoingCallsResponse);
```

## `src/rust_analyzer.rs`

Three thin LSP wrappers on `RustAnalyzerSession`, same shape as
`text_document_references`:

```rust
pub async fn prepare_call_hierarchy(
    &mut self, document_uri: impl Into<String>, line: u32, character: u32,
) -> Result<Value, RustAnalyzerError> {
    // CallHierarchyPrepareParams { text_document_position_params, .. }
    // lsp.send_request::<CallHierarchyPrepare>(params).await
}

pub async fn call_hierarchy_incoming_calls(
    &mut self, item: lsp_types::CallHierarchyItem,
) -> Result<Value, RustAnalyzerError> {
    // lsp.send_request::<CallHierarchyIncomingCalls>(CallHierarchyIncomingCallsParams { item, .. }).await
}

pub async fn call_hierarchy_outgoing_calls(
    &mut self, item: lsp_types::CallHierarchyItem,
) -> Result<Value, RustAnalyzerError> {
    // mirror of the above, CallHierarchyOutgoingCalls / OutgoingCallsParams
}
```

## `src/server.rs` (`Core`) and `src/racli_session.rs` (`RacliSession`)

One pass-through method per RPC, mirroring `find_references`: resolve/
canonicalize the path into a `file://` URI, call the matching
`RustAnalyzerSession` method, map JSON → proto via new `lsp_map.rs` helpers
(`call_hierarchy_item_to_proto` / `_from_proto`, preserving `data` as
`data_json` via `serde_json::to_string`/`from_str`). No orchestration logic
lives here — that's the client's job.

## `src/call_hierarchy.rs` (new) — client-side orchestration

This is the piece that turns three RPCs into one CLI command.

```rust
#[derive(Parser)]
pub struct CallHierarchyArgs {
    pub path: PathBuf,
    #[arg(long)] pub line: u32,
    #[arg(long)] pub character: u32,
    #[arg(long, value_enum, default_value = "both")]
    pub direction: Direction,   // Incoming | Outgoing | Both
    #[arg(long, default_value_t = 1)]
    pub depth: u32,
    #[arg(long)] pub text: bool,
}

pub async fn run_cli_call_hierarchy(args: CallHierarchyArgs) {
    // 1. canonicalize path (same as find_definition/find_references)
    // 2. client::prepare_call_hierarchy(sock, file_path, line, character) -> Vec<CallHierarchyItem>
    //    - empty -> print "(no call hierarchy item at that position)", return
    //    - len > 1 -> take items[0], eprintln the rest as candidates
    // 3. walk(item, direction, depth, &mut visited) building a tree: each
    //    recursive step calls client::incoming_calls / client::outgoing_calls
    //    with the *exact* item (data_json intact) and recurses on `.from`/`.to`
    //    while depth remains and (uri, selection_range) hasn't been visited
    // 4. print as a JSON tree, or --text as indented lines (one call site per line)
}
```

`src/client.rs` gets three new thin gRPC-calling functions
(`prepare_call_hierarchy`, `incoming_calls`, `outgoing_calls`), same style as
the existing `find_references`/`find_definition` functions.

## MCP (`src/mcp.rs`)

One new tool, `call_hierarchy`, that calls the same client-side orchestration
function as the CLI path (not the raw RPCs) so MCP callers get the same
depth/direction/cycle-guard behavior as the CLI, and get back one JSON tree
per call instead of having to drive the three RPCs themselves. Wire it into
`mcp_proto_json.rs` the same way `find_references` was added.

## Testing

- Unit tests for the client-side tree-building/cycle-guard logic
  (`call_hierarchy.rs`), independent of a live rust-analyzer.
- A `tests/call_hierarchy.rs` integration test and
  `features/call_hierarchy.feature` cucumber scenario, mirroring
  `tests/find_references.rs` / `features/find_references.feature`.
- A `benches/call_hierarchy.rs` benchmark, mirroring
  `benches/find_references.rs`, pointed at racli's own source.

## Open questions / follow-ups (not blocking MVP)

- Should `--depth > 1` warn (or cap) when the graph is large, to avoid an
  agent accidentally requesting an expensive full-codebase traversal?
- Multiple `prepareCallHierarchy` candidates: should the CLI support
  `--candidate N` to pick a specific one instead of always taking the first?
- Should MCP's `call_hierarchy` tool description explicitly suggest pairing
  it with `find_references` for full blast-radius / rename-safety analysis?

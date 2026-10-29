# ADR 006: MCP Tools Mirror the CLI

- **Date:** 2026-10-28
- **Status:** Accepted.

## Context

`racli mcp` serves seven tools, `get_version`, `search`, `find_definition`, `find_references`,
`find_implementations`, `call_hierarchy` and `document_symbols`, from an in-process
`RacliSession`. Each position tool takes a `filePath` and a 0-based LSP position, and every tool
returns JSON shaped like racli's protobuf messages, converted in `src/mcp/mcp_proto_json.rs`.

The CLI has moved on since then:

- [ADR 001](001-lookup-by-symbol.md) lets the position commands take symbol names, with `--kind`
  and `--glob`, and lists the closest symbols for a name that finds nothing. Its decision 10 left
  the MCP tools unchanged, saying they "should later get the same lookup by name".
- [ADR 002](002-one-based-positions-and-text-output.md) made `--line` and `--character` 1-based and
  `--text` grep-style. Its decision 3 kept MCP 0-based, so that an MCP result's positions still
  worked as MCP inputs, and rejected 1-based MCP "for now".
- `find-definition` gained `--hover` and `--source`, and `search` gained `--exact`.
- The gRPC search messages gained `container_name` and `truncated`, which MCP's JSON drops.
- [ADR 004](004-backend-status-in-response-metadata.md) put the backend status on every gRPC
  response. MCP results don't carry it.

All of these except the last two live in the CLI's client code: `symbol.rs`,
`definition_details.rs`, `search.rs` and each command's `run_cli_*` and print functions. That code
calls the backend through `client::Client` over the Unix socket. The MCP server calls
`RacliSession` directly, so none of it is reachable from MCP. Only `call-hierarchy` shares its
logic with MCP, through `CallHierarchyBackend`, implemented both over the socket and for
`RacliSession`.

An agent using MCP therefore needs a `search`, then a position tool, for what one CLI command does
by name. It gets more tokens of JSON back than `--text` would print, and it has to convert the
positions it reads in one tool's result before passing them to the CLI, or the other way round.

## Decisions

### 1. Each MCP tool is its CLI command

An MCP tool takes the same options as its CLI command and returns what the command prints:

- its standard output as the first text content block;
- what it would print on standard error, such as unknown names with the closest symbols, "did you
  mean" candidates from `call-hierarchy`, and the `truncated` warning, as a second text block, when
  there is any.

The command's logic runs in both, rather than being written again for MCP. Name lookup and
`find-definition --hover` and `--source` run over a backend trait implemented for `Client` and for
`RacliSession`, as `CallHierarchyBackend` does for call hierarchy. Each command's run writes its
output to a buffer, which the CLI prints and the MCP server returns.

`get_version` keeps its structured JSON and declares its `outputSchema`.

### 2. `format` picks the output, and defaults to `text`

The position tools, `search` and `document_symbols` take `format`, as the CLI's `search` and
`document-symbols` take `--output-format`:

- **`text`**, the default: what `--text` prints. MCP's callers are agents, and `--text` was made
  for them: one short line per result, with a `path:line:col` that can be passed back in.
- **`json`**: the CLI's JSON, including the array of `{"symbol": …, "result": …}` when names are
  given (ADR 001 decision 5). Like the CLI's JSON, it keeps LSP's URIs and 0-based ranges (ADR 002
  decision 3).

Any other value is rejected (decision 5). The schema lists the values each tool takes, so a
format added later, such as `csv` for `search` as on the CLI, is one more value rather than another
parameter.

Either way the output is a text content block: with `json`, it's the CLI's JSON as a string.
Since a tool's result depends on `format`, the position tools, `search` and `document_symbols`
don't declare an `outputSchema` or return `structuredContent`.

### 3. Positions are 1-based, given with `path`

`line` and `character` are 1-based, `character` in UTF-16 units, as on the CLI. `0` is rejected
(decision 5). This replaces ADR 002 decision 3 for MCP inputs.

The file is given with `path`, not `filePath`, matching `--path`. As on the CLI, `path`, `line`
and `character` go together, and names can be given with or without them. A relative `path` is
relative to the directory `racli mcp` runs in, which is the workspace root, so text output's
relative paths work as `path` as they are.

### 4. The backend status is in `_meta`, with a warning when not ready

Every tool result's `_meta` has `racli/backend`, which is `ready`, `indexing` or `error`, with the
same meaning as the `racli-backend` gRPC metadata. With `error`, `racli/backendMessage` has
rust-analyzer's message. The status is read after the answer is produced, as the gRPC handlers do.

MCP hosts don't reliably show `_meta` to the model. So when the status isn't `ready`, the result
also has a text block saying so, such as `rust-analyzer is still indexing; results may be
incomplete`. As in ADR 004 decision 2, requests are never refused or delayed.

### 5. Errors follow the CLI's

- Arguments the command would reject are a tool result with `isError: true` and the reason, as
  rmcp already returns for arguments that don't match a tool's schema, such as an unknown
  `format`. They are what the CLI reports as a usage error: a name that isn't a Rust path, part of
  a position, `0` for `line` or `character`, `kind` or `glob` without a name, an invalid glob, and
  neither names nor a position. Messages name the MCP parameters, such as `path`, rather than the
  CLI options. The caller, usually a model, reads the reason and can retry.
- A name that finds nothing, or a position in a file that doesn't exist, doesn't stop the others
  (ADR 001 decision 7). Its message, with the closest symbols for a name, goes in the second text
  block. If no name or position found anything, the result has `isError: true`, so the caller sees
  the call failed and reads why.
- A failure in rust-analyzer or racli is an `internal_error`, as now.

## Alternatives Considered

- **Adding the CLI's features to MCP's own JSON one by one.** Rejected: name lookup, hover, source
  and `--exact` would be written twice, and the two would drift as they have. MCP's protobuf-shaped
  JSON would also stay a third shape next to the CLI's JSON and text.
- **JSON by default, as on the CLI.** Rejected: MCP's callers are agents, and JSON costs several
  times the tokens of `--text` for the same results.
- **A `text` boolean, as the CLI's `--text`.** Rejected: it can only choose between two formats,
  so another one, such as `csv`, would need a second parameter that conflicts with it, as `--text`,
  `--json`, `--csv` and `--output-format` do on the CLI.
- **Both text content and `structuredContent` in every result.** Rejected: it doubles every
  result, and hosts differ on which one the model sees.
- **`structuredContent` with `format: "json"`,** so programs get native JSON instead of a string
  to parse. Deferred: `structuredContent` is an object, so the CLI's JSON arrays, such as
  `[{"symbol": …, "result": …}]`, would have to be wrapped in a field, as `{"items": […]}`, and
  MCP's JSON would no longer match the CLI's.
- **1-based JSON for MCP only.** Rejected: a fourth output shape to keep, and MCP's JSON would no
  longer match the CLI's.
- **Keeping `filePath`.** Rejected: a caller still sending 0-based positions would get answers one
  line and column off without knowing. With `path`, the old call fails, because `line` and
  `character` arrive without `path`.
- **Rejected arguments as a JSON-RPC `invalid_params` error.** Rejected: rmcp returns arguments
  that don't match a tool's schema as a tool result with `isError`, so the caller can read the
  reason and retry, and racli's own checks would then report bad input a second way.
- **A status field in each result.** Rejected: it would be in the text the model reads on every
  call, and in the JSON shape, for a state that is usually `ready`. The warning block covers the
  case that matters.
- **A separate `hover` tool.** Deferred: the CLI has none, and `find_definition` with `hover`
  covers it by name or position. `RacliSession::hover` makes it easy to add.

## Consequences

- **Breaking for MCP clients:** positions are 1-based, `filePath` is `path`, and results are text
  by default, or the CLI's JSON with `format: "json"`, instead of protobuf-shaped JSON.
- The MCP position tools take `names`, `path`, `line`, `character`, `kind`, `glob` and `format`;
  `find_definition` also takes `hover` and `source`, and `call_hierarchy` takes `direction` and
  `depth`. `search` takes `exact` and `format`, and `document_symbols` takes `path` and `format`.
- `search --csv` has no MCP equivalent yet; it can be added as `format: "csv"`.
- A position printed by a tool's text result works as that tool's input as it is, on MCP as on
  the CLI.
- Each CLI command is split into a run shared with MCP, which writes to a buffer, and a CLI
  wrapper that connects, prints and exits. CLI output doesn't change.
- Most of `mcp_proto_json.rs` goes away.
- Name lookups cost MCP what they cost the CLI: several `Search` calls each (ADR 001
  consequences), though in-process.
- MCP tests wait for the backend by calling `get_version` until its `_meta` says `ready`.

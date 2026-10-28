# ADR 003: A High-Level Client

- **Date:** 2026-10-28
- **Status:** Accepted.

## Context

The CLI talks to the racli backend, `racli server` or `racli tee`, through `racli::client::Client`
over the workspace's Unix socket. Both serve the same gRPC API. That API has two kinds of RPC:

- **Typed RPCs**, such as `FindDefinition` and `DocumentSymbols`. Each runs one racli operation in
  `RacliSession`, which the MCP server also calls, and returns racli's protobuf messages.
- **Passthrough RPCs**: `LspInitialize`, `LspRequest`, `LspNotify` and `LspEvents`. They let
  `racli tee` proxy an editor's LSP session to rust-analyzer, and carry LSP method names and JSON.

`find-definition --hover` needed `textDocument/hover`, which had no typed RPC. It was first added as
`Client::lsp_request`, which built the hover params JSON in the CLI and parsed rust-analyzer's JSON
result. That opened a way for every CLI feature to be composed from raw LSP requests instead of
typed RPCs.

## Decisions

### 1. `Client` is a high-level interface to the racli backend

Its methods name racli operations and take and return racli's protobuf messages. They never take an
LSP method name or JSON, and callers don't need to know that rust-analyzer is behind the backend.
Results keep LSP's shapes, such as URIs and 0-based ranges, as [ADR 002](002-one-based-positions-and-text-output.md)
decides.

### 2. Each operation the CLI needs is a typed RPC

The operation is implemented in `RacliSession`, so the gRPC server and the MCP server share it.
Hover is the first: `Racli.Hover` takes a path and a position and returns the hover's contents as
Markdown, and `Client::hover` calls it. `Client::lsp_request` is removed.

### 3. The passthrough RPCs are for `racli tee` only

They stay in the gRPC API for the editor proxy, which uses the generated `RacliClient` directly.
`Client` doesn't wrap them.

## Alternatives Considered

- **Every CLI request as an `LspRequest`.** Rejected: the logic in `RacliSession` and `Core`, such as
  splitting and merging multi-pattern searches, would move into the CLI, and the MCP server, which
  calls `RacliSession` in-process, would need its own copy. Every client would also build and parse
  LSP JSON, so mistakes would only show up at runtime.
- **Keep `Client::lsp_request` as an escape hatch.** Rejected: one raw request makes the next one
  easy, and the API would no longer say what the backend supports.

## Consequences

- A CLI feature that needs a new LSP request adds a proto message, a `RacliSession` method, a
  rust-analyzer actor message and a `Client` method, rather than one raw call.
- The MCP server can offer anything the CLI does by calling the same `RacliSession` method.
- `racli tee` keeps forwarding any LSP request an editor sends.

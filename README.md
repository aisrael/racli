racli - a CLI tool for [rust-analyzer](https://github.com/rust-lang/rust-analyzer)
====

## Architecture

In the usual setup there are three pieces:

- **rust-analyzer** — the Language Server process that `racli server` drives over LSP.
- **racli server** — a long-running gRPC listener on a Unix socket (default `/tmp/racli.sock`); it spawns `rust-analyzer`, completes an LSP `initialize` handshake with the current working directory as the workspace root, and serves RPCs to clients.
- **racli (client)** — the same binary used in client mode: subcommands that connect to the socket and call the server.

Stop the server with Ctrl+C or SIGTERM to trigger LSP `shutdown`/`exit` and clean termination of the child.

A high-level diagram lives in [docs/high-level-architecture.md](docs/high-level-architecture.md).

### `racli server`

`racli server` binds the gRPC Unix socket (default `/tmp/racli.sock`) and, when `rust-analyzer` is available on your `PATH`, spawns it as a child in the current working directory and completes the LSP `initialize` handshake described above.

## Client commands

These subcommands expect a running `racli server` at the default Unix socket path unless noted otherwise.

### `racli search <query>`

Runs LSP [`workspace/symbol`](https://rust-analyzer.github.io/book/features.html#workspace-symbol) through the server: the client calls gRPC `Search`, the server forwards the query to rust-analyzer, and the reply is a structured [`WorkspaceSymbolResponse`](proto/racli.proto) mirroring `lsp_types::WorkspaceSymbolResponse` (either a **flat** list of symbol information or a **nested** list of workspace symbols). Symbols are scoped to the server's current working directory when `racli server` was started. By default, output is **JSON** (one array of symbol objects); use `--text` or `--csv` (or `--output-format`) for plain text or CSV. The default Unix socket is `/tmp/racli.sock` (same as `racli version`).

### `racli find-definition <PATH> --line <N> --character <N>`

Runs LSP [`textDocument/definition`](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocument_definition): the client calls gRPC `FindDefinition` with an absolute filesystem path (after canonicalizing `PATH` on the client) plus a **0-based** line and **0-based** UTF-16 character offset on that line, matching LSP `Position`. The server resolves the path again, builds a `file://` URI, and returns a list of definition sites (scalar, array, and link-shaped LSP results are flattened to `uri` + `range`). Default output is **JSON**; pass `--text` for one human-readable line per location. Use the same workspace and socket rules as `racli search`; rust-analyzer must have indexed the crate (if the server just started, wait until analysis has caught up—for example until `racli search` returns symbols—before relying on definitions).

### `racli version`

Prints the client version from the binary (`CARGO_PKG_VERSION`). If the server answers at the default socket, prints the server version from gRPC `GetVersion`. If the server is missing, errors, or does not respond within 10 seconds, a message is written to stderr and only the client line is printed to stdout.

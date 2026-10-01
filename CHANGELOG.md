# Changelog

All notable changes to racli are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and racli follows [Semantic Versioning](https://semver.org/).

## [0.3.0] - 2026-09-30

Changes since 0.2.2.

### Added

- **`racli tee`**: lets an editor use racli as its rust-analyzer. It serves LSP on stdin/stdout and gRPC on the project's socket, so `racli search` and the other client commands query the same rust-analyzer instance as the editor. Editor requests go through the socket and are queued with every other client's. ([#17])
- **Per-project sockets**: `racli server` and `racli tee` listen on `/tmp/racli-<hash>.sock`, derived from the working directory, so several projects can each run their own server. Client commands find the nearest running server from any subdirectory of the project. ([#17])
- **`racli document-symbols <PATH>`**: a hierarchical outline of the symbols in a file (LSP `textDocument/documentSymbol`). ([#11])
- **`racli call-hierarchy <PATH> --line <N> --character <N>`**: walks callers and callees (LSP call hierarchy), with `--direction incoming|outgoing|both` and `--depth <N>`. ([#12])
- **`racli find-implementations <PATH> --line <N> --character <N>`**: finds the `impl` blocks of a trait or type, or the overrides of a trait method (LSP `textDocument/implementation`). ([#13])
- The three new commands are also available as MCP tools and gRPC RPCs.
- **`racli search --kind` and `--scope`**: `--kind all-symbols` adds functions, methods, constants, statics and fields to the default types-only results. `--scope workspace-and-dependencies` adds dependency crates and the standard library. The MCP `search` tool takes the same options. ([#15])
- **`--symbol-search-limit <N>`** on `racli server` and `racli mcp` caps `workspace/symbol` results. It defaults to 1000; rust-analyzer's own default of 128 used to truncate results silently. ([#15])
- **gRPC LSP passthrough**: the `LspInitialize`, `LspRequest`, `LspNotify` and `LspEvents` RPCs give any gRPC client access to the shared rust-analyzer. ([#17])
- A relative `RACLI_SERVER_LOG_FILE` is resolved against the working directory, and missing directories are created. `RACLI_SERVER_LOG_FILE=.racli/racli.log` gives each project its own log. ([#17])

### Changed

- **JSON output is compact.** CLI commands print JSON on a single line instead of pretty-printing it. Pipe through `jq` for indented output. ([#17])
- **Errors are readable.** `racli` prints errors as a cause chain (`error: outer: cause: root cause`) instead of a `Debug` dump. If rust-analyzer can't start (for example, the toolchain has no rust-analyzer component), racli now says so. ([#17])
- Startup and shutdown are managed by a [ractor](https://docs.rs/ractor) supervision tree. Startup runs rust-analyzer, then the file watcher, then the front end. Shutdown runs in reverse. ([#16])
- racli advertises more LSP client capabilities to rust-analyzer, such as markdown hover, semantic tokens and inlay hints, in every mode. ([#17])

### Fixed

- rust-analyzer no longer exits with "client exited without proper shutdown sequence" when racli stops it. ([#17])
- During an editor restart, an old racli no longer deletes the socket its successor just created. ([#17])
- The file watcher no longer drops filesystem events detected during shutdown. ([#9])
- Building with rmcp 3.4 or later no longer prints deprecation warnings. ([#14])

### Breaking changes

- `racli server` no longer listens on `/tmp/racli.sock` by default. racli 0.2.x clients won't find a 0.3.0 server unless it's started with `RACLI_DERIVE_SOCKET_PATH=0` or an explicit `RACLI_UNIX_SOCKET`.
- Scripts that relied on pretty-printed JSON, or on the old `Error: <Debug>` error format, need updating.
- Library API:
  - `grpc_server::init_grpc_server_tracing` moved to `logging::init_server_tracing`, and the `RACLI_SERVER_LOG_*_ENV` constants moved to `racli::logging`.
  - `GrpcServerError`, `mcp::ServerError` and `RacliBackendStartError` have a new `Actor(String)` variant.
  - `RacliSession::new` is now `pub(crate)`.

[0.3.0]: https://github.com/aisrael/racli/compare/123c0d4...v0.3.0
[#9]: https://github.com/aisrael/racli/pull/9
[#11]: https://github.com/aisrael/racli/pull/11
[#12]: https://github.com/aisrael/racli/pull/12
[#13]: https://github.com/aisrael/racli/pull/13
[#14]: https://github.com/aisrael/racli/pull/14
[#15]: https://github.com/aisrael/racli/pull/15
[#16]: https://github.com/aisrael/racli/pull/16
[#17]: https://github.com/aisrael/racli/pull/17

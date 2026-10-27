# Changelog

All notable changes to racli are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and racli follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **`racli-tee` binary**: runs `racli tee` with no subcommand, so editors whose server path takes no arguments (such as VS Code's `rust-analyzer.server.path`) can point at it directly instead of a wrapper script. `cargo install racli` installs it alongside `racli`.

## [0.2.0] - 2026-09-30

Changes since 0.1.1.

### Added

- **`racli tee`**: lets an editor use racli as its rust-analyzer. It serves LSP on stdin/stdout and gRPC on the project's socket, so `racli search` and the other client commands query the same rust-analyzer instance as the editor. Editor requests go through the socket and are queued with every other client's. ([#17])
- **Per-project sockets**: `racli server` and `racli tee` listen on `/tmp/racli-<hash>.sock`, derived from the working directory, so several projects can each run their own server. Client commands find the nearest running server from any subdirectory of the project. ([#17])
- **New client commands**, each also available as an MCP tool and a gRPC RPC:
  - **`racli find-references <PATH> --line <N> --character <N>`**: lists every reference to the symbol at a position, including its declaration (LSP `textDocument/references`). ([#6])
  - **`racli document-symbols <PATH>`**: a hierarchical outline of the symbols in a file (LSP `textDocument/documentSymbol`). ([#11])
  - **`racli call-hierarchy <PATH> --line <N> --character <N>`**: walks callers and callees (LSP call hierarchy), with `--direction incoming|outgoing|both` and `--depth <N>`. ([#12])
  - **`racli find-implementations <PATH> --line <N> --character <N>`**: finds the `impl` blocks of a trait or type, or the overrides of a trait method (LSP `textDocument/implementation`). ([#13])
- **MCP tools**: `racli mcp` provides `get_version`, `search`, `find_definition`, `find_references`, `document_symbols`, `call_hierarchy` and `find_implementations`. ([#3], [#6], [#11], [#12], [#13])
- **`racli search --kind` and `--scope`**: `--kind all-symbols` adds functions, methods, constants, statics and fields to the default types-only results. `--scope workspace-and-dependencies` adds dependency crates and the standard library. The MCP `search` tool takes the same options. ([#15])
- **`--symbol-search-limit <N>`** on `racli server` and `racli mcp` caps `workspace/symbol` results. It defaults to 1000; rust-analyzer's own default of 128 used to truncate results silently. ([#15])
- **File watching**: the server watches the workspace and tells rust-analyzer about changed files, so results stay current as you edit. ([#2])
- **gRPC LSP passthrough**: the `LspInitialize`, `LspRequest`, `LspNotify` and `LspEvents` RPCs give any gRPC client access to the shared rust-analyzer. ([#17])
- **Logging configuration**:
  - `RACLI_LOG_LEVEL` sets the log level for client commands, which previously didn't log. ([#4])
  - `RACLI_SERVER_LOG_FILE` sends server logs to a file instead of stderr. A relative path is resolved against the working directory, and missing directories are created, so `RACLI_SERVER_LOG_FILE=.racli/racli.log` gives each project its own log. ([#4], [#17])

### Changed

- **`racli mcp` runs over stdio** and starts its own rust-analyzer and file watcher, with the working directory as the workspace root. It no longer listens on a Unix socket or needs a running `racli server`. ([#3])
- **JSON output is compact.** CLI commands print JSON on a single line instead of pretty-printing it. Pipe through `jq` for indented output. ([#17])
- **Errors are readable.** `racli` prints errors as a cause chain (`error: outer: cause: root cause`) instead of a `Debug` dump. If rust-analyzer can't start (for example, the toolchain has no rust-analyzer component), racli now says so. ([#17])
- `RACLI_SERVER_LOG_LEVEL` accepts a level name (`off`, `error`, `warn`, `info`, `debug` or `trace`, in any case) or a number from 0 to 5. Its default changed from `debug` to `info`. ([#4])
- Startup and shutdown are managed by a [ractor](https://docs.rs/ractor) supervision tree. Startup runs rust-analyzer, then the file watcher, then the front end. Shutdown runs in reverse. ([#16])
- racli advertises more LSP client capabilities to rust-analyzer, such as markdown hover, semantic tokens and inlay hints. ([#11], [#17])

### Fixed

- Pressing Ctrl+C or sending SIGTERM while rust-analyzer is still starting now shuts racli down cleanly. ([#3])
- rust-analyzer no longer exits with "client exited without proper shutdown sequence" when racli stops it. ([#17])
- When a server restarts, the old process no longer deletes the socket its successor just created. ([#17])

### Breaking changes

- `racli server` no longer listens on `/tmp/racli.sock` by default, so racli 0.1.1 clients won't find a 0.2.0 server. To keep the old path, start the server with `RACLI_DERIVE_SOCKET_PATH=0` or an explicit `RACLI_UNIX_SOCKET`.
- MCP clients must launch `racli mcp` as a stdio server instead of connecting to its Unix socket.
- Scripts that relied on pretty-printed JSON, or on the old `Error: <Debug>` error format, need updating.
- `racli server` and `racli mcp` log at `info` instead of `debug` by default. Set `RACLI_SERVER_LOG_LEVEL=debug` to get the old output.
- Library API: the `transport::socket_server` and `transport::socketwrapper` modules were removed, and `RACLI_SERVER_LOG_LEVEL_ENV` moved from `grpc_server` to `logging`.

[0.2.0]: https://github.com/aisrael/racli/compare/v0.1.1...v0.2.0
[#2]: https://github.com/aisrael/racli/pull/2
[#3]: https://github.com/aisrael/racli/pull/3
[#4]: https://github.com/aisrael/racli/pull/4
[#6]: https://github.com/aisrael/racli/pull/6
[#11]: https://github.com/aisrael/racli/pull/11
[#12]: https://github.com/aisrael/racli/pull/12
[#13]: https://github.com/aisrael/racli/pull/13
[#15]: https://github.com/aisrael/racli/pull/15
[#16]: https://github.com/aisrael/racli/pull/16
[#17]: https://github.com/aisrael/racli/pull/17

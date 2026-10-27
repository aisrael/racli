[![CI Tests](https://github.com/aisrael/racli/actions/workflows/ci.yml/badge.svg)](https://github.com/aisrael/racli/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/racli.svg)](https://crates.io/crates/racli)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)]


racli - a CLI tool for [rust-analyzer](https://github.com/rust-lang/rust-analyzer)
====

The latest version is **0.2.0**, which adds `racli tee` for sharing rust-analyzer with your editor, per-project sockets, MCP over stdio, and the `find-references`, `document-symbols`, `call-hierarchy` and `find-implementations` commands. See the [changelog](CHANGELOG.md) for details.

## Installation

```sh
cargo install racli
```

## Usage

In a Rust project folder, first run the server:

```sh
racli server
```

Then, to search for e.g. `worker`:

```sh
racli search worker
```

## Architecture

In the usual setup there are three pieces:

- **rust-analyzer** — the Language Server process that `racli server` drives over LSP.
- **racli server** — a long-running gRPC listener on a per-project Unix socket (see [Sockets](#sockets)); it spawns `rust-analyzer`, completes an LSP `initialize` handshake with the current working directory as the workspace root, and serves RPCs to clients.
- **racli (client)** — the same binary used in client mode: subcommands that find the project's socket and call the server.

### Sockets

Each project gets its own server socket, so several projects (or editor windows) can each run their own racli and rust-analyzer. The socket path is `/tmp/racli-<hash>.sock`, where the hash is taken from the server's working directory.

Client commands find it automatically. Starting from their own working directory, they check it and then each parent directory in turn, and connect to the nearest one with a running server. So `racli search` works from any subdirectory of the project that `racli server` or `racli tee` was started in. If no server is found, the client prints a warning naming the directory and the socket it expected.

- `RACLI_UNIX_SOCKET=<path>` sets an explicit socket path. It overrides discovery on both the server and the client side.
- `RACLI_DERIVE_SOCKET_PATH=0` (or `false`) turns per-project sockets off, so servers and clients both use the single `/tmp/racli.sock`. Clients with this setting don't find per-project servers, and vice versa.
- **racli tee** — `racli server` plus an LSP server on stdio for editors: the editor launches `racli tee` in place of `rust-analyzer`, and CLI/MCP clients share the same rust-analyzer through the socket.
- **racli mcp** — MCP over stdio only: spawns rust-analyzer and the workspace file watcher inside the MCP child process (no Unix socket). Use **`racli server`** for `racli search`, `racli find-definition`, `racli find-references`, `racli document-symbols`, and `racli version`.

Stop the server with Ctrl+C or SIGTERM to trigger LSP `shutdown`/`exit` and clean termination of the child.

A high-level diagram lives in [docs/high-level-architecture.md](docs/high-level-architecture.md). Design decisions are recorded as ADRs in [docs/adrs](docs/adrs/):

- [ADR 001: Lookup by Symbol](docs/adrs/001-lookup-by-symbol.md)

### `racli server`

`racli server` binds the project's gRPC Unix socket (see [Sockets](#sockets)) and, when `rust-analyzer` is available on your `PATH`, spawns it as a child in the current working directory and completes the LSP `initialize` handshake described above.

If you start `racli server`, `racli tee` or `racli-tee` from a terminal in a directory that racli is already serving, it says so and exits 1 instead of starting a second rust-analyzer. When stdin isn't a terminal, the new instance takes over the socket. That's what happens when an editor restarts its server, so restarts aren't blocked by an old instance that hasn't exited yet.

`--symbol-search-limit <N>` (also accepted by `racli mcp`) caps how many results `Search` returns per `workspace/symbol` query (default `1000`; rust-analyzer's own default is `128`). racli sends one more than the limit to rust-analyzer as `workspace.symbol.search.limit` during `initialize`, so it can tell a cut-off from an exact fit: when rust-analyzer returns more than the limit, `Search` drops the extra results and sets `truncated` on its response, and `racli search` warns on stderr. The limit applies to each `|`-separated pattern separately.

### `racli tee`

`racli tee` does everything `racli server` does (spawns rust-analyzer, serves gRPC on the project's socket; see [Sockets](#sockets)). It also speaks LSP on stdin/stdout, so an editor can use it as its rust-analyzer while `racli search` and friends query the same instance. The stdio side never talks to rust-analyzer directly. It is a gRPC client of its own socket, using the `LspInitialize` / `LspRequest` / `LspNotify` / `LspEvents` RPCs, so editor requests are sequenced alongside every other client's.

Because sockets are per project, each editor window gets its own `racli tee`, and `racli search` run inside that project reaches the editor's rust-analyzer. The chosen socket path is logged in the `racli tee starting` line.

It stops when the editor sends `exit` or closes stdin, or on SIGINT/SIGTERM. Logs go to stderr, or to `$RACLI_SERVER_LOG_FILE`, and never to stdout. A relative `RACLI_SERVER_LOG_FILE` is based on the working directory, and missing directories are created. So `RACLI_SERVER_LOG_FILE=.racli/racli.log` gives each project its own log, and you may want to add `.racli/` to that project's `.gitignore`.

Editors that take a server path with no arguments (for example VS Code's `rust-analyzer.server.path`) can use the `racli-tee` binary, which `cargo install racli` installs alongside `racli`. It behaves exactly like `racli tee` and takes the same options:

```json
"rust-analyzer.server.path": "~/.cargo/bin/racli-tee"
```

To check whether racli is serving a project, run `racli tee --check` (or `racli-tee --check`) in the project root. It asks the server on that directory's socket for its version without starting anything. If a server answers, it prints the version and socket and exits 0. Otherwise it says why on stderr and exits 1. Unlike client commands, it doesn't look for a server in parent directories.

Limitations:
- The editor's `initialize` is answered with racli's own `InitializeResult`, and the editor's capabilities and `initializationOptions` are ignored.
- Requests are handled one at a time, and `$/cancelRequest` is ignored.
- Only `textDocument/publishDiagnostics`, `experimental/serverStatus`, `window/showMessage` and `window/logMessage` reach the editor. Server-to-client requests are not forwarded.

## Client commands

These subcommands expect a running `racli server` or `racli tee` for the project they're run in (found as described in [Sockets](#sockets)), unless noted otherwise.

### `racli search <query>`

Runs LSP [`workspace/symbol`](https://rust-analyzer.github.io/book/features.html#workspace-symbol) through the server: the client calls gRPC `Search`, the server forwards the query to rust-analyzer, and the reply is a structured [`WorkspaceSymbolResponse`](proto/racli.proto) mirroring `lsp_types::WorkspaceSymbolResponse` (either a **flat** list of symbol information or a **nested** list of workspace symbols). Symbols are scoped to the server's current working directory when `racli server` was started. By default, output is **JSON** (one array of symbol objects); use `--text` or `--csv` (or `--output-format`) for plain text or CSV.

Separate alternative patterns with a single unescaped `|` (similar to `grep -E`), for example `racli search 'Foo|Bar'`. Each pattern is still a plain substring for rust-analyzer, not a full regular expression. A literal `|` in a pattern must be written as `\|` (a backslash before the pipe). Example: `racli search 'a\|b|c'` searches for the substring `a|b` and for `c`, then merges and dedupes the combined results.

By default rust-analyzer returns only types (modules, structs, enums, traits, type aliases) from workspace crates. `--kind all-symbols` adds functions, methods, constants, statics, and fields, and `--scope workspace-and-dependencies` adds dependency crates and the standard library. An empty query (`racli search ''`) lists every matching symbol up to the server's `--symbol-search-limit`. The MCP `search` tool takes the same options as `kind` (`only_types` / `all_symbols`) and `scope` (`workspace` / `workspace_and_dependencies`).

### `racli find-definition --path <PATH> --line <N> --character <N>`

Runs LSP [`textDocument/definition`](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocument_definition): the client calls gRPC `FindDefinition` with an absolute filesystem path (after canonicalizing `PATH` on the client) plus a **0-based** line and **0-based** UTF-16 character offset on that line, matching LSP `Position`. The server resolves the path again, builds a `file://` URI, and returns a list of definition sites (scalar, array, and link-shaped LSP results are flattened to `uri` + `range`). Default output is **JSON**; pass `--text` for one human-readable line per location. Use the same workspace and socket rules as `racli search`; rust-analyzer must have indexed the crate (if the server just started, wait until analysis has caught up—for example until `racli search` returns symbols—before relying on definitions).

### `racli find-references --path <PATH> --line <N> --character <N>`

Runs LSP [`textDocument/references`](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocument_references): the client calls gRPC `FindReferences` with an absolute filesystem path (after canonicalizing `PATH` on the client) plus a **0-based** line and **0-based** UTF-16 character offset on that line, matching LSP `Position`. The declaration is included in the results. The server resolves the path again, builds a `file://` URI, and returns a list of reference locations (`uri` + `range`). Default output is **JSON**; pass `--text` for one human-readable line per location. Use the same workspace and socket rules as `racli search` and `racli find-definition`.

### Symbol names

`find-definition`, `find-references`, `find-implementations` and `call-hierarchy` take symbol names as positional arguments, as well as, or instead of, a position given with `--path <PATH> --line <N> --character <N>`:

```sh
racli find-references mkfifo
racli find-references sys::mkfifo            # a module path from the crate root
racli find-references Fifo::name             # the type or trait that contains it
racli find-references SymbolMatch SymbolFilter   # several names
racli find-definition Fifo --kind struct --glob 'src/**'
racli find-references mkfifo --path src/main.rs --line 85 --character 31   # names and a position
```

Every positional argument is a name and must be a Rust path (`mkfifo`, `sys::mkfifo`, `Fifo::name`, `r#type`). Without `--glob`, a name is looked up in the whole workspace. The client finds each name's definitions with `workspace/symbol` over all symbol kinds, keeps the ones named exactly as given, and runs the command from each one's position. rust-analyzer resolves a module path itself; for `Type::member`, the client matches the member's LSP `containerName` instead. It searches the workspace first, then its dependencies.

`--kind` keeps definitions of one kind, as `racli search --text` prints it (`function`, `method`, `struct`, …, in any case). `--glob <PATTERN>` keeps definitions in files the pattern matches, as a shell glob would: `*` stays within one path component and `**` spans several, so `src/**` is everything under `src/`. A relative pattern matches files under the current directory. Both choose which definitions a name finds, not which results are shown, and both apply to names only.

Each definition gets its own result, and a definition found twice is run once. With `--text`, each result follows a line naming the symbol, in the layout of `racli search --text`; JSON output is an array of `{"symbol": {name, kind, containerName?, uri, range}, "result": …}`, where `result` is what the command prints for a position. A position given with names is labelled with the symbol it refers to. A position on its own prints as before. If no symbol has a name, the error lists the closest symbols to retry with, and the other names still run. If rust-analyzer's results were cut off at the server's `--symbol-search-limit`, the lookup says some definitions may be missing; a qualified name avoids that. See [ADR 001](docs/adrs/001-lookup-by-symbol.md).

### `racli document-symbols <PATH>`

Runs LSP [`textDocument/documentSymbol`](https://microsoft.github.io/language-server-protocol/specifications/specification-3-16/#textDocument_documentSymbol): the client calls gRPC `DocumentSymbols` with an absolute filesystem path (after canonicalizing `PATH` on the client). Unlike `find-definition`/`find-references`, this is **file-scoped only** — no line/character. The server resolves the path again, builds a `file://` URI, and returns a hierarchical outline of the symbols in that file (modules, structs, functions, methods nested under `impl` blocks, etc.), each with `name`, `kind`, `range`, `selectionRange`, an optional `detail`, and nested `children`. Default output is **JSON** (an array of top-level symbol objects, each recursively nesting `children`); pass `--text` for an indented tree (two spaces per nesting depth), or select explicitly with `--json`/`--output-format`. Use the same workspace and socket rules as `racli search`.

### `racli version`

Prints the client version from the binary (`CARGO_PKG_VERSION`). If a server answers on the project's socket, prints the server version from gRPC `GetVersion`. If the server is missing, errors, or does not respond within 10 seconds, a message is written to stderr and only the client line is printed to stdout.

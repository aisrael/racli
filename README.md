racli - a CLI tool for [rust-analyzer](https://github.com/rust-lang/rust-analyzer)
====

[![CI Tests](https://github.com/aisrael/racli/actions/workflows/ci.yml/badge.svg)](https://github.com/aisrael/racli/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/racli.svg)](https://crates.io/crates/racli)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE.md)

`racli` is meant to be a CLI for `rust-analyzer`. It's meant to:

- make using `rust-analyzer` more efficient, but allowing a single `rust-analyzer` process to be shared across multiple editors and agents
- provide an interface for AI agents to search and navigate a Rust codebase that's faster and more token-efficient than `grep`

See the [changelog](CHANGELOG.md) for change history.

## Installation

```sh
cargo install racli
```

rust-analyzer watches the workspace for changes itself. To have racli watch it instead and send rust-analyzer the changes, as racli did before, build with the `notify` feature: `cargo install racli --features notify`. That links the [notify](https://docs.rs/notify) crate, which on macOS loads CoreFoundation and CoreServices every time `racli` starts, CLI commands included. See [ADR 005](docs/adrs/005-file-watching-by-rust-analyzer.md).

## Usage

`racli` has three main usage modes:

- as a standalone MCP server for agents
- as a CLI client + local socket gRPC server
- in 'tee' mode as a drop-in replacement for `rust-analyzer`

### `racli` as an MCP server

Start it in the root of your Rust workspace. For example, to add it to Claude Code:

```sh
claude mcp add racli -- racli mcp
```

The server gives your agent these tools: `search`, `find_definition`, `find_references`, `find_implementations`, `call_hierarchy` and `document_symbols`. They work like the CLI commands. They take symbol names and 1-based positions, and they return text. See [ADR 006](docs/adrs/006-mcp-tools-mirror-the-cli.md).

Each `racli mcp` process starts its own rust-analyzer. To share one rust-analyzer, use `racli server` or `racli tee`.

### `racli server`

`racli server` starts rust-analyzer and a gRPC server on a Unix socket. Start it in the root of your Rust workspace:

```sh
racli server
```

This opens a socket at `/tmp/racli-<hash>.sock`, with a unique hash computed from the workspace directory. Set `RACLI_UNIX_SOCKET` to specify a socket path.

In another terminal, run a `racli` client command in the same workspace or in any of its subdirectories.

For example:

```sh
racli search SymbolMatch
racli find-definition run_cli_search
racli find-references run_cli_search
racli find-implementations CallHierarchyBackend
racli call-hierarchy run_cli_search
racli document-symbols src/search.rs
```

Each command prints one line of text for each result. Add `--json` to get JSON, or `--csv` for CSV output. Use `racli version` to see the client, server and rust-analyzer versions.

If a server already runs for the directory, `racli server` prints a message and exits.

### `racli tee`

`racli tee` is a drop-in replacement for `rust-analyzer`. Your editor starts it and talks to using LSP ([Language Server Protocol](https://microsoft.github.io/language-server-protocol/)) over stdio. It also serves the same socket as `racli server`. Your editor, the CLI and your agents can then share one `rust-analyzer` process.

Some editors cannot pass arguments to the server. For these editors, use `racli-tee`, a thin wrapper that functions the same as `racli tee`. For example, in VS Code:

```json
{ "rust-analyzer.server.path": "racli-tee" }
```

To check if racli serves the current directory, run `racli tee --check`. It exits with 0 if racli serves the directory. Otherwise, it exits with 1.

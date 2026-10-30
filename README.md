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

For now, `racli` requires [Cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html) to install:

```sh
cargo install racli
```

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


## Features

Because `racli` uses `rust-analyzer` under the hood, it can do more than just text search: it understands Rust types, traits, modules and call paths.

### Exact results

`racli find-references` and `racli find-definition` return real uses and definitions. They do not return matches in comments, strings or other symbols with the same name.

### Look up by name

- `racli find-references`
- `racli find-definition`
- `racli find-implementations`
- `racli call-hiearchy`

Each take one or more symbol names.

### Implementations

`find-implementations` lists the `impl` blocks of a trait or type, and the overrides of a trait method.

### Call hierarchy

`call-hierarchy` shows the callers and callees of a function, to the depth you choose.

### Read less code

`document-symbols` prints the outline of a file. `find-definition --hover` prints a signature and its docs. `find-definition --source` prints the whole body of a definition.

### Dependencies and `std`

Look up symbols in your dependencies and the standard library with `search --scope workspace-and-dependencies`.

### Short output

Each result is one `path:line:col` line, with 1-based positions as `grep` and `rustc` use. Use `--json` for programmatic parsing.

### One `rust-analyzer`

Several editors and agents can share one `rust-analyzer` process, so each does not start its own.

For more information, see:

- [Usage](#usage) for the MCP server, the CLI and `tee` mode
- [Architecture](docs/high-level-architecture.md) for how the parts fit together
- [ADRs](docs/adrs/) for the design decisions
- [The `racli` agent skill](.agents/skills/racli/SKILL.md) for every command and option

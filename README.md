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

### `racli version`

Prints the client version from the binary (`CARGO_PKG_VERSION`). If the server answers at the default socket, prints the server version from gRPC `GetVersion`. If the server is missing, errors, or does not respond within 10 seconds, a message is written to stderr and only the client line is printed to stdout.

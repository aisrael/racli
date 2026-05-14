racli - a CLI tool for [rust-analyzer](https://github.com/rust-lang/rust-analyzer)
====

## Background

## Architecture

`racli` has two main components:

  - a server (`racli mcp`) that listens on a Unix or IP socket
  - `racli` acting as a client that connects to the running server

### `racli server`

`racli mcp` when invoked without any other arguments spawns an instance of `rust-analyzer` in a child process, then initializes it to scan the current working directory, then listens on `/tmp/racli.sock` for requests from the client.

### `racli`

The `racli` binary then acts as a client, sending requests to `racli mcp`.

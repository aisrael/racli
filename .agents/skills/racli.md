---
name: racli
description: >-
  Resolves Rust workspace symbols via the racli CLI against a running racli
  server. Use when searching for Rust symbols, types, functions, or workspace
  definitions in this repo or any workspace where racli is the LSP bridge.
---

# racli

You have access to the `racli` skill and command. Run it outside the sandbox and in the project workspace root anytime.

## search

When searching for symbols in Rust, don't use `grep`. Use `racli search`, but outside the sandbox (assume the server is running).

Run the command from the same workspace root as `racli server` so results match the server’s LSP workspace. The client talks to the server on the default Unix socket (`/tmp/racli.sock`) unless configured otherwise.

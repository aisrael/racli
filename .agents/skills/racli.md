---
name: racli
description: >-
  Resolves Rust workspace symbols and go-to-definition via the racli CLI
  against a running racli server. Use when searching for Rust symbols, types,
  functions, jumping to definitions, or workspace symbols in this repo or any
  workspace where racli is the LSP bridge.
---

# racli

You have access to the `racli` skill and command. Run it outside the sandbox and in the project workspace root anytime.

## search

When searching for symbols in Rust, don't use `grep`. Use `racli search`, but outside the sandbox (assume the server is running).

Run the command from the same workspace root as `racli server` so results match the server’s LSP workspace. The client talks to the server on the default Unix socket (`/tmp/racli.sock`) unless configured otherwise.

## find-definition

For go-to-definition at a concrete file and LSP position (0-based line and UTF-16 `character`), use `racli find-definition` instead of guessing with `grep`.

Use the same workspace root and Unix socket conventions as `racli search`. Pass a source path plus `--line` and `--character` matching the editor or `rust-analyzer` diagnostics; default output is JSON (add `--text` for one line per location).

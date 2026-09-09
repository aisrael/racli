---
name: racli
description: >-
  Search Rust workspace symbols and resolve go-to-definition via the racli CLI
  against a running racli server (LSP workspace/symbol and textDocument/definition).
  Use for symbol lookup, definition jumps, and structured search in any workspace
  where racli bridges rust-analyzer. Prefer racli over grep for these tasks; fall
  back to grep only when racli returns nothing useful. For MCP, `racli mcp` runs
  rust-analyzer in-process (no socket); point the host at the workspace root cwd.
---

# racli

Run `racli` client commands from the **same directory the server used as its workspace root** (the cwd where `racli server` was started) so LSP paths and symbols match. The client talks to the server on a Unix domain socket: default `/tmp/racli.sock`, or override with the `RACLI_UNIX_SOCKET` environment variable if the server was started with a different path.

Assume **`racli server` is already running** for CLI subcommands (`search`, `find-definition`, `version`). Run client commands **outside the sandbox** when the environment blocks access to the Unix socket.

**MCP:** If the integration uses `racli mcp`, the MCP host must spawn it with **cwd = workspace root**; that process embeds rust-analyzer and does not require a separate `racli server`.

## search (`workspace/symbol`)

For **Rust symbol / identifier search**, do not use `grep`. Use `racli search <QUERY>`.

- **Query syntax:** A single unescaped `|` separates **alternative substring patterns** (each is a plain substring for rust-analyzer, not full regex). Example: `racli search 'Foo|Bar'`. A literal pipe in a pattern is written as `\|` (e.g. `racli search 'a\|b|c'` searches for `a|b` and for `c`; results are merged and deduped).
- **Output:** Default is **JSON** (array of objects with fields such as `name`, `kind`, `uri`, optional `range`). Use `--text` for one human-readable line per symbol, `--csv` or `--output-format csv` for CSV with headers, `--json` or `--output-format json` to be explicit about JSON.

## find-definition (`textDocument/definition`)

For **go-to-definition at a specific source location**, do not use `grep`. Use `racli find-definition` with a file path and LSP position.

- **Invocation:** `racli find-definition <PATH> --line <N> --character <N>`  
  `PATH` may be absolute or relative to the current directory; the client canonicalizes it before calling the server. `--line` is **0-based**; `--character` is **0-based UTF-16** offset on that line (same as LSP `Position` and rust-analyzer diagnostics).
- **Output:** Default is **JSON** (definition locations). Use `--text` for one human-readable line per location.

If the server just started, wait until analysis has caught up (for example until `racli search` returns sensible symbols) before relying on definitions.

## grep fallback

When searching for **plain text** or non-symbol strings, prefer `racli search` first for Rust-aware results. Use `grep` (or similar) **only** when `racli search` does not return anything meaningful for the task.

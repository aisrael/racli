---
name: racli
description: >-
  Search Rust workspace symbols, resolve go-to-definition, find references and
  implementations, outline a file, and walk callers/callees via the racli CLI
  against a running racli server (LSP workspace/symbol, textDocument/definition,
  textDocument/references, textDocument/implementation, textDocument/documentSymbol
  and call hierarchy). Use for symbol lookup, definition jumps, finding usages,
  finding trait/type impls, file outlines, call graphs, and structured search in any workspace
  where racli bridges rust-analyzer. Prefer racli over grep for these tasks; fall
  back to grep only when racli returns nothing useful. For MCP, `racli mcp` runs
  rust-analyzer in-process (no socket); point the host at the workspace root cwd.
---

# racli

Run `racli` client commands from **inside the project the server was started in**: its workspace root (the cwd where `racli server` or `racli tee` was started) or any subdirectory. Each project has its own Unix socket (`/tmp/racli-<hash>.sock`), and the client finds it automatically by checking its working directory and then each parent directory for a running server. Set `RACLI_UNIX_SOCKET` only to force a specific socket path.

Assume **`racli server` is already running** for CLI subcommands (`search`, `find-definition`, `find-references`, `find-implementations`, `document-symbols`, `call-hierarchy`, `version`). Run client commands **outside the sandbox** when the environment blocks access to the Unix socket.

**MCP:** If the integration uses `racli mcp`, the MCP host must spawn it with **cwd = workspace root**; that process embeds rust-analyzer and does not require a separate `racli server`.

## search (`workspace/symbol`)

For **Rust symbol / identifier search**, do not use `grep`. Use `racli search <QUERY>`.

- **Query syntax:** A single unescaped `|` separates **alternative substring patterns** (each is a plain substring for rust-analyzer, not full regex). Example: `racli search 'Foo|Bar'`. A literal pipe in a pattern is written as `\|` (e.g. `racli search 'a\|b|c'` searches for `a|b` and for `c`; results are merged and deduped).
- **Kinds and scope:** By default, only **types** (modules, structs, enums, traits, type aliases) from **workspace crates** are returned. Add `--kind all-symbols` to include functions, methods, constants, statics and fields. Add `--scope workspace-and-dependencies` to include dependency crates and the standard library. When a types-only search finds no types, rust-analyzer falls back to all symbols. When it finds any type, functions and methods are left out, so use `--kind all-symbols` whenever you are looking for a function, method, constant or field.
- **Output:** Default is **JSON** (array of objects with fields such as `name`, `kind`, `uri`, optional `range`). Use `--text` for one human-readable line per symbol, `--csv` or `--output-format csv` for CSV with headers, `--json` or `--output-format json` to be explicit about JSON.

## find-definition (`textDocument/definition`)

For **go-to-definition at a specific source location**, do not use `grep`. Use `racli find-definition` with a file path and LSP position.

- **Invocation:** `racli find-definition <PATH> --line <N> --character <N>`  
  `PATH` may be absolute or relative to the current directory; the client canonicalizes it before calling the server. `--line` is **0-based**; `--character` is **0-based UTF-16** offset on that line (same as LSP `Position` and rust-analyzer diagnostics).
- **Output:** Default is **JSON** (definition locations). Use `--text` for one human-readable line per location.

## find-references (`textDocument/references`)

For **finding usages of a symbol**, do not use `grep`. Use `racli find-references` with a file path and LSP position.

- **Invocation:** `racli find-references <PATH> --line <N> --character <N>`, with the same path and position rules as `find-definition`.
- **Output:** Default is **JSON** (reference locations, each with `uri` and `range`). The declaration is always included. Use `--text` for one human-readable line per location.

## find-implementations (`textDocument/implementation`)

For **finding the implementations of a trait or type**, do not use `grep`. Use `racli find-implementations` with a file path and LSP position.

- **Invocation:** `racli find-implementations <PATH> --line <N> --character <N>`, with the same path and position rules as `find-definition`.
- **Behavior:** On a trait or type, returns its `impl` blocks. On a trait method, returns each `impl`'s override. On a position inside an `impl` block, returns nothing, because it only goes from a trait or type to its impls.
- **Output:** Default is **JSON** (locations, each with `uri` and `range`). Use `--text` for one human-readable line per location.

## document-symbols (`textDocument/documentSymbol`)

For **an outline of the symbols in one file**, use `racli document-symbols <PATH>` instead of reading or grepping the whole file. It takes no line or character, since it covers the whole file.

- **Output:** Default is **JSON**: an array of top-level symbols, each with `name`, `kind`, `range`, `selectionRange`, optional `detail`, and nested `children` (for example, methods under their `impl` block). Use `--text` for an indented tree with one symbol per line.

## call-hierarchy (call hierarchy)

For **finding the callers or callees of a function or method**, use `racli call-hierarchy` with a file path and LSP position on the function's name.

- **Invocation:** `racli call-hierarchy <PATH> --line <N> --character <N> [--direction incoming|outgoing|both] [--depth <N>]`, with the same path and position rules as `find-definition`. `--direction` defaults to `both`, and `--depth` (default `1`) sets how many levels of callers or callees to walk. Each extra level costs one more round trip per node.
- **Output:** Default is **JSON**: an object with `item` (the resolved function), plus `incoming` and/or `outgoing` arrays. Each array entry has the caller's or callee's `name`, `kind`, `uri`, `range`, `selectionRange`, the `callSites` where the call appears, and nested `children` when `--depth` is greater than 1. Use `--text` for an indented outline.
- If the position matches more than one item, the extra candidates are listed in `otherCandidates` and printed as "did you mean" hints on stderr. If nothing matches, it prints `(no call hierarchy item at that position)`.

If the server just started, wait until analysis has caught up (for example until `racli search` returns sensible symbols) before relying on definitions, references, implementations or call hierarchies.

## grep fallback

When searching for **plain text** or non-symbol strings, prefer `racli search` first for Rust-aware results. Use `grep` (or similar) **only** when `racli search` does not return anything meaningful for the task.

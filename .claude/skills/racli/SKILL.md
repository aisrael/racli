---
name: racli
description: >-
  Look up Rust symbols with the racli CLI against a running racli server
  (rust-analyzer): search symbols, go to definitions, find references and
  implementations, outline a file, and walk callers/callees, by symbol name or
  file position. Prefer racli over grep for any Rust symbol lookup; use grep
  only for plain text or when racli finds nothing. For MCP, `racli mcp` starts
  its own rust-analyzer child process; start it with the workspace root as its cwd.
---

# racli

## Setup

- Run commands from the project's root or any subdirectory. The client finds the project's server (`racli server` or `racli tee`) by its socket, `/tmp/racli-<hash>.sock`. Set `RACLI_UNIX_SOCKET` only to force one.
- Assume the server is running. Run commands outside the sandbox if it blocks the socket.
- If the server just started, wait until `racli search` returns sensible symbols before trusting results.
- Output is text by default: one short grep-style line per result, far fewer tokens than JSON. Pass `--json` only when you need JSON.

## Look up by name

Pass symbol names to `find-definition`, `find-references`, `find-implementations` or `call-hierarchy`:

```sh
racli find-references run_cli_search
racli find-references SymbolMatch SymbolFilter    # several names
racli find-implementations RacliBackend::hover
```

- **Names** match exactly: a bare name (`mkfifo`), a module path from the crate root (`sys::mkfifo`, not `raw::mkfifo` for `sys::raw::mkfifo`), or a member qualified by its type or trait (`Fifo::name`). The workspace is searched first, then dependencies.
- **Each definition** gets its own result, under a header line `name [KIND] path:line:col`.
- **Narrow** with `--kind <KIND>` (`function`, `method`, `struct`, …) or `--glob <PATTERN>` (`'src/**'`, relative to the current directory). They choose definitions, not results.
- **Not found:** stderr lists the closest symbols; retry with a qualified name.
- **Cut off:** if stderr says rust-analyzer had more results than `--symbol-search-limit`, definitions may be missing (common with short names in dependencies, like `min`). Qualify the name, e.g. `core::cmp::min`.

## Look up by position

Use a position only when you already have one, e.g. from a compiler error:

```sh
racli find-references --path src/search.rs --line <LINE> --character <COL>
```

`--line` and `--character` are 1-based (`--character` in UTF-16 units). A printed `path:line:col` can be passed back as is. Names and a position can be combined in one call.

## Commands

- **`search <QUERY>`**: fuzzy symbol search; `--exact` keeps only exact names. `|` separates alternatives (`'Foo|Bar'`; `\|` is a literal pipe). Returns only types unless `--kind all-symbols`, so use that for functions, methods, constants and fields. `--scope workspace-and-dependencies` adds dependencies and std. It prints `name [KIND] path:line:col`.
- **`find-definition`**: where a symbol is defined. Add `--hover` for its signature and docs before calling it, or `--source` for its whole body before changing it, instead of reading the file.
- **`find-references`**: every use, including the declaration.
- **`find-implementations`**: a trait's or type's `impl` blocks, or a trait method's overrides. Nothing from inside an `impl`.
- **`call-hierarchy`**: callers and callees. `--direction incoming|outgoing|both` (default `both`), `--depth <N>` (default 1, one round trip per node per level). Prints "no call hierarchy item" for a non-function.
- **`document-symbols <PATH>`**: a file's outline, instead of reading the whole file. It prints an indented tree with 1-based ranges.

`find-definition`, `find-references` and `find-implementations` print `path:line:col: source line`. With `--json`, JSON keeps LSP's absolute URIs and 0-based ranges; with names, it's an array of `{"symbol": …, "result": …}`.

## MCP

`racli mcp` serves the same commands as MCP tools (`search`, `find_definition`, `find_references`, `find_implementations`, `call_hierarchy`, `document_symbols`), with the options as parameters: `names` (an array), `path`, `line` and `character` (1-based), `kind`, `glob`, `hover`, `source`, `exact`, `direction`, `depth`. Results are the CLI's text, with paths relative to the workspace root; pass `format: "json"` only when you need JSON. Unknown names and their closest symbols come as a second text block. A result that says rust-analyzer is still indexing may be incomplete.

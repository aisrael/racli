# ADR 002: 1-Based Positions and Grep-Style Text Output

- **Date:** 2026-10-27
- **Status:** Proposed.

## Context

racli's CLI used LSP's positions as they are. `--line` and `--character` were 0-based, and
`--text` printed each location as a `file://` URI and a 0-based range:

```text
file:///…/racli/fixtures/queue/src/sys.rs 2:7-2:13
```

That made `--text` hard to use on its own:

- **Positions didn't match anything else.** grep, rustc and editors count lines and columns from
  1, so a position from a compiler error had to be adjusted by hand before it could be passed in,
  and a position printed by `--text` was one off in an editor.
- **Paths were long and absolute,** and weren't `path:line` links that terminals and editors open.
- **There was no source text.** Reading what was at each location took another command, such as
  `sed -n`, per location.

[ADR 001](001-lookup-by-symbol.md) made lookups by name the usual way to run the position
commands, so typed positions are now rarer. When one is needed, it's usually copied from somewhere
that's 1-based.

## Decisions

### 1. `--line` and `--character` are 1-based

```sh
racli find-references --path fixtures/queue/src/sys.rs --line 3 --character 8
```

They count from 1, as grep, rustc and editors do. `--character` still counts UTF-16 units, as LSP
does. The client subtracts 1 from each before calling the server, so the gRPC API and the server
keep LSP's 0-based positions. `0` is rejected: "positions are 1-based, so the first line or
character is 1".

### 2. `--text` prints locations as `path:line:col`

```text
fixtures/queue/src/main.rs:81:22:         let _ = sys::mkfifo(PATH, 0o666);
fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
```

- **The path** is relative to the current directory when it's under it, and absolute otherwise,
  for example for a dependency's source. There's no `file://` prefix.
- **The line and column are 1-based,** as in decision 1, so a position printed by `--text` can be
  passed back to `--line` and `--character` as is.

### 3. Locations end with their source line

`find-definition`, `find-references` and `find-implementations` print each location's line of
source after the position, as grep does. The client reads it from the file, once per file per
command. If the file can't be read, the location is printed without it.

### 4. Every command's text output follows decisions 2 and 3

| Command | `--text` |
| --- | --- |
| `find-definition`, `find-references`, `find-implementations` | `path:line:col: source line` |
| `search` | `name [KIND] path:line:col` |
| `call-hierarchy` | the indented outline, with `path:line:col` for each item |
| `document-symbols` | the indented tree, with 1-based ranges such as `3:1-9:2` |

The header lines that name each symbol when the commands run by name (ADR 001, decision 5) use
`search`'s layout, and so do the closest symbols an unknown name lists.

### 5. JSON, CSV, gRPC and MCP keep LSP's positions

JSON and CSV output keep absolute URIs and 0-based ranges, as LSP gives them, so tools that read
them don't change. The gRPC API and the MCP tools also stay 0-based, so a position read from an MCP
result still works as an MCP input.

## Alternatives Considered

- **A new text format, such as `--lines`,** next to the old `--text`. Rejected: two text formats
  to document and keep working, for a format only people and agents read.
- **1-based output with 0-based input.** Rejected: a position copied from `--text` output would
  point one line and one column off.
- **1-based positions everywhere, including gRPC and MCP.** Rejected for now: those results are
  LSP-shaped, and their positions should match their own inputs (decision 5).
- **Paths relative to the workspace root** instead of the current directory. Rejected: a path
  relative to the current directory opens from the terminal it was printed in, wherever in the
  workspace that is.

## Consequences

- **Breaking:** scripts that pass `--line` or `--character` need to add 1 to each, and scripts that
  parse `--text` output need updating: paths are relative, positions are 1-based, and locations end
  with the source line.
- `--text` costs a read of each file it prints a location in.
- Positions written in earlier docs are 0-based. ADR 001's examples, for instance, predate this
  decision.

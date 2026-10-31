# ADR 002: 1-Based Positions and Grep-Style Text Output

- **Date:** 2026-10-27
- **Status:** Accepted.

## Context

The CLI used LSP's 0-based positions as they are, and `--text` printed locations as a `file://`
URI and a 0-based range, such as `file:///…/fixtures/queue/src/sys.rs 2:7-2:13`. grep, rustc and
editors count from 1, so positions had to be adjusted by hand in both directions. The long URIs
weren't `path:line` links, and reading what was at each location took another command.

## Decisions

### 1. `--line` and `--character` are 1-based

`--character` still counts UTF-16 units, as LSP does. The client subtracts 1 from each before
calling the server, and rejects `0`.

### 2. `--text` prints locations as `path:line:col`

```text
fixtures/queue/src/main.rs:81:22:         let _ = sys::mkfifo(PATH, 0o666);
fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
```

- The path is relative to the current directory, or absolute outside it.
- The line and column are 1-based, so a printed position can be passed back in as is.
- `find-definition`, `find-references` and `find-implementations` end each location with its
  source line, read once per file.
- `search` prints `name [KIND] path:line:col`, `call-hierarchy` prints `path:line:col` for each
  item, and `document-symbols` prints 1-based ranges.

### 3. JSON, CSV, gRPC and MCP keep LSP's positions

> [!NOTE]
> 2026-10-30 Superseded by [ADR 007](007-relative-paths-in-json.md).

They keep absolute URIs and 0-based ranges, so tools that read them don't change, and an MCP
result's positions still work as MCP inputs.

## Alternatives Considered

- **A second text format next to `--text`.** Rejected: two formats to keep, for output only people
  and agents read.
- **1-based output with 0-based input.** Rejected: a copied position would be one off.
- **1-based gRPC and MCP.** Rejected for now: their results are LSP-shaped (decision 3).

## Consequences

- **Breaking:** scripts that pass positions add 1 to each, and scripts that parse `--text` need
  updating.
- Positions in earlier docs, such as [ADR 001](001-lookup-by-symbol.md)'s examples, are 0-based.

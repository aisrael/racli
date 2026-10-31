# ADR 007: Relative Paths in JSON, CSV and MCP Output

- **Date:** 2026-10-30
- **Status:** Accepted.

## Context

[ADR 002](002-one-based-positions-and-text-output.md) decision 3 kept absolute `file://` URIs in
JSON, CSV, gRPC and MCP output, so that tools which read them would not change. The main readers
of this output are now agents, and each location pays for its URI in tokens. The same long prefix
repeats on every row.

For example, `racli find-references display_path --json` prints 19 locations in 2,966 bytes. The
absolute prefix (`file://` and the workspace directory) would add 836 bytes to those rows, which
is 28% more, with no new information. `--text` has printed a path relative to the current
directory since ADR 002 decision 2, and agents read it without trouble.

## Decisions

### 1. JSON, CSV and MCP print the same path as `--text`

The path is relative to the current directory when the file is under it, and absolute otherwise.
The `file://` prefix is gone. This replaces only the "absolute URIs" part of ADR 002 decision 3.

### 2. The `uri` key and column are now `path`

The value is no longer a URI, so the name changes with it. This applies to `search` (JSON and
CSV), the `symbol` object that `find-definition`, `find-references`, `find-implementations` and
`call-hierarchy` print, the location rows of the first three, and the items of `call-hierarchy`.

### 3. Everything else in ADR 002 decision 3 stays

JSON, CSV, gRPC and MCP keep 0-based ranges, so an MCP result's positions still work as MCP
inputs ([ADR 006](006-mcp-tools-mirror-the-cli.md)). gRPC keeps its URIs, which are the wire
format between the client and the server. Only the client's printed output changes.

## Alternatives Considered

- **Keep `uri` as the key and change only the value.** Rejected: a key named `uri` that holds a
  relative path misleads readers and tools.
- **Print both `uri` and `path`.** Rejected: it keeps the token cost this ADR removes.
- **A flag to choose URIs or paths.** Rejected: two forms to keep, for output that one form
  serves.
- **Always absolute paths.** Rejected: they keep most of the prefix cost.

## Consequences

- **Breaking:** scripts and tools that read `uri` from `--json`, `--csv` or MCP results must read
  `path` instead, and cannot assume the value is absolute.
- A path is relative to the directory the command (or the MCP server) runs from, so it can differ
  between callers. A caller that needs an absolute path joins it to that directory.

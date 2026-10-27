# ADR 001: Lookup by Symbol

- **Date:** 2026-10-27
- **Status:** Proposed.

## Context

`find-definition`, `find-references`, `find-implementations` and `call-hierarchy` each take a
file and an LSP position: `<PATH> --line <N> --character <N>`. Whoever runs them usually starts
from a name instead, so getting the position costs a `racli search` first, or a `grep`.

Agents feel this most. In a long session on another project, an agent told to prefer racli still
ran `grep` before most lookups, because one `grep` goes from a name to its uses while racli took
two or three calls. Finding the uses of `eval_call` meant a `search`, then a `find-references`
with the position it returned. Checking three names, as in `grep "NativeFn\|KeywordFn\|Handler"`,
took three times that.

`racli search` doesn't close the gap on its own:

- it matches substrings, so `search eval_call` also returns `test_eval_binary_pipe_into_call`;
- rust-analyzer resolves a module path written from the crate root, such as `sys::mkfifo`, but
  not a type's member, such as `Fifo::name`.

## Decisions

### 1. The position commands take symbol names as positional arguments

```sh
racli find-references mkfifo
racli find-references NativeFn KeywordFn Handler
racli call-hierarchy sys::mkfifo --direction incoming
```

A name is what a caller starts from, so it's the default way to use the four commands. Each takes
one or more names, and each must be a Rust path, such as `mkfifo`, `sys::mkfifo`, `Fifo::name` or
`r#type`.

### 2. A position is given with `--path`

```sh
racli find-references --path src/sys.rs --line 2 --character 7
```

`--path`, `--line` and `--character` go together, and give the one position a command runs from,
as `<PATH> --line --character` did before. Since the file is an option, every positional argument
is a name. An argument that isn't a Rust name, such as `src/sys.rs`, is a usage error that points
to `--path`. Usage errors exit with status 2, as clap's own do.

### 3. A name matches exactly, in the workspace first

racli asks rust-analyzer for symbols of every kind, then keeps only those named exactly as given.
It searches the workspace crates first. Only if nothing there matches does it search their
dependencies and the standard library, so `CString` finds std's `CString`.

### 4. A name can be qualified two ways

- **By a module path from the crate root:** `sys::mkfifo`, or `queue::sys::raw::mkfifo` with the
  crate's name. rust-analyzer resolves these itself. A path that doesn't start at the crate root,
  such as `raw::mkfifo` for `sys::raw::mkfifo`, doesn't match.
- **By the type or trait that contains a member:** `Fifo::name` or `Named::name`. rust-analyzer
  doesn't resolve these, so racli matches the member's LSP `containerName` against the last
  segment before the name.

A module path is tried first. The container is tried only if the path matches nothing.

### 5. Each definition gets its own result

The command runs from every definition the names find, and its results are grouped by definition.
racli never picks one silently, and an ambiguous name isn't an error. A definition found twice,
for example by `mkfifo` and `sys::mkfifo`, is run once.

- **`--text`:** a line names each definition, in the layout of `racli search --text`, and its
  results are indented under it. `call-hierarchy` prints only its trees, since each one starts
  with the item it was run from.
- **JSON:** an array of `{"symbol": {name, kind, containerName?, uri, range}, "result": …}`,
  where `result` is what the command prints for a position. The array is used even for a single
  definition, so the shape doesn't depend on how many there are.

A position on its own prints what it did before, in text and JSON.

### 6. A position given with names is one more target

```sh
racli find-references mkfifo --path src/main.rs --line 85 --character 31
```

The command runs from the position as it would on its own, after the names. To group it with the
names' results, racli labels it with the symbol it refers to: it finds the definition from the
position, takes the symbol's name from the outline of the definition's file, and labels it as a
lookup of that name would. If a name already found that definition, the position isn't run again.

### 7. An unknown name lists the closest symbols, and doesn't stop the others

```text
no symbol named mkfif; closest:
  mkfifo [FUNCTION] file:///…/sys.rs 24:15-24:21
  mkfifo [FUNCTION] file:///…/sys.rs 2:7-2:13
```

The list is rust-analyzer's own ranking of the fuzzy matches, up to ten. It's printed on stderr,
like racli's other client errors, and the command still runs for the names that were found.

### 8. `--kind` and `--glob` narrow the definitions names find

- `--kind` keeps definitions of one kind, as `racli search --text` prints it (`function`,
  `method`, `struct`, …, in any case).
- `--glob <PATTERN>` keeps definitions in files the pattern matches, as a shell glob would:
  `*` and `?` stay within one path component, and `**` spans several, so `src/**` is everything
  under `src/`. A relative pattern matches files under the current directory, and an absolute
  one, such as a dependency's directory in `~/.cargo/registry`, matches as is.

Both filter which definitions a name finds, not the command's results: `find-references mkfifo
--glob 'src/sys/**'` gives every reference to the `mkfifo` defined under `src/sys/`, wherever the
reference is. If they leave out every definition of a name, the error lists the ones they left
out. They apply to names only, so they're a usage error without a name.

LSP has nothing to filter by: `workspace/symbol` takes a query, and rust-analyzer's `searchScope`
extension picks crates, not paths. So racli filters the symbols rust-analyzer returns.

### 9. A search cut off at the limit says so

rust-analyzer returns at most `--symbol-search-limit` symbols per query, 1000 by default, and its
fuzzy matching doesn't rank exact matches first. So an exact match can fall past the limit and
never reach racli. In flt, with a limit high enough to cut nothing off:

| Name | Scope | Results | Exact matches | At positions | Past 1000 |
|---|---|---|---|---|---|
| `new` | workspace | 73 | 14 | 5–18 | 0 |
| `new` | with dependencies | 1459 | 738 | 278–1016 | 17 |
| `len` | with dependencies | 2597 | 106 | 1378–1520 | all 106 |

Within the workspace, every name tried had at most 259 results, so the limit didn't matter. With
dependencies, it did: at the default limit, a search for `len` returned 1000 results and not one
was named `len`.

LSP can't say whether a `workspace/symbol` result was cut off, and 1000 results could also be an
exact fit. So the server asks rust-analyzer for one more result than its limit. If it gets that
many, it drops the extra one and marks the response `truncated`. The client then says so:

- a name that finds nothing says it may exist after all:
  `no symbol named min (rust-analyzer had more results than the server's --symbol-search-limit, so
  some definitions may be missing; qualify the name or raise the limit)`;
- a name that finds definitions from a cut-off search warns that some may be missing;
- `racli search` warns on stderr.

A qualified name, such as `core::cmp::min`, narrows rust-analyzer's results before the limit
applies, so it's the usual way out.

### 10. The client looks names up

The lookup runs in the client, using the existing `Search`, `FindDefinition` and
`DocumentSymbols` RPCs, and the position RPCs are unchanged. The server only adds LSP's
`containerName` to its symbol messages, as `container_name`, for decision 4, and `truncated` to
its search responses, for decision 9. Since a client from
this change works with an older server, names work there too, except for those qualified by a type
or trait.

The MCP tools don't change for now. They should later get the same lookup by name, in some form.

## Alternatives Considered

- **A `--symbol <NAME>` option** in place of the position. Rejected: it made a name the special
  case, though it's what callers usually start from, and it couldn't be combined with a position
  or given more than once.
- **Keeping the file as a positional argument,** next to the names, and reading any argument
  that isn't a Rust name as the file. Rejected: every argument would have two possible meanings,
  told apart only by how it's written. With `--path`, each argument has one.
- **A path prefix filter (`--in <PATH>`)** instead of `--glob`. Rejected: a glob covers a
  directory (`src/**`), a file, and patterns a prefix can't, such as `src/**/tests.rs`.
- **A `find-symbol-and-references` subcommand,** and others like it. Rejected: names on every
  position command cover all pairings at once (decision 1).
- **Picking the first definition, or failing when a name is ambiguous.** Rejected: the first
  definition is often the wrong one, and failing makes the caller look up the position after all.
- **Changing the JSON shape only when a name is ambiguous.** Rejected: a script would need to
  handle both shapes.
- **Matching a module path anywhere in a symbol's path,** so `raw::mkfifo` works. Deferred:
  workspace symbols don't carry module paths, so it would take an outline request for each
  candidate's file to work out which module contains it. The error from decision 7 already shows
  the definitions to retry with.
- **Treating exactly `--symbol-search-limit` results as cut off.** Rejected: an exact fit would
  warn for nothing. Asking for one more result tells the two apart (decision 9).
- **Raising the default limit.** Rejected: it makes every `racli search` heavier, and only moves
  the cut-off further out.
- **A server RPC that looks names up.** Rejected for now: the client can do it with existing RPCs,
  and doing it there leaves the gRPC API almost unchanged.

## Consequences

- **Breaking:** the four position commands no longer take the file as a positional argument.
  `racli find-references src/sys.rs --line 2 --character 7` becomes `racli find-references --path
  src/sys.rs --line 2 --character 7`, and the old form is a usage error that says so.
- The four commands take `[NAME]...`, `--path`, `--line`, `--character`, `--kind` and `--glob`.
  A position on its own prints as before.
- New fields: `container_name` on the gRPC `LspSymbolInformation` and `LspWorkspaceSymbol`
  messages, and `truncated` on `SearchResponse`.
- rust-analyzer is started with `workspace.symbol.search.limit` one higher than
  `--symbol-search-limit`. racli's own searches still return at most the limit, but an editor's
  `workspace/symbol` requests under `racli tee` go to rust-analyzer as they are, so they can get
  one result more.
- New dependency: the `glob` crate, for `--glob`.
- When any name is given, JSON output has the grouped shape of decision 5.
- A lookup can cost several `Search` calls: one for the name, one more for a member qualified by a
  type, and the same again in the dependencies when the workspace has no match. A position given
  with names costs a `FindDefinition`, a `DocumentSymbols` and a `Search` more.
- Errors for names that find nothing exit with status 0, as racli's other client errors do. Usage
  errors exit with status 2.

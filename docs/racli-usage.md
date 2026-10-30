# racli client commands

This document describes the `racli` client commands. Each command asks a running racli server for an answer from `rust-analyzer`.

- [Before you start](#before-you-start)
- [Symbol names](#symbol-names)
- [Positions](#positions)
- [Output formats](#output-formats)
- [`search`](#search)
- [`find-definition`](#find-definition)
- [`find-references`](#find-references)
- [`find-implementations`](#find-implementations)
- [`call-hierarchy`](#call-hierarchy)
- [`document-symbols`](#document-symbols)
- [`version`](#version)

The examples with output use the small `fixtures/queue` project in this repository. The same commands work in any Rust workspace.

## Before you start

A racli server must run for the workspace. Start one with `racli server` or `racli tee`. See the [README](../README.md#usage).

- Run a client command in the workspace root or in any of its subdirectories. The client finds the server by its socket, `/tmp/racli-<hash>.sock`. To use a different socket, set `RACLI_UNIX_SOCKET`.
- A new server needs time to index the workspace. Until it finishes, results can be incomplete.

## Symbol names

`find-definition`, `find-references`, `find-implementations` and `call-hierarchy` take one or more symbol names. A name is one of these:

| Form | Example |
| --- | --- |
| A bare name | `mkfifo` |
| A module path from the crate root | `sys::mkfifo` |
| A member of a type or trait | `Fifo::name` |

How names match:

- Names match exactly. `mkfifo` does not match `mkfifo_at`.
- racli searches the workspace first, then the dependencies.
- If a name has more than one definition, the command runs for each definition. Each result starts with a header line: `name [KIND] path:line:col`.
- If you give a name that does not exist, racli lists the closest symbols on stderr. The command still runs for the other names.
- A module path picks one definition when several have the same name. For example, `sys::mkfifo` does not match `sys::raw::mkfifo`.

Two options narrow the definitions that a name finds. They choose definitions, not results.

| Option | Meaning |
| --- | --- |
| `--kind <KIND>` | Use only definitions of this kind, as `racli search --text` prints it. For example, `function`, `method` or `struct`. |
| `--glob <PATTERN>` | Use only definitions in files that match the glob, for example `'src/**/*.rs'`. A relative pattern starts at the current directory. |

If rust-analyzer found more results than `--symbol-search-limit`, racli warns on stderr. Some definitions can be missing. This is common for short names in dependencies. Use a qualified name, for example `core::cmp::min`.

## Positions

Use a position when you already have one, for example from a compiler error. The same four commands accept it:

```sh
racli find-references --path src/sys.rs --line 3 --character 8
```

- `--path` is a Rust source file, absolute or relative to the current directory.
- `--line` and `--character` are 1-based. `--character` counts UTF-16 units, as LSP does.
- `--line` and `--character` are both required with `--path`.
- A value of `0` is an error. The command exits with status 2.
- You can pass the `path:line:col` that a command prints back to another command.
- You can give names and a position in one command.

## Output formats

| Option | Meaning |
| --- | --- |
| `--text` | One short line for each result. This is the default. |
| `--json` | JSON. It keeps LSP's absolute URIs and 0-based ranges. |
| `--csv` | CSV with a header row. Only `search` has it. |
| `--output-format <FORMAT>` | `search` and `document-symbols` only. Same as the options above. |

Text output uses fewer tokens than JSON. Use `--json` only when a program reads the output. For commands that take names, JSON is an array of `{"symbol": …, "result": …}`.

## `search`

Finds symbols by a fuzzy name match.

```sh
racli search <QUERY> [--kind <KIND>] [--scope <SCOPE>] [--exact] [--text | --json | --csv]
```

| Option | Meaning |
| --- | --- |
| `--kind only-types` | Return types only. This is the default. |
| `--kind all-symbols` | Also return functions, methods, constants, statics and fields. |
| `--scope workspace` | Search the workspace crates only. This is the default. |
| `--scope workspace-and-dependencies` | Also search dependencies and the standard library. |
| `--exact` | Keep only symbols with exactly the name in the query. |

Use `|` in the query to search for more than one pattern. Use `\|` for a literal pipe. This is not full regular-expression syntax.

Each result is `name [KIND] path:line:col`. A header line gives the number of symbols.

```sh
racli search name --kind all-symbols --exact --text
```

```text
flat: 2 symbol(s)
name [FUNCTION] fixtures/queue/src/sys.rs:38:8
name [FUNCTION] fixtures/queue/src/sys.rs:32:8
```

More examples:

```sh
racli search mkfifo
racli search 'Foo|Bar'
racli search new --kind all-symbols
racli search min --scope workspace-and-dependencies --kind all-symbols
racli search mkfifo --csv
```

`--exact` is still fuzzy about the module path. rust-analyzer resolves a path such as `sys::mkfifo` before racli filters the names.

## `find-definition`

Shows where a symbol is defined.

```sh
racli find-definition <NAME>… [--kind <KIND>] [--glob <PATTERN>] [--hover] [--source] [--text | --json]
```

| Option | Meaning |
| --- | --- |
| `--hover` | Also print the signature and the doc comment of each definition. |
| `--source` | Also print the whole source of each definition, with its doc comment. |

Use `--hover` to read a signature before you call a function. Use `--source` to read a body before you change it. Both save you from opening the file.

```sh
racli find-definition sys::mkfifo
```

```text
mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8
  fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
```

With `--source`, the numbered source lines follow:

```sh
racli find-definition sys::mkfifo --source
```

```text
mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8
  fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
      3  pub fn mkfifo(path: &str, mode: u16) -> i32 {
      4      let c_path = CString::new(path).unwrap();
      5      let p_path = c_path.as_ptr();
      6      unsafe {
      7          return raw::mkfifo(p_path, mode as libc::mode_t);
      8      }
      9  }
```

More examples:

```sh
racli find-definition Fifo Named              # several names
racli find-definition Fifo --kind struct
racli find-definition Named --hover
racli find-definition --path src/main.rs --line 81 --character 22
```

## `find-references`

Lists every use of a symbol. The list always includes the declaration.

```sh
racli find-references <NAME>… [--kind <KIND>] [--glob <PATTERN>] [--text | --json]
```

Each result is `path:line:col: source line`.

```sh
racli find-references sys::mkfifo
```

```text
fixtures/queue/src/main.rs:81:22:         let _ = sys::mkfifo(PATH, 0o666);
fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
```

More examples:

```sh
racli find-references SymbolMatch SymbolFilter   # several names
racli find-references Fifo::name
racli find-references mkfifo --glob 'src/**'
racli find-references --path src/sys.rs --line 3 --character 8
```

## `find-implementations`

Lists the `impl` blocks of a trait or type. For a trait method, it lists the overrides. It returns nothing for a symbol inside an `impl` block.

```sh
racli find-implementations <NAME>… [--kind <KIND>] [--glob <PATTERN>] [--text | --json]
```

Each result is `path:line:col: source line`.

```sh
racli find-implementations Named
racli find-implementations Named::name
racli find-implementations --path fixtures/queue/src/sys.rs --line 31 --character 11
```

## `call-hierarchy`

Shows the callers and the callees of a function.

```sh
racli call-hierarchy <NAME>… [--direction <DIRECTION>] [--depth <N>] [--kind <KIND>] [--glob <PATTERN>] [--text | --json]
```

| Option | Meaning |
| --- | --- |
| `--direction incoming` | Callers only. |
| `--direction outgoing` | Callees only. |
| `--direction both` | Callers and callees. This is the default. |
| `--depth <N>` | How many levels of callers or callees to get. The default is 1. Each level costs one request for each item. |

The command prints an indented outline. Callers follow `<- callers:` and callees follow `-> callees:`. For a name that is not a function, it prints `no call hierarchy item`.

```sh
racli call-hierarchy sys::mkfifo --direction incoming
```

```text
mkfifo (FUNCTION) fixtures/queue/src/sys.rs:3:8
<- callers:
  main (FUNCTION) fixtures/queue/src/main.rs:58:4
```

More examples:

```sh
racli call-hierarchy sys::mkfifo --direction outgoing
racli call-hierarchy sys::mkfifo --depth 3
racli call-hierarchy --path fixtures/queue/src/sys.rs --line 3 --character 8 --direction incoming
```

## `document-symbols`

Prints the outline of one file. Use it instead of reading the whole file.

```sh
racli document-symbols <PATH> [--text | --json] [--output-format <FORMAT>]
```

The command prints an indented tree. Nested symbols are indented under their parent. Each line is `name (KIND) start-line:start-col-end-line:end-col`, with 1-based ranges.

```sh
racli document-symbols fixtures/queue/src/sys.rs
```

One line of the output is:

```text
mkfifo (FUNCTION) 3:1-9:2
```

`document-symbols` takes a file path, not a symbol name.

## `version`

Prints the versions of the client, the server and rust-analyzer.

```sh
racli version
```

If it cannot reach a server within 10 seconds, it prints the client version and an error on stderr.

## Related documents

- [ADR 001: Lookup by Symbol](adrs/001-lookup-by-symbol.md)
- [ADR 002: 1-Based Positions and Grep-Style Text Output](adrs/002-one-based-positions-and-text-output.md)
- [High-level architecture](high-level-architecture.md)

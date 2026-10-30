# Benchmarks

These [Criterion](https://github.com/bheisler/criterion.rs) benchmarks compare racli with a `grep` approximation of the same lookup. They run against racli's own source tree.

## What they measure

Each bench file covers one command:

| Bench                  | Command                                          |
| ---------------------- | ------------------------------------------------ |
| `search`               | `racli search` (also with `--kind all-symbols`)  |
| `find_definition`      | `racli find-definition` (with and without hover) |
| `find_references`      | `racli find-references`                          |
| `find_implementations` | `racli find-implementations`                     |
| `call_hierarchy`       | `racli call-hierarchy --direction incoming`      |
| `document_symbols`     | `racli document-symbols`                         |

Each bench group has these variants:

- `grep`: the `grep` approximation.
- `racli_*`: the CLI calling a running `racli server`, from a position and by symbol name.
- `mcp_*`: the matching `racli mcp` tool, from a position and by symbol name. These are off by default (see below).

Before it measures, each bench starts a server and waits until rust-analyzer answers the first query. The benches need Unix, because racli uses Unix domain sockets. On other platforms they only print a message.

## Running

```sh
cargo bench                      # all benches
cargo bench --bench search       # one bench file
cargo bench --bench search -- grep   # only variants whose name matches "grep"
```

Extra arguments after `--` go to Criterion. Results are in `target/criterion/`.

## MCP benches

The `mcp_*` variants are off by default. To run them, set `RACLI_BENCH_MCP` to any non-empty value other than `0` or `false`:

```sh
RACLI_BENCH_MCP=1 cargo bench
```

When the variable is not set, the benches do not start `racli mcp` and print a note that they skip the MCP variants.

## Layout

`support/mod.rs` has the shared helpers: finding the `racli` binary, starting a server or an MCP session, and waiting until rust-analyzer is ready.

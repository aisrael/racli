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

## Token benchmark

`tokens` is not a Criterion bench. It measures how many tokens each answer puts into an agent's context, for `find-definition`, `find-references`, `find-implementations` and `call-hierarchy`. It compares racli with the `grep` and `sed` pipelines that an agent would run without it, and scores each output against racli's locations.

```sh
cargo bench --bench tokens
```

- With `ANTHROPIC_API_KEY` set, it counts tokens with the Anthropic `count_tokens` endpoint through `curl` (version 8.3 or later). The output of racli and `grep` for this repository goes to the endpoint.
- Without a key, or with `RACLI_BENCH_TOKENS=estimate`, it prints an estimate of bytes divided by 2. These are not Claude token counts.
- It prints Markdown tables, and writes `results.csv`, `grep-only.txt` (the `grep` locations that racli does not give), `grep-missed.txt` (the racli locations that `grep` does not find) and a token cache.
- The symbols, the commands of each tool and the workspace come from a YAML file. By default it reads `config/racli.yaml` (racli's own tree). To use another, set `RACLI_BENCH_CONFIG`. For example, `RACLI_BENCH_CONFIG=path/to/other.yaml cargo bench --bench tokens` runs it on another Rust workspace, and skips itself if the `workspace` in the file has no `Cargo.toml`. The keys are described at the top of `racli.yaml`.
- Results go to `target/<name>/`, where `<name>` is the `name` in the YAML file.

## Layout

`support/mod.rs` has the shared helpers: finding the `racli` binary, starting a server or an MCP session, and waiting until rust-analyzer is ready.

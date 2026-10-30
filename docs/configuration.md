# racli configuration

You configure `racli` with environment variables, command-line options and one build feature. There is no configuration file.

- [Environment variables](#environment-variables)
- [Sockets](#sockets)
- [Logging](#logging)
- [Command-line options](#command-line-options)
- [Build feature: `notify`](#build-feature-notify)
- [Fixed limits](#fixed-limits)

## Environment variables

| Variable                   | Used by                           | Default | Purpose                                 |
| -------------------------- | --------------------------------- | ------- | --------------------------------------- |
| `RACLI_UNIX_SOCKET`        | server, `tee` and client commands | not set | Use this socket path.                   |
| `RACLI_DERIVE_SOCKET_PATH` | server, `tee` and client commands | `1`     | Turn per-project sockets on or off.     |
| `RACLI_LOG_LEVEL`          | client commands                   | `info`  | Log level for client commands.          |
| `RACLI_SERVER_LOG_LEVEL`   | `server`, `mcp` and `tee`         | `info`  | Log level for racli's own messages.     |
| `RACLI_SERVER_LOG_FILE`    | `server`, `mcp` and `tee`         | not set | Write logs to this file, not to stderr. |

An empty value is the same as an unset variable.

## Sockets

`racli server` and `racli tee` listen on a Unix socket. Client commands connect to it. `racli mcp` does not use a socket.

By default, each project has its own socket. This lets several projects, or several editor windows, each run their own racli and rust-analyzer. The socket path is `/tmp/racli-<hash>.sock`. The hash comes from the working directory of the server, after racli resolves symbolic links.

### `RACLI_UNIX_SOCKET`

Set this to an exact socket path. It applies to the server and to the client. It overrides all other socket settings, so set the same value for both.

```sh
RACLI_UNIX_SOCKET=/tmp/my-project.sock racli server
RACLI_UNIX_SOCKET=/tmp/my-project.sock racli search SymbolMatch
```

### `RACLI_DERIVE_SOCKET_PATH`

| Value                         | Result                                                            |
| ----------------------------- | ----------------------------------------------------------------- |
| not set, empty, `1` or `true` | Each project has its own socket. This is the default.             |
| `0` or `false`                | The server and the clients all use one socket, `/tmp/racli.sock`. |
| any other value               | racli prints a warning and acts as if the value is `0`.           |

The value is not case-sensitive. A client with this setting does not find per-project servers. A server with this setting does not serve per-project clients.

### How a client finds the server

If `RACLI_UNIX_SOCKET` is set, or per-project sockets are off, the client uses that socket. It does not search.

Otherwise, the client checks the socket for its current directory. Then it checks each parent directory, one at a time. It connects to the nearest socket that has a running server. A command therefore works in any subdirectory of the project that `racli server` or `racli tee` started in.

If the client finds no server, it prints a warning with the directory and the socket it expected. Then it tries the socket for the current directory.

## Logging

Two sets of variables control logging. One set is for client commands. The other set is for the long-running modes: `server`, `mcp` and `tee`.

### Log levels

Set a level as a name or as a number:

| Number | Name    |
| ------ | ------- |
| `0`    | `off`   |
| `1`    | `error` |
| `2`    | `warn`  |
| `3`    | `info`  |
| `4`    | `debug` |
| `5`    | `trace` |

Names are not case-sensitive. If the value is not valid, racli prints a warning on stderr and uses `info`.

### Client commands

`RACLI_LOG_LEVEL` sets the level. Client commands log to stderr.

```sh
RACLI_LOG_LEVEL=debug racli find-references run_cli_search
```

### `server`, `mcp` and `tee`

- `RACLI_SERVER_LOG_LEVEL` sets the level for racli's own messages. Messages from other libraries stay at `info`.
- `RACLI_SERVER_LOG_FILE` writes the logs to a file. If it is not set, the logs go to stderr. Logs never go to stdout, because `mcp` and `tee` use stdout for their protocols.

For a relative file path, racli starts at the working directory. It creates missing directories and adds new lines to the end of an existing file. If racli cannot open the file, it prints an error and exits with status 1.

```sh
RACLI_SERVER_LOG_LEVEL=debug RACLI_SERVER_LOG_FILE=.racli/racli.log racli server
```

A relative path such as `.racli/racli.log` gives each project its own log. You can add `.racli/` to the `.gitignore` of the project.

The same variables apply when an editor or an agent starts `racli mcp` or `racli tee`. Set them in the `env` of the MCP server entry or in the editor's environment. For example, in an MCP configuration:

```json
{
  "mcpServers": {
    "racli": {
      "command": "racli",
      "args": ["mcp"],
      "env": {
        "RACLI_SERVER_LOG_LEVEL": "debug",
        "RACLI_SERVER_LOG_FILE": ".racli/racli.log"
      }
    }
  }
}
```

## Command-line options

### `--symbol-search-limit <N>`

Accepted by `racli server`, `racli mcp`, `racli tee` and `racli-tee`. It sets the largest number of results that rust-analyzer returns for one `workspace/symbol` query. The default is `1000`. The smallest value is `1`. The default of rust-analyzer is `128`.

racli asks rust-analyzer for one more result than the limit. This lets racli tell a list that is cut off from a list that exactly fits. If rust-analyzer returns more results than the limit, racli removes the extra results. `racli search` then prints a warning on stderr, and the response has `truncated` set. The limit applies to each `|`-separated pattern.

Names of short symbols in dependencies can reach the limit, so some definitions can be missing. Use a qualified name, for example `core::cmp::min`, or raise the limit:

```sh
racli server --symbol-search-limit 5000
```

### `racli tee --check`

Accepted by `racli tee` and `racli-tee`. It does not start a server. It asks the server on the socket for the current directory for its version.

- If a server answers, it prints the version and the socket and exits with status 0.
- If not, it prints the reason on stderr and exits with status 1.

Unlike client commands, `--check` does not look in parent directories.

### `racli server --port <PORT>`

`--port` is reserved. It has no effect, because racli servers use only Unix sockets.

## Build feature: `notify`

rust-analyzer watches the workspace for changes itself. This is the default.

To have racli watch the workspace and send the changes to rust-analyzer, build with the `notify` feature:

```sh
cargo install racli --features notify
```

This links the [notify](https://docs.rs/notify) crate. On macOS, the crate loads CoreFoundation and CoreServices every time `racli` starts. This includes client commands. See [ADR 005](adrs/005-file-watching-by-rust-analyzer.md).

## Fixed limits

You cannot change these values:

| Limit                                | Value      |
| ------------------------------------ | ---------- |
| Time to wait for each client request | 60 seconds |
| Time to wait for `racli version`     | 10 seconds |

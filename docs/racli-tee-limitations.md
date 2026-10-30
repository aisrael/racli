# Limitations of `racli tee`

`racli tee` lets an editor use racli in place of `rust-analyzer`. The editor talks LSP over stdio to `racli tee`. `racli tee` sends the editor's messages to one shared `rust-analyzer` through its own socket. Because the `rust-analyzer` is shared, `racli tee` does not act exactly like a `rust-analyzer` that only your editor uses.

These limitations also apply to `racli-tee`.

- [The editor's `initialize` message is mostly ignored](#the-editors-initialize-message-is-mostly-ignored)
- [Requests run one at a time](#requests-run-one-at-a-time)
- [Few server messages reach the editor](#few-server-messages-reach-the-editor)
- [Shutdown does not stop rust-analyzer](#shutdown-does-not-stop-rust-analyzer)

## The editor's `initialize` message is mostly ignored

racli starts `rust-analyzer` and does the LSP `initialize` handshake itself. When the editor sends `initialize`, `racli tee` answers with the `InitializeResult` of that handshake. It does not send the editor's message to `rust-analyzer`.

As a result:

- The editor's client capabilities are ignored.
- The editor's `initializationOptions` are ignored. Settings that your editor sends to `rust-analyzer` in this message do not reach it.
- The capabilities in the answer are the capabilities of the shared `rust-analyzer`, not a set that depends on your editor.

## Requests run one at a time

LSP allows concurrent requests, but `racli tee` handles them in order. It reads the editor's messages one at a time. It sends each request to the server and waits for the answer before it reads the next message. This keeps the order of the messages. It also means:

- A slow request, such as a large `workspace/symbol` query, delays the requests after it.
- `$/cancelRequest` is ignored. The editor cannot cancel a request that is waiting or running.

The editor's requests also share the same queue as the requests from CLI clients of the same socket. `racli mcp` does not share this queue, because it starts its own `rust-analyzer`.

## Few server messages reach the editor

`racli tee` sends only these `rust-analyzer` notifications to the editor:

- `textDocument/publishDiagnostics`
- `experimental/serverStatus`
- `window/showMessage`
- `window/logMessage`

The editor does not get other notifications, such as progress reports.

`racli tee` does not forward requests that `rust-analyzer` sends to the editor, for example a request to register a capability. When the editor sends an answer to such a request, `racli tee` drops it.

## Shutdown does not stop rust-analyzer

Other clients share the `rust-analyzer`, so `racli tee` answers the editor's `shutdown` request itself. The `rust-analyzer` keeps running. `racli tee` stops when the editor sends `exit` or closes stdin, or when it gets SIGINT or SIGTERM.

## See also

- [Configuration](configuration.md) for logging and the `--check` option of `racli tee`
- [High-level architecture](high-level-architecture.md) for how `racli tee` fits with the server

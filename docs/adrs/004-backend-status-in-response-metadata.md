# ADR 004: Backend Status in Response Metadata

- **Date:** 2026-10-28
- **Status:** Accepted.

## Context

rust-analyzer takes a while after starting to load the workspace, run build scripts and proc
macros, and index. Until then it answers with partial results, or with `null` for files it hasn't
loaded. Clients couldn't tell: the integration tests polled `search` until it returned something,
and `racli tee`'s stdio test failed when it asked for a file's symbols too early.

racli already knows. It asks rust-analyzer for `experimental/serverStatus` notifications, whose
`quiescent` is `true` once there is no pending work and whose `health` is `error` when, for example,
the workspace failed to load. `LspEvents` caches the latest one. But the only way to read it was the
`LspEvents` passthrough stream, which [ADR 003](003-high-level-client.md) keeps out of `Client`.

## Decisions

### 1. Every gRPC response says what state the backend was in

The server adds the latest status to the metadata of every response, after producing it:

- `racli-backend`: `ready`, `indexing` or `error`.
- `racli-backend-message-bin`: rust-analyzer's message, only with `error`.

`ready` means quiescent and not `error`. `indexing` means not yet quiescent, including before
rust-analyzer's first status. The handlers set it, so every RPC carries it, `GetVersion` included.

### 2. A backend that isn't ready still answers

Requests aren't refused or delayed. rust-analyzer's partial answers are still useful, and
`quiescent` turns `false` again whenever it reloads, such as after a `Cargo.toml` change.

### 3. `Client` returns each value with its status

Every `Client` method returns `Reply { value, backend }`, where `backend` is a `BackendStatus`:
`Ready`, `Indexing`, `Error(message)`, or `Unknown` when the server sent none, as a racli from
before this decision doesn't. The status belongs to that answer, so a caller can tell that one
result may be incomplete.

## Alternatives Considered

- **A `GetStatus` RPC.** Rejected: a separate call can't say whether a particular answer was
  produced while rust-analyzer was indexing.
- **A `not ready` gRPC error.** Rejected: see decision 2.
- **A status field in every response message.** Rejected: eleven messages and handlers to change,
  and server state mixed into LSP-shaped results.
- **`Client` remembers the last status it saw.** Rejected: with concurrent calls the status isn't
  tied to an answer.

## Consequences

- Tests wait for the backend by polling any call, such as `get_version`, until it's `Ready`.
- Callers read `reply.value`; CLI commands can warn when an answer came back while `Indexing`.
- `ready` depends on rust-analyzer sending `experimental/serverStatus`. One that doesn't would stay
  `indexing`.

# ADR 005: File Watching by rust-analyzer

- **Date:** 2026-10-28
- **Status:** Accepted.

## Context

rust-analyzer reads the workspace from disk, so it needs to hear about files that change after it
starts. racli ran its own watcher for this: `FileWatcherActor` watched the workspace root
recursively with the [notify](https://docs.rs/notify) crate, and sent rust-analyzer
`workspace/didChangeWatchedFiles`. racli told rust-analyzer it supports dynamically registered
watchers, so rust-analyzer left watching to racli. In `racli tee` the editor never sees those
registrations, since racli answers them itself, so racli's watcher was the only one.

On macOS, notify uses FSEvents through `fsevent-sys`, which links the CoreFoundation and
CoreServices frameworks into the `racli` binary. macOS loads every linked library when a process
starts, before `main`, whether or not it's used. So every CLI command paid for them although only
`racli server`, `racli tee` and `racli mcp` watch files. Measured on Apple Silicon:

- `racli --version` takes 2.4 ms; built without notify, 1.7 ms.
- A minimal Tokio binary takes 1.3 ms; linking the two frameworks, without calling them, makes it
  2.1 ms.
- A binary that links all of racli but never calls `racli::run()` takes the same 2.4 ms, so the
  cost isn't in racli's code and can't be skipped at runtime.

That 0.7 ms is about a quarter of a typical CLI command, and enough to make `racli call-hierarchy`
lose to `grep` in racli's benchmarks.

rust-analyzer can watch files itself: with `files.watcher` set to `server`, it uses its own notify
watcher in its own process, as it does for editors that don't watch files for it.

## Decisions

### 1. rust-analyzer watches the workspace by default

racli starts rust-analyzer with `files.watcher: "server"` and doesn't advertise watched-files
dynamic registration. racli has no file watcher and doesn't depend on notify. This applies to
`racli server`, `racli tee` and `racli mcp` alike.

### 2. racli's own watcher is the `notify` feature

Building with `--features notify` brings back `FileWatcherActor` and the old initialization, for
setups where rust-analyzer's watcher falls short. The feature is chosen at build time for the whole
binary, so that build loads the frameworks for CLI commands too.

### 3. Both builds are tested

An integration test changes a file after startup and waits for the change to show up in `search`.
CI runs it, and clippy, with and without the feature.

## Alternatives Considered

- **notify's kqueue backend.** It doesn't link the frameworks, but it opens a file descriptor per
  watched file and directory. racli watches the whole workspace, `target/` included, which would
  exceed macOS's default limit of 256 open files.
- **A separate client binary without notify.** The server keeps FSEvents, but the subcommands that
  start a server would have to run another binary, and both would need installing and versioning
  together.
- **Loading CoreServices with `dlopen` only in the server.** notify can't do this, so racli would need
  its own unsafe FSEvents bindings.
- **Skipping the watcher at runtime in client commands.** The client never starts it already; the cost
  is paid when the binary is loaded.

## Consequences

- CLI commands start about 0.7 ms faster on macOS, and the default build has one dependency fewer.
- File watching is rust-analyzer's: what it watches and ignores follows its own rules and settings,
  rather than racli's filter for `*.rs`, `Cargo.toml` and `Cargo.lock`.
- The feature adds a second configuration whose code is behind `#[cfg(feature = "notify")]`, and
  which has to be kept building and tested.

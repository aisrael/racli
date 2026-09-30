//! ractor supervision tree for `racli server` / `racli mcp`: startup and shutdown sequencing of the
//! rust-analyzer child, the workspace file watcher, and the gRPC/MCP front end.
//!
//! ```text
//! RootActor
//! ├── BackendSupervisor
//! │   ├── RustAnalyzerActor
//! │   └── FileWatcherActor
//! └── GrpcFrontend | McpFrontend
//! ```
//!
//! Children start top to bottom and stop in reverse order.

/// [`backend::BackendSupervisor`]: owns the rust-analyzer and file watcher actors plus the shared [`crate::racli_session::RacliSession`].
pub(crate) mod backend;
/// [`file_watcher::FileWatcherActor`]: forwards workspace filesystem changes to rust-analyzer.
pub(crate) mod file_watcher;
/// gRPC and MCP front-end actors that serve requests from the shared session.
pub(crate) mod frontend;
/// [`root::RootActor`]: process-level supervisor for the backend and one front end.
pub(crate) mod root;
/// [`rust_analyzer::RustAnalyzerActor`]: owns the rust-analyzer LSP child and serializes requests to it.
pub(crate) mod rust_analyzer;

use ractor::ActorProcessingErr;
use ractor::SpawnErr;

/// Unwraps a child's startup failure so its typed error can be downcast by the caller; other spawn errors are boxed as-is.
pub(crate) fn startup_error(err: SpawnErr) -> ActorProcessingErr {
    match err {
        SpawnErr::StartupFailed(inner) => inner,
        other => Box::new(other),
    }
}

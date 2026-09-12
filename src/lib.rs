#![doc = include_str!("../README.md")]

/// CLI argument parsing and subcommand dispatch for the `racli` binary.
mod cli;
/// gRPC client helpers for talking to `racli server` over a Unix socket.
pub mod client;
/// `racli find-definition`: CLI arguments and formatting for LSP go-to-definition results.
pub mod find_definition;
/// `racli find-references`: CLI arguments and formatting for LSP find-references results.
pub mod find_references;
/// Unix-socket gRPC server for `racli server`.
pub mod grpc_server;
/// Log-level parsing and stderr tracing setup for the client subcommands.
pub mod logging;
/// Generic LSP client.
pub mod lsp_client;
/// Maps `lsp_types` values into racli protobuf shapes.
pub mod lsp_map;
/// MCP server over stdio (`rmcp`); tools use an in-process rust-analyzer session and workspace watcher.
pub mod mcp;
/// Protobuf and tonic-generated types for the Racli gRPC API.
pub mod proto;
/// Shared live workspace backend (rust-analyzer + watcher + [`RacliSession`]) for gRPC and MCP.
pub mod racli_live_backend;
/// Shared gRPC/MCP backend (rust-analyzer + [`crate::server::Core`]).
pub mod racli_session;
/// `rust-analyzer` LSP child process used by `racli server`.
pub mod rust_analyzer;
/// `racli search` CLI and response formatting.
pub mod search;
/// Shared server logic and future service wiring.
pub mod server;
/// Transport layer components
pub mod transport;
/// Shared small helpers (e.g. Unix socket path resolution).
pub mod utils;
mod workspace_file_watcher;

pub use cli::RunError;
pub use cli::run;
pub use grpc_server::GrpcServerError;
pub use grpc_server::run_grpc_unix_socket_interactive;
pub use grpc_server::run_grpc_unix_socket_until_shutdown;
pub use search::SearchArgs;
pub use search::SearchOutputFormat;
pub use utils::DEFAULT_UNIX_SOCKET_PATH;
pub use utils::effective_unix_socket_path;

/// Crate / binary version string embedded at compile time from `CARGO_PKG_VERSION`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

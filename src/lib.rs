#![doc = include_str!("../README.md")]

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use clap::Subcommand;

/// gRPC client helpers for talking to `racli server` over a Unix socket.
pub mod client;
/// `racli find-definition`: CLI arguments and formatting for LSP go-to-definition results.
pub mod find_definition;
/// Unix-socket gRPC server for `racli server`.
pub mod grpc_server;
/// Generic LSP client.
pub mod lsp_client;
/// Maps `lsp_types` values into racli protobuf shapes.
pub mod lsp_map;
/// MCP server over stdio (`rmcp`); tools forward to `racli server` gRPC.
pub mod mcp;
/// Protobuf and tonic-generated types for the Racli gRPC API.
pub mod proto;
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
mod workspace_file_watcher;

pub use grpc_server::GrpcServerError;
pub use grpc_server::run_grpc_unix_socket_interactive;
pub use grpc_server::run_grpc_unix_socket_until_shutdown;
pub use search::SearchArgs;
pub use search::SearchOutputFormat;

/// Crate / binary version string embedded at compile time from `CARGO_PKG_VERSION`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default Unix socket path for `racli server` when `RACLI_UNIX_SOCKET` is unset or empty.
pub const DEFAULT_UNIX_SOCKET_PATH: &str = "/tmp/racli.sock";

/// Returns the Unix socket path from `RACLI_UNIX_SOCKET`, or [`DEFAULT_UNIX_SOCKET_PATH`] if unset or empty.
pub fn effective_unix_socket_path() -> PathBuf {
    std::env::var_os("RACLI_UNIX_SOCKET")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_UNIX_SOCKET_PATH))
}

/// Top-level error returned by [`run`] for any server, listener, or MCP failure.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// gRPC server failed to bind, serve, or clean up the socket.
    #[error(transparent)]
    Grpc(#[from] GrpcServerError),
    /// MCP server failed during handler setup or on the stdio transport.
    #[error(transparent)]
    Mcp(#[from] mcp::ServerError),
}

/// Root CLI arguments: exactly one subcommand.
#[derive(Parser)]
#[command(name = "racli", version = VERSION)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

/// Subcommands for the `racli` binary (server, MCP, or version probe).
#[derive(Subcommand)]
enum Command {
    /// Start the gRPC server on the Unix socket.
    Server(ServerArgs),
    /// MCP stdio transport (`rmcp`); tools forward to [`Command::Server`] over the Unix gRPC socket.
    Mcp(ServerArgs),
    /// Print versions (client-side and, via gRPC, server-side).
    Version,
    /// Search workspace symbols via rust-analyzer (LSP `workspace/symbol`).
    Search(search::SearchArgs),
    /// Resolve the definition at a file position via rust-analyzer (LSP `textDocument/definition`).
    FindDefinition(find_definition::FindDefinitionArgs),
}

/// Arguments for `racli server` (`--port` is reserved).
#[derive(Parser)]
pub struct ServerArgs {
    /// Optional TCP port (not used for the current Unix-socket-only servers).
    #[arg(short, long)]
    pub port: Option<u16>,
}

pub async fn run() -> Result<(), RunError> {
    let args = Args::parse();

    match args.command {
        Command::Server(_opts) => {
            run_grpc_unix_socket_interactive(effective_unix_socket_path()).await?;
        }
        Command::Mcp(_opts) => {
            mcp::run_stdio().await?;
        }
        Command::Version => {
            let sock = effective_unix_socket_path();
            let sock_display = sock.display().to_string();
            match tokio::time::timeout(Duration::from_secs(10), client::get_version(&sock)).await {
                Ok(Ok(resp)) => {
                    println!("client: {VERSION}");
                    println!("server: {}", resp.version);
                    let lsp = resp.lsp_server_info.as_ref();
                    match lsp {
                        Some(info) if !info.name.is_empty() || !info.version.is_empty() => {
                            println!("{}: {}", info.name, info.version);
                        }
                        _ => {}
                    }
                }
                Ok(Err(err)) => {
                    eprintln!("racli server ({sock_display}): {err}");
                    println!("client: {VERSION}");
                }
                Err(_elapsed) => {
                    eprintln!(
                        "racli server ({sock_display}): connection timed out after 10 seconds"
                    );
                    println!("client: {VERSION}");
                }
            }
        }
        Command::Search(args) => search::run_cli_search(args).await,
        Command::FindDefinition(args) => find_definition::run_cli_find_definition(args).await,
    }

    Ok(())
}

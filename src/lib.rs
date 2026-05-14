//! racli binary library: async CLI entry, gRPC/MCP servers, and helpers used by integration tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use clap::Subcommand;

/// gRPC client helpers for talking to `racli server` over a Unix socket.
pub mod client;
/// Unix-socket gRPC server for `racli server`.
pub mod grpc_server;
/// MCP server over a Unix stream (`rmcp`).
pub mod mcp;
/// Protobuf and tonic-generated types for the Racli gRPC API.
pub mod proto;
/// `rust-analyzer` LSP child process used by `racli server`.
pub mod rust_analyzer;
/// Shared server logic and future service wiring.
pub mod server;
/// Socket abstractions and the generic accept loop used by MCP.
pub mod transport;

pub use grpc_server::{
    GrpcServerError, run_grpc_unix_socket_interactive, run_grpc_unix_socket_until_shutdown,
};

/// Crate / binary version string embedded at compile time from `CARGO_PKG_VERSION`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default filesystem path for the gRPC and MCP Unix sockets when subcommands omit an override.
pub const DEFAULT_UNIX_SOCKET_PATH: &str = "/tmp/racli.sock";

/// Top-level error returned by [`run`] for any server, listener, or MCP failure.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// gRPC server failed to bind, serve, or clean up the socket.
    #[error(transparent)]
    Grpc(#[from] GrpcServerError),
    /// MCP server failed during listen setup or handler initialization.
    #[error(transparent)]
    Mcp(#[from] mcp::ServerError),
    /// MCP generic listener failed to bind, accept, or handle signals.
    #[error(transparent)]
    Listen(#[from] transport::socket_server::ListenError),
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
    /// Start the MCP server on the Unix socket (`rmcp`).
    Mcp(ServerArgs),
    /// Print versions (client-side and, via gRPC, server-side).
    Version,
}

/// Arguments shared by `racli server` and `racli mcp` (reserved for future listen options).
#[derive(Parser)]
pub struct ServerArgs {
    /// Optional TCP port (not used for the current Unix-socket-only servers).
    #[arg(short, long)]
    pub port: Option<u16>,
}

/// Builds a Tokio [`SocketAddr`](crate::transport::socketwrapper::SocketAddr) for a filesystem Unix socket path.
fn unix_socket_addr_from_path(
    path: PathBuf,
) -> Result<transport::socketwrapper::SocketAddr, transport::socket_server::ListenError> {
    let std_socket_addr =
        std::os::unix::net::SocketAddr::from_pathname(&path).map_err(|source| {
            transport::socket_server::ListenError::Bind {
                path: path.clone(),
                source,
            }
        })?;
    let tokio_socket_addr = tokio::net::unix::SocketAddr::from(std_socket_addr);
    Ok(transport::socketwrapper::SocketAddr::Unix(
        tokio_socket_addr,
    ))
}

/// Parses CLI arguments and dispatches `server`, `mcp`, or `version` until the chosen task completes.
pub async fn run() -> Result<(), RunError> {
    let args = Args::parse();

    match args.command {
        Command::Server(_opts) => {
            run_grpc_unix_socket_interactive(PathBuf::from(DEFAULT_UNIX_SOCKET_PATH)).await?;
        }
        Command::Mcp(_opts) => {
            let socket_addr = unix_socket_addr_from_path(PathBuf::from(DEFAULT_UNIX_SOCKET_PATH))?;
            mcp::run(socket_addr).await?;
        }
        Command::Version => match tokio::time::timeout(
            Duration::from_secs(10),
            client::get_version(Path::new(DEFAULT_UNIX_SOCKET_PATH)),
        )
        .await
        {
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
                eprintln!("racli server ({DEFAULT_UNIX_SOCKET_PATH}): {err}");
                println!("client: {VERSION}");
            }
            Err(_elapsed) => {
                eprintln!(
                    "racli server ({DEFAULT_UNIX_SOCKET_PATH}): connection timed out after 10 seconds"
                );
                println!("client: {VERSION}");
            }
        },
    }

    Ok(())
}

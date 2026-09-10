//! CLI argument parsing and subcommand dispatch for the `racli` binary.

use std::time::Duration;

use clap::Parser;
use clap::Subcommand;

use crate::VERSION;
use crate::client;
use crate::effective_unix_socket_path;
use crate::find_definition;
use crate::grpc_server::GrpcServerError;
use crate::grpc_server::run_grpc_unix_socket_interactive;
use crate::logging;
use crate::mcp;
use crate::search;

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
    /// MCP stdio transport (`rmcp`); rust-analyzer and file watching run in-process (no Unix socket).
    Mcp,
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

/// The actual `racli` entrypoint
pub async fn run() -> Result<(), RunError> {
    let args = Args::parse();

    match args.command {
        Command::Server(_opts) => {
            run_grpc_unix_socket_interactive(effective_unix_socket_path()).await?;
        }
        Command::Mcp => {
            mcp::run_stdio().await?;
        }
        Command::Version => {
            logging::init_client_tracing();
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
        Command::Search(args) => {
            logging::init_client_tracing();
            search::run_cli_search(args).await
        }
        Command::FindDefinition(args) => {
            logging::init_client_tracing();
            find_definition::run_cli_find_definition(args).await
        }
    }

    Ok(())
}

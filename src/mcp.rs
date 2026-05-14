//! The `racli mcp` server (MCP via `rmcp` on a Unix socket).

use crate::transport::socket_server::{ListenError, Listener};
use crate::transport::socketwrapper::SocketAddr;

/// Errors from the MCP listen path or per-connection `rmcp` initialization.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Binding or accept loop failed on the MCP listen socket.
    #[error(transparent)]
    Listen(#[from] ListenError),
    /// `rmcp` could not complete the MCP handshake on an accepted stream.
    ///
    /// `ServerInitializeError` is very large on the stack; boxing keeps [`ServerError`] and
    /// [`crate::RunError`] small so `Result` values are not padded to that size.
    #[error("mcp initialization failed")]
    McpInit(Box<rmcp::service::ServerInitializeError>),
}

/// Boxes `rmcp`'s initialize error into [`ServerError::McpInit`].
impl From<rmcp::service::ServerInitializeError> for ServerError {
    fn from(value: rmcp::service::ServerInitializeError) -> Self {
        Self::McpInit(Box::new(value))
    }
}

/// Minimal MCP server handler (no custom tools yet); satisfies `rmcp::ServerHandler`.
#[derive(Clone, Copy, Debug)]
struct RacliMcp;

/// No-op MCP handler until custom tools or resources are added.
impl rmcp::ServerHandler for RacliMcp {}

/// Runs `rmcp` on `stream` until the session ends or initialization fails (errors go to stderr).
pub async fn serve_unix_stream(stream: tokio::net::UnixStream) {
    use rmcp::serve_server;

    let handler = RacliMcp;
    match serve_server(handler, stream).await {
        Ok(running) => {
            let _ = running.waiting().await;
        }
        Err(e) => {
            eprintln!("mcp init error: {}", ServerError::from(e));
        }
    }
}

/// Binds [`crate::transport::socket_server::Listener`] on `socket_addr` and serves MCP per accepted Unix stream.
pub async fn run(socket_addr: SocketAddr) -> Result<(), ServerError> {
    Listener::new(socket_addr, |stream| async move {
        serve_unix_stream(stream).await;
    })
    .run()
    .await?;

    Ok(())
}

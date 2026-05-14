//! Transport layer components

/// Tokio-oriented socket address wrapper (Unix today, IP reserved).
#[derive(Debug)]
pub enum SocketAddr {
    /// TCP/UDP-style endpoint (not yet wired into [`crate::transport::socket_server::Listener::run`]).
    #[allow(dead_code)] // Future IP listener binding.
    Ip(std::net::SocketAddr),
    /// Tokio Unix socket address, typically backed by a filesystem path.
    Unix(tokio::net::unix::SocketAddr),
}

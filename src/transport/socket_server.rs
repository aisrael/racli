//! Generic listener that accepts connections on a [`SocketAddr`] and runs a per-stream handler.
//! On Unix paths it stops on SIGINT or SIGTERM, drops the listener, and unlinks the pathname so restarts can re-bind.

use std::future::Future;
#[cfg(not(unix))]
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;

use super::socketwrapper::SocketAddr;

#[derive(Debug, Error)]
pub enum ListenError {
    /// `UnixListener::bind` failed for the requested pathname.
    #[error("failed to bind unix socket at {path}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Accepting the next client connection returned an I/O error.
    #[error("failed to accept unix connection")]
    Accept(#[source] std::io::Error),
    /// The bound address must be a filesystem path; abstract or unnamed sockets are rejected.
    #[error("unix socket address has no pathname (unnamed/abstract sockets are not supported)")]
    MissingPathname,
    /// Could not register SIGTERM (or related) for graceful shutdown.
    #[error("failed to install shutdown signal handler")]
    Signal(#[source] std::io::Error),
    /// Returned when this crate is built on targets without Unix sockets in the listener path.
    #[allow(dead_code)]
    #[error("unix sockets are not supported on this platform")]
    Unsupported,
}

/// Listens on [`SocketAddr`] and spawns `handler` for each accepted connection.
pub struct Listener<H> {
    socket_addr: SocketAddr,
    #[cfg(unix)]
    handler: Arc<H>,
    #[cfg(not(unix))]
    _handler: PhantomData<H>,
}

impl<H, Fut> Listener<H>
where
    H: Fn(tokio::net::UnixStream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    /// Wraps `handler` so each accepted [`UnixStream`](tokio::net::UnixStream) is processed concurrently.
    pub fn new(socket_addr: SocketAddr, handler: H) -> Self {
        Self {
            socket_addr,
            handler: Arc::new(handler),
        }
    }

    /// Binds the socket, accepts until a shutdown signal, then unlinks the path and returns.
    pub async fn run(self) -> Result<(), ListenError> {
        match self.socket_addr {
            SocketAddr::Unix(addr) => {
                #[cfg(unix)]
                {
                    Self::run_unix(addr, self.handler).await
                }
                #[cfg(not(unix))]
                {
                    Err(ListenError::Unsupported)
                }
            }
            SocketAddr::Ip(_) => {
                unimplemented!("IP listener not yet supported");
            }
        }
    }

    /// Unix-only accept loop: Ctrl+C or SIGTERM stops the server; each connection spawns `handler`.
    #[cfg(unix)]
    async fn run_unix(
        addr: tokio::net::unix::SocketAddr,
        handler: Arc<H>,
    ) -> Result<(), ListenError> {
        use tokio::signal::unix::{SignalKind, signal};

        let path = addr.as_pathname().ok_or(ListenError::MissingPathname)?;
        let path_buf = path.to_path_buf();

        let listener =
            tokio::net::UnixListener::bind(&path_buf).map_err(|source| ListenError::Bind {
                path: path_buf.clone(),
                source,
            })?;

        let mut sigterm = signal(SignalKind::terminate()).map_err(ListenError::Signal)?;

        let loop_result = loop {
            tokio::select! {
                biased;

                res = tokio::signal::ctrl_c() => {
                    res.map_err(ListenError::Signal)?;
                    break Ok(());
                }
                _ = sigterm.recv() => {
                    break Ok(());
                }
                res = listener.accept() => {
                    match res {
                        Ok((stream, _peer)) => {
                            let handler = Arc::clone(&handler);
                            tokio::spawn(async move { handler(stream).await });
                        }
                        Err(e) => break Err(ListenError::Accept(e)),
                    }
                }
            }
        };

        drop(listener);
        let _ = std::fs::remove_file(&path_buf);

        loop_result
    }
}

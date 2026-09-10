use std::path::PathBuf;

/// Default Unix socket path for `racli server` when `RACLI_UNIX_SOCKET` is unset or empty.
pub const DEFAULT_UNIX_SOCKET_PATH: &str = "/tmp/racli.sock";

/// Returns the Unix socket path from `RACLI_UNIX_SOCKET`, or [`DEFAULT_UNIX_SOCKET_PATH`] if unset or empty.
pub fn effective_unix_socket_path() -> PathBuf {
    std::env::var_os("RACLI_UNIX_SOCKET")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_UNIX_SOCKET_PATH))
}

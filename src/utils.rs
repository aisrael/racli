use std::path::Path;
use std::path::PathBuf;

/// Formats `err` and its `source()` chain as `"outer: cause: root cause"` for user-facing messages.
pub fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        // `#[error(transparent)]` wrappers repeat their inner message; don't print it twice.
        if !message.ends_with(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = cause.source();
    }
    message
}

/// Shared socket path used when per-project sockets are disabled (`RACLI_DERIVE_SOCKET_PATH=0`) and `RACLI_UNIX_SOCKET` is unset or empty.
pub const DEFAULT_UNIX_SOCKET_PATH: &str = "/tmp/racli.sock";

/// Returns the Unix socket path from `RACLI_UNIX_SOCKET`, or [`DEFAULT_UNIX_SOCKET_PATH`] if unset or empty.
pub fn effective_unix_socket_path() -> PathBuf {
    std::env::var_os("RACLI_UNIX_SOCKET")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_UNIX_SOCKET_PATH))
}

/// Name of the env var controlling per-project sockets: on by default (unset/empty, `1`, `true`);
/// `0` or `false` restores the single [`DEFAULT_UNIX_SOCKET_PATH`].
pub const RACLI_DERIVE_SOCKET_PATH_ENV: &str = "RACLI_DERIVE_SOCKET_PATH";

/// Resolves a [`RACLI_DERIVE_SOCKET_PATH_ENV`] value (case-insensitive): unset/empty/`1`/`true` is on,
/// `0`/`false` is off, and anything else is `None`.
fn parse_derive_flag(raw: Option<&str>) -> Option<bool> {
    match raw.map(|r| r.trim().to_ascii_lowercase()).as_deref() {
        None | Some("" | "1" | "true") => Some(true),
        Some("0" | "false") => Some(false),
        Some(_) => None,
    }
}

/// Returns whether [`RACLI_DERIVE_SOCKET_PATH_ENV`] is enabled (the default); an unrecognized value is treated as off with a warning.
pub fn derive_socket_path_enabled() -> bool {
    let raw = std::env::var_os(RACLI_DERIVE_SOCKET_PATH_ENV);
    let raw = raw.as_ref().map(|r| r.to_string_lossy());
    parse_derive_flag(raw.as_deref()).unwrap_or_else(|| {
        tracing::warn!(
            value = ?raw,
            "unrecognized {RACLI_DERIVE_SOCKET_PATH_ENV} value (expected 1/true/0/false); not deriving the socket path"
        );
        false
    })
}

/// Returns whether `RACLI_UNIX_SOCKET` is set to a non-empty path.
fn explicit_socket_path_set() -> bool {
    std::env::var_os("RACLI_UNIX_SOCKET").is_some_and(|s| !s.is_empty())
}

/// 64-bit FNV-1a; used instead of `DefaultHasher` because its output must stay stable across Rust releases.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Returns `/tmp/racli-<hash>.sock` for `dir` (canonicalized when possible), so each project gets its own socket.
pub fn derived_unix_socket_path(dir: &Path) -> PathBuf {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let hash = fnv1a_64(dir.as_os_str().as_encoded_bytes());
    PathBuf::from(format!("/tmp/racli-{hash:016x}.sock"))
}

/// Returns `RACLI_UNIX_SOCKET` if set; otherwise [`derived_unix_socket_path`] for `dir` when
/// [`RACLI_DERIVE_SOCKET_PATH_ENV`] is enabled; otherwise [`DEFAULT_UNIX_SOCKET_PATH`].
pub fn unix_socket_path_for_dir(dir: &Path) -> PathBuf {
    if !explicit_socket_path_set() && derive_socket_path_enabled() {
        derived_unix_socket_path(dir)
    } else {
        effective_unix_socket_path()
    }
}

/// Returns whether a server accepts connections on `path` (a stale socket file left by a crash does not count).
fn socket_is_live(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// Returns the derived socket of `start` or its nearest ancestor for which `is_live` holds.
fn find_live_socket(start: &Path, is_live: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    start
        .ancestors()
        .map(derived_unix_socket_path)
        .find(|path| is_live(path))
}

/// Socket for client commands: `RACLI_UNIX_SOCKET` if set, [`DEFAULT_UNIX_SOCKET_PATH`] if deriving is
/// off, else the live derived socket of the cwd or its nearest ancestor (so clients work from any
/// subdirectory of a served project). With no live server, warns and returns the cwd's derived path.
pub fn client_unix_socket_path() -> PathBuf {
    if explicit_socket_path_set() || !derive_socket_path_enabled() {
        return effective_unix_socket_path();
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => std::fs::canonicalize(&cwd).unwrap_or(cwd),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read current directory; using the default socket");
            return effective_unix_socket_path();
        }
    };
    find_live_socket(&cwd, socket_is_live).unwrap_or_else(|| {
        let socket = derived_unix_socket_path(&cwd);
        tracing::warn!(
            cwd = %cwd.display(),
            socket = %socket.display(),
            "no running racli server found for this directory or its parents; start `racli server` \
             or `racli tee` in the project root, or set RACLI_UNIX_SOCKET"
        );
        socket
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("root cause")]
    struct Root;

    #[derive(Debug, thiserror::Error)]
    enum Mid {
        #[error("middle")]
        Wrapped(#[source] Root),
        #[error(transparent)]
        Transparent(Root),
    }

    #[derive(Debug, thiserror::Error)]
    #[error("outer")]
    struct Outer(#[source] Mid);

    #[test]
    fn error_chain_joins_causes() {
        assert_eq!(
            error_chain(&Outer(Mid::Wrapped(Root))),
            "outer: middle: root cause"
        );
    }

    #[test]
    fn error_chain_skips_transparent_repeats() {
        assert_eq!(
            error_chain(&Outer(Mid::Transparent(Root))),
            "outer: root cause"
        );
        assert_eq!(error_chain(&Root), "root cause");
    }

    #[test]
    fn derive_flag_defaults_on() {
        for on in [
            None,
            Some(""),
            Some("  "),
            Some("1"),
            Some("true"),
            Some(" True "),
        ] {
            assert_eq!(parse_derive_flag(on), Some(true), "{on:?}");
        }
        for off in ["0", "false", "FALSE"] {
            assert_eq!(parse_derive_flag(Some(off)), Some(false), "{off:?}");
        }
        assert_eq!(parse_derive_flag(Some("yes")), None);
    }

    /// Paths under `/nonexistent` aren't canonicalized, so their derived sockets are predictable.
    fn live_at(dirs: &[&str]) -> impl Fn(&Path) -> bool {
        let live: Vec<PathBuf> = dirs
            .iter()
            .map(|d| derived_unix_socket_path(Path::new(d)))
            .collect();
        move |path| live.iter().any(|l| l == path)
    }

    #[test]
    fn finds_socket_of_start_dir() {
        let found = find_live_socket(
            Path::new("/nonexistent/proj"),
            live_at(&["/nonexistent/proj"]),
        );
        assert_eq!(
            found,
            Some(derived_unix_socket_path(Path::new("/nonexistent/proj")))
        );
    }

    #[test]
    fn finds_ancestor_socket_from_subdirectory() {
        let found = find_live_socket(
            Path::new("/nonexistent/proj/src/deep"),
            live_at(&["/nonexistent/proj"]),
        );
        assert_eq!(
            found,
            Some(derived_unix_socket_path(Path::new("/nonexistent/proj")))
        );
    }

    #[test]
    fn nearest_live_socket_wins() {
        let found = find_live_socket(
            Path::new("/nonexistent/ws/member/src"),
            live_at(&["/nonexistent/ws", "/nonexistent/ws/member"]),
        );
        assert_eq!(
            found,
            Some(derived_unix_socket_path(Path::new(
                "/nonexistent/ws/member"
            )))
        );
    }

    #[test]
    fn stale_nearer_socket_is_skipped() {
        // Only the parent is live; the member's (stale or absent) socket doesn't shadow it.
        let found = find_live_socket(
            Path::new("/nonexistent/ws/member"),
            live_at(&["/nonexistent/ws"]),
        );
        assert_eq!(
            found,
            Some(derived_unix_socket_path(Path::new("/nonexistent/ws")))
        );
    }

    #[test]
    fn no_live_socket_is_none() {
        assert_eq!(
            find_live_socket(Path::new("/nonexistent/proj"), live_at(&[])),
            None
        );
    }

    #[test]
    fn socket_liveness_requires_a_listener() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        assert!(!socket_is_live(&path), "missing socket");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(socket_is_live(&path), "listening socket");
        drop(listener);
        assert!(
            !socket_is_live(&path),
            "stale socket file after the listener is gone"
        );
    }

    #[test]
    fn derived_path_is_stable_and_per_directory() {
        let a = derived_unix_socket_path(Path::new("/nonexistent/project-a"));
        let b = derived_unix_socket_path(Path::new("/nonexistent/project-b"));
        assert_ne!(a, b);
        assert_eq!(
            a,
            derived_unix_socket_path(Path::new("/nonexistent/project-a"))
        );
        // Pinned so the path can't silently change between racli builds (clients must agree on it).
        assert_eq!(a, PathBuf::from("/tmp/racli-5ec43b340a7df10d.sock"));
    }

    #[test]
    fn derived_path_resolves_symlinks() {
        // e.g. macOS `/tmp` -> `/private/tmp`: both spellings of a project must share one socket.
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&project, &link).unwrap();
        assert_eq!(
            derived_unix_socket_path(&link),
            derived_unix_socket_path(&project)
        );
    }

    #[test]
    fn derived_path_has_fixed_short_shape() {
        // Unix socket paths are limited to 104 bytes on macOS; the hash keeps it fixed-length.
        let deep = PathBuf::from("/nonexistent").join("x".repeat(500));
        let path = derived_unix_socket_path(&deep);
        let name = path.to_str().unwrap();
        let hex = name
            .strip_prefix("/tmp/racli-")
            .and_then(|rest| rest.strip_suffix(".sock"))
            .unwrap_or_else(|| panic!("unexpected shape: {name}"));
        assert_eq!(hex.len(), 16);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(name.len(), "/tmp/racli-0000000000000000.sock".len());
    }

    #[test]
    fn fnv1a_64_matches_reference_vectors() {
        // Test vectors from the FNV reference implementation (64-bit FNV-1a).
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn derived_path_canonicalizes() {
        let dir = std::env::temp_dir();
        assert_eq!(
            derived_unix_socket_path(&dir.join(".")),
            derived_unix_socket_path(&dir)
        );
    }
}

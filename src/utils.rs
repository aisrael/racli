use std::path::Path;
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

/// Name of the env var that, when `1` or `true`, makes `racli tee` derive its socket path from its working directory.
pub const RACLI_DERIVE_SOCKET_PATH_ENV: &str = "RACLI_DERIVE_SOCKET_PATH";

/// Parses a boolean env flag: `1`/`true` and `0`/`false`/empty (case-insensitive); anything else is `None`.
fn parse_flag(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "" | "0" | "false" => Some(false),
        _ => None,
    }
}

/// Returns whether [`RACLI_DERIVE_SOCKET_PATH_ENV`] is enabled; an unrecognized value is treated as off with a warning.
pub fn derive_socket_path_enabled() -> bool {
    let Some(raw) = std::env::var_os(RACLI_DERIVE_SOCKET_PATH_ENV) else {
        return false;
    };
    let raw = raw.to_string_lossy();
    parse_flag(&raw).unwrap_or_else(|| {
        tracing::warn!(
            value = %raw,
            "unrecognized {RACLI_DERIVE_SOCKET_PATH_ENV} value (expected 1/true/0/false); not deriving the socket path"
        );
        false
    })
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
    let explicit = std::env::var_os("RACLI_UNIX_SOCKET").is_some_and(|s| !s.is_empty());
    if !explicit && derive_socket_path_enabled() {
        derived_unix_socket_path(dir)
    } else {
        effective_unix_socket_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flag_values() {
        for on in ["1", "true", "TRUE", " True "] {
            assert_eq!(parse_flag(on), Some(true), "{on:?}");
        }
        for off in ["", "0", "false", "False"] {
            assert_eq!(parse_flag(off), Some(false), "{off:?}");
        }
        assert_eq!(parse_flag("yes"), None);
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

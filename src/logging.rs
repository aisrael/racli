//! Shared log-level parsing for the client subcommands (`search`, `find-definition`, `version`).

use std::str::FromStr;

use tracing::level_filters::LevelFilter;

/// Name of the env var that sets the log level for the client subcommands (`search`, `find-definition`, `version`).
pub const RACLI_LOG_LEVEL_ENV: &str = "RACLI_LOG_LEVEL";

/// Parses `raw` as a digit `0`-`5` or a level name, case-insensitively; both are handled by
/// [`LevelFilter`]'s own `FromStr` (digits map to `OFF..=TRACE` in order).
fn parse_level(raw: &str) -> Option<LevelFilter> {
    LevelFilter::from_str(raw.trim()).ok()
}

/// Reads `env_var` and resolves it to a [`LevelFilter`]; unset/empty defaults to `INFO` silently,
/// an unparsable value defaults to `INFO` after printing a warning to stderr.
pub fn resolve_level(env_var: &str) -> LevelFilter {
    match std::env::var(env_var) {
        Ok(raw) if !raw.trim().is_empty() => parse_level(&raw).unwrap_or_else(|| {
            eprintln!("warning: invalid {env_var}={raw:?}, defaulting to info");
            LevelFilter::INFO
        }),
        _ => LevelFilter::INFO,
    }
}

/// Installs a stderr `tracing` subscriber for the client subcommands, level from [`RACLI_LOG_LEVEL_ENV`].
pub fn init_client_tracing() {
    let level = resolve_level(RACLI_LOG_LEVEL_ENV);
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_digits() {
        assert_eq!(parse_level("0"), Some(LevelFilter::OFF));
        assert_eq!(parse_level("1"), Some(LevelFilter::ERROR));
        assert_eq!(parse_level("2"), Some(LevelFilter::WARN));
        assert_eq!(parse_level("3"), Some(LevelFilter::INFO));
        assert_eq!(parse_level("4"), Some(LevelFilter::DEBUG));
        assert_eq!(parse_level("5"), Some(LevelFilter::TRACE));
        assert_eq!(parse_level("6"), None);
    }

    #[test]
    fn parses_words_case_insensitively() {
        assert_eq!(parse_level("DEBUG"), Some(LevelFilter::DEBUG));
        assert_eq!(parse_level("Trace"), Some(LevelFilter::TRACE));
        assert_eq!(parse_level("off"), Some(LevelFilter::OFF));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_level("nonsense"), None);
        assert_eq!(parse_level("6"), None);
    }

    #[test]
    fn resolve_level_defaults_to_info_when_unset() {
        // SAFETY: test-only, single-threaded within this process for this var.
        unsafe { std::env::remove_var("RACLI_LOG_LEVEL_TEST_UNSET") };
        assert_eq!(
            resolve_level("RACLI_LOG_LEVEL_TEST_UNSET"),
            LevelFilter::INFO
        );
    }
}

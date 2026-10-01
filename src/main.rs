//! The `racli` executable entry point.

use std::process::ExitCode;

/// Runs the async CLI via [`racli::run`] and prints a [`racli::RunError`] with its causes before exiting non-zero.
#[tokio::main]
async fn main() -> ExitCode {
    match racli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", racli::utils::error_chain(&e));
            ExitCode::FAILURE
        }
    }
}

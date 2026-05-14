//! The `racli` executable entry point.

/// Runs the async CLI via [`racli::run`] and surfaces [`racli::RunError`] to the process.
#[tokio::main]
async fn main() -> Result<(), racli::RunError> {
    racli::run().await
}

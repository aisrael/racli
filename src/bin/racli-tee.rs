//! The `racli-tee` executable: `racli tee` for editors whose server path takes no arguments.

use clap::CommandFactory;
use clap::FromArgMatches;
use racli::tee::TeeArgs;

/// Parses [`TeeArgs`] under the `racli-tee` name and runs `racli tee` until the editor session ends.
#[tokio::main]
async fn main() {
    let matches = TeeArgs::command()
        .name("racli-tee")
        .about("Serve gRPC on the Unix socket and LSP on stdio (for editors); same as `racli tee`.")
        .get_matches();
    let args = TeeArgs::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    racli::tee::run_tee_and_exit(args).await
}

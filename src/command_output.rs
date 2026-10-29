//! What a client command prints, collected so the CLI can print it and the MCP server can return
//! it (ADR 006).

use std::time::Duration;

/// A line a command prints on standard error.
#[derive(Debug)]
enum StderrLine {
    /// About the backend or a target; the CLI prints it after `racli <command> (<socket>): `.
    Backend(String),
    /// About the command's own output; the CLI prints it after `racli <command>: `.
    Command(String),
}

/// Why a command stopped before printing its results.
#[derive(Debug)]
pub enum CommandError {
    /// Arguments the command rejects; the CLI exits with status 2.
    Usage(String),
    /// A backend call failed or timed out; the CLI prints it after `racli <command> (<socket>): `.
    Failed(String),
}

/// What a command prints on standard output and standard error.
#[derive(Debug, Default)]
pub struct CommandOutput {
    stdout: String,
    stderr: Vec<StderrLine>,
}

impl CommandOutput {
    /// Appends `text` to standard output.
    pub fn print(&mut self, text: impl AsRef<str>) {
        self.stdout.push_str(text.as_ref());
    }

    /// Appends `line` and a newline to standard output.
    pub fn println(&mut self, line: impl AsRef<str>) {
        self.print(line);
        self.stdout.push('\n');
    }

    /// Adds a standard error line about the backend or a target.
    pub fn backend_error(&mut self, message: impl Into<String>) {
        self.stderr.push(StderrLine::Backend(message.into()));
    }

    /// Adds a standard error line about the command's own output.
    pub fn note(&mut self, message: impl Into<String>) {
        self.stderr.push(StderrLine::Command(message.into()));
    }

    /// Prints the output as `racli <command>` does, given the socket it used and how the command
    /// ended: standard error first, as the commands found it, then the results. A usage error
    /// exits with status 2.
    pub fn print_cli(&self, command: &str, socket: &str, result: Result<(), CommandError>) {
        if let Err(CommandError::Usage(message)) = &result {
            crate::symbol::exit_with_usage_error(message);
        }
        for line in &self.stderr {
            match line {
                StderrLine::Backend(message) => eprintln!("racli {command} ({socket}): {message}"),
                StderrLine::Command(message) => eprintln!("racli {command}: {message}"),
            }
        }
        if let Err(CommandError::Failed(message)) = &result {
            eprintln!("racli {command} ({socket}): {message}");
        }
        print!("{}", self.stdout);
    }
}

/// How long a command waits for each backend call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Awaits a backend call for up to 60 seconds, failing the command if it takes longer.
pub async fn within_timeout<T>(call: impl Future<Output = T>) -> Result<T, CommandError> {
    tokio::time::timeout(REQUEST_TIMEOUT, call)
        .await
        .map_err(|_elapsed| CommandError::Failed("request timed out after 60 seconds".into()))
}

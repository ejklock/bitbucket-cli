use bitbucket_cli::{cli, tui};

use anyhow::Result;
use clap::Parser;
use colored::Colorize;

use cli::{Cli, Commands};

/// Stack size of the worker thread that parses arguments and runs the command.
///
/// Debug builds of the derived command tree need more than the 1 MiB main
/// thread stack Windows gives a process, so the work runs on its own thread.
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

fn main() {
    let worker = std::thread::Builder::new()
        .name("bitbucket-main".into())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(run)
        .expect("failed to spawn the worker thread");

    match worker.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            report_error(&e);
            std::process::exit(1);
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Parse the command line, build the runtime and run the chosen subcommand.
fn run() -> Result<()> {
    let cli = Cli::parse();

    cli::set_workspace_override(cli.workspace.clone());
    cli::set_profile_override(cli.profile.clone());
    cli::set_output_format(cli.output);

    block_on_detached(dispatch(cli.command, cli.workspace))
}

/// Run `future` to completion on a current-thread runtime, then shut the
/// runtime down without waiting for blocking-pool work, which is abandoned.
///
/// A pending `tokio::io::stdin()` read cannot be cancelled, so waiting for it
/// would hold the process open until the next input line or EOF.
fn block_on_detached<F: std::future::Future<Output = Result<()>>>(future: F) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(future);
    runtime.shutdown_background();
    result
}

/// Run the chosen subcommand.
async fn dispatch(command: Commands, workspace: Option<String>) -> Result<()> {
    match command {
        Commands::Auth { command } => command.run().await,
        Commands::Repo { command } => command.run().await,
        Commands::Pr { command } => command.run().await,
        Commands::Issue { command } => command.run().await,
        Commands::Pipeline { command } => command.run().await,
        Commands::Workspace { command } => command.run().await,
        Commands::User { command } => command.run().await,
        Commands::Mcp => bitbucket_cli::mcp::serve().await,
        Commands::Tui => tui::run_tui(workspace).await,
    }
}

/// Print `error` to stderr and nothing else.
///
/// Error text can include remote-controlled strings (API messages, OAuth
/// callback parameters); control characters other than newline are stripped
/// so they cannot smuggle terminal escape sequences.
fn report_error(error: &anyhow::Error) {
    let message: String = error
        .to_string()
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect();
    eprintln!("{} {}", "Error:".red().bold(), message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sleeper() {
        drop(tokio::task::spawn_blocking(|| {
            std::thread::sleep(Duration::from_secs(10));
        }));
    }

    #[test]
    fn block_on_detached_returns_the_error_without_waiting_for_blocking_work() {
        let start = Instant::now();
        let result = block_on_detached(async {
            sleeper();
            Err(anyhow::anyhow!("boom"))
        });
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(result.unwrap_err().to_string(), "boom");
    }

    #[test]
    fn block_on_detached_returns_ok_without_waiting_for_blocking_work() {
        let start = Instant::now();
        let result = block_on_detached(async {
            sleeper();
            Ok(())
        });
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(result.is_ok());
    }

    #[test]
    fn block_on_detached_passes_results_through_unchanged() {
        assert!(block_on_detached(async { Ok(()) }).is_ok());
        let err = block_on_detached(async { Err(anyhow::anyhow!("plain failure")) });
        assert_eq!(err.unwrap_err().to_string(), "plain failure");
    }
}

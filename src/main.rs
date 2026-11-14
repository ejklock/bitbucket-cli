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

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(dispatch(cli.command, cli.workspace))
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

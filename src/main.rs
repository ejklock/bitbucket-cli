use bitbucket_cli::{cli, tui};

use anyhow::Result;
use clap::Parser;
use colored::Colorize;

use cli::{Cli, Commands};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    cli::set_workspace_override(cli.workspace.clone());
    cli::set_profile_override(cli.profile.clone());
    cli::set_output_format(cli.output);

    let result = dispatch(cli.command, cli.workspace).await;

    if let Err(e) = result {
        report_error(&e);
        std::process::exit(1);
    }

    Ok(())
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

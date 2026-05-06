use anyhow::Result;
use clap::Parser;

mod cli;
mod commands;
mod config;

use cli::{Cli, Command};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Run { name, project, prompt }) => {
            commands::run::run(name, project, prompt, cli.tier).await
        }
        Some(Command::Exec { name, cmd }) => commands::exec::run(name, cmd).await,
        Some(Command::Auth { tier }) => commands::auth::run(tier).await,
        Some(Command::List) => commands::list::run().await,
        Some(Command::Destroy { name, workspace }) => {
            commands::destroy::run(name, workspace).await
        }
        Some(Command::Build { tier, no_cache }) => commands::build::run(tier, no_cache).await,
        Some(Command::Tier { op }) => commands::tier::run(op).await,
        Some(Command::Cleanup) => commands::cleanup::run().await,
        Some(Command::SshConfig) => commands::ssh_config::run().await,
        Some(Command::Completions { shell }) => commands::completions::run(shell).await,
        None => {
            // Bare `cbox <name> [project]` — create-or-attach.
            let name = cli
                .name
                .expect("clap should have required `name` when no subcommand");
            commands::attach::run(
                name,
                cli.project,
                cli.tier,
                cli.branch,
                cli.shell,
                cli.claude,
                cli.attach,
            )
            .await
        }
    }
}

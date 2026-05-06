use anyhow::Result;
use clap::CommandFactory;
use clap_complete::generate;

use crate::cli::{Cli, CompletionShell};

pub async fn run(shell: CompletionShell) -> Result<()> {
    let mut cmd = Cli::command();
    let bin = cmd.get_name().to_string();
    generate(clap_complete::Shell::from(shell), &mut cmd, bin, &mut std::io::stdout());
    Ok(())
}

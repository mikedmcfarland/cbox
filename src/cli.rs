use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

#[derive(Parser, Debug)]
#[command(
    name = "cbox",
    version,
    about = "Run Claude Code sessions in isolated Docker containers",
    long_about = None,
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Session name. With no subcommand, creates or attaches to a session.
    #[arg(required = true)]
    pub name: Option<String>,

    /// Project name (from cbox.yaml `projects:`) or filesystem path.
    pub project: Option<String>,

    /// Override the default tier for this session.
    #[arg(long, global = true)]
    pub tier: Option<String>,

    /// Branch to check out in the session's workspace.
    #[arg(long)]
    pub branch: Option<String>,

    /// Open a shell instead of starting Claude on first invocation.
    #[arg(long, conflicts_with_all = ["claude", "attach"])]
    pub shell: bool,

    /// Start another Claude session on a subsequent invocation.
    #[arg(long, conflicts_with = "attach")]
    pub claude: bool,

    /// Reattach to the existing dtach session instead of opening a new connection.
    #[arg(long)]
    pub attach: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run an autonomous (headless) Claude session and detach.
    Run {
        /// Session name.
        name: String,
        /// Project name or filesystem path.
        project: Option<String>,
        /// Prompt to send Claude.
        prompt: String,
    },

    /// Run a one-off command in an existing session's workspace.
    Exec {
        /// Session name.
        name: String,
        /// Command and arguments.
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },

    /// One-time interactive setup for OAuth-based MCPs in a tier.
    Auth {
        /// Tier name.
        tier: String,
    },

    /// List all sessions and tier instances.
    List,

    /// Destroy a session (socket). Workspace persists by default.
    Destroy {
        /// Session name.
        name: String,
        /// Also remove the session's workspace.
        #[arg(long)]
        workspace: bool,
    },

    /// Build the tier image (or all tiers when omitted).
    Build {
        /// Tier name. Builds all tiers if omitted.
        tier: Option<String>,
        /// Disable Docker layer cache.
        #[arg(long)]
        no_cache: bool,
    },

    /// Tier-instance lifecycle operations.
    Tier {
        #[command(subcommand)]
        op: TierOp,
    },

    /// Stop tier instances that have no alive sessions.
    Cleanup,

    /// Update ~/.ssh/cbox_hosts with current tier endpoints.
    SshConfig,

    /// Print shell completion script.
    Completions {
        /// Target shell.
        shell: CompletionShell,
    },
}

#[derive(Subcommand, Debug)]
pub enum TierOp {
    /// Stop the tier instance entirely.
    Stop {
        /// Tier name.
        tier: String,
    },
    /// Pause the tier instance (preserves state).
    Pause {
        /// Tier name.
        tier: String,
    },
    /// Resume a paused tier instance.
    Resume {
        /// Tier name.
        tier: String,
    },
    /// List tier instances and their states.
    List,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    PowerShell,
    Zsh,
}

impl From<CompletionShell> for Shell {
    fn from(s: CompletionShell) -> Self {
        match s {
            CompletionShell::Bash => Shell::Bash,
            CompletionShell::Elvish => Shell::Elvish,
            CompletionShell::Fish => Shell::Fish,
            CompletionShell::PowerShell => Shell::PowerShell,
            CompletionShell::Zsh => Shell::Zsh,
        }
    }
}

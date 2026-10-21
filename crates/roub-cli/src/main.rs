//! `roub`: the command-line client. Short-lived; talks only to the local roubd daemon.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "roub",
    version,
    about = "Server-centric version control client"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check out a ref into the current directory.
    Checkout {
        /// Ref to check out.
        #[arg(default_value = "main")]
        reference: String,
        /// Directories to materialize; the whole tree when omitted.
        #[arg(long = "sparse")]
        sparse: Vec<String>,
    },
    /// Show files changed since the checked-out changeset.
    Status,
    /// Create a changeset from the changed files and move the current ref.
    Commit {
        /// Changeset message.
        #[arg(short, long)]
        message: String,
    },
    /// Move the working copy to the ref's current changeset.
    Update,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Checkout { .. } => "checkout",
        Command::Status => "status",
        Command::Commit { .. } => "commit",
        Command::Update => "update",
    };
    anyhow::bail!("`roub {name}` is not implemented yet")
}

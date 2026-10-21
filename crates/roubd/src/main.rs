//! `roubd`: the node daemon. Keeps one object store for every job on the node, runs the file
//! watcher and talks only to roub-cache in its region.

use clap::Parser;

#[derive(Parser)]
#[command(name = "roubd", version, about = "roub node daemon")]
struct Args {
    /// roub-cache endpoint for this region.
    #[arg(long, env = "ROUB_UPSTREAM", default_value = "http://127.0.0.1:7471")]
    upstream: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    anyhow::bail!("roubd is not implemented yet (upstream {})", args.upstream)
}

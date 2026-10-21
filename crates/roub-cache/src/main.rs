//! `roub-cache`: regional tree and blob cache. Answers the same ObjectService RPCs as
//! roub-server, asks upstream only on a miss and passes RepositoryService calls through.

use clap::Parser;

#[derive(Parser)]
#[command(name = "roub-cache", version, about = "roub regional cache")]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "ROUB_LISTEN", default_value = "0.0.0.0:7471")]
    listen: String,
    /// roub-server endpoint.
    #[arg(long, env = "ROUB_UPSTREAM", default_value = "http://127.0.0.1:7470")]
    upstream: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    anyhow::bail!(
        "roub-cache is not implemented yet (listen {}, upstream {})",
        args.listen,
        args.upstream
    )
}

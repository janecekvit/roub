//! `roub-server`: the authoritative tier.

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use roub_core::RepositoryName;
use roub_proto::v1::object_service_server::ObjectServiceServer;
use roub_proto::v1::repository_service_server::RepositoryServiceServer;
use roub_server::{MemoryStore, RoubService};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "roub-server", version, about = "Authoritative roub server")]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "ROUB_LISTEN", default_value = "0.0.0.0:7470")]
    listen: SocketAddr,

    /// Repository to host; repeat the flag or separate names with commas.
    #[arg(
        long = "repository",
        env = "ROUB_REPOSITORIES",
        value_delimiter = ',',
        required = true
    )]
    repositories: Vec<RepositoryName>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args = Args::parse();
    tracing::info!(listen = %args.listen, repositories = ?args.repositories, "roub-server starting");
    tracing::warn!(
        "storage is in memory and there is no authentication yet: listen only on a trusted network"
    );

    let service = RoubService::new(Arc::new(MemoryStore::new(args.repositories)));
    tonic::transport::Server::builder()
        .add_service(ObjectServiceServer::new(service.clone()))
        .add_service(RepositoryServiceServer::new(service))
        .serve_with_shutdown(args.listen, async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("roub-server shutting down");
        })
        .await?;
    Ok(())
}

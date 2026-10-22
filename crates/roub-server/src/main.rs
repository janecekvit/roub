//! `roub-server`: the authoritative tier.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use roub_core::RepositoryName;
use roub_proto::v1::object_service_server::ObjectServiceServer;
use roub_proto::v1::repository_service_server::RepositoryServiceServer;
use roub_server::{MemoryStore, PostgresStore, RoubService, Store};
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

    /// PostgreSQL connection string. Without it everything is kept in memory. Prefer the
    /// environment variable: command lines are visible to other users of the machine.
    #[arg(
        long,
        env = "ROUB_DATABASE_URL",
        hide_env_values = true,
        requires = "blob_dir"
    )]
    database_url: Option<String>,

    /// Directory for blob contents, used with `--database-url`.
    #[arg(long, env = "ROUB_BLOB_DIR", requires = "database_url")]
    blob_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args = Args::parse();
    tracing::info!(listen = %args.listen, repositories = ?args.repositories, "roub-server starting");
    tracing::warn!("there is no authentication yet: listen only on a trusted network");

    match (args.database_url, args.blob_dir) {
        (Some(url), Some(blob_dir)) => {
            // The connection string may hold a password, so it never reaches a log or an error.
            let config = url.parse().map_err(|_| {
                anyhow::anyhow!("ROUB_DATABASE_URL is not a valid connection string")
            })?;
            let store = PostgresStore::connect(config, &blob_dir, args.repositories)
                .await
                .context("connecting to PostgreSQL")?;
            tracing::info!(blob_dir = %blob_dir.display(), "storage: PostgreSQL and local blobs");
            serve(store, args.listen).await
        }
        _ => {
            tracing::warn!("storage is in memory: everything is lost when the server stops");
            serve(MemoryStore::new(args.repositories), args.listen).await
        }
    }
}

async fn serve<S: Store>(store: S, listen: SocketAddr) -> anyhow::Result<()> {
    let service = RoubService::new(Arc::new(store));
    tonic::transport::Server::builder()
        .add_service(ObjectServiceServer::new(service.clone()))
        .add_service(RepositoryServiceServer::new(service))
        .serve_with_shutdown(listen, async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("roub-server shutting down");
        })
        .await?;
    Ok(())
}

//! `roub-server`: the authoritative tier. Stateless; all long-lived state lives in PostgreSQL
//! and the blob store. Every RPC answers UNIMPLEMENTED until the storage layer lands.

use std::net::SocketAddr;
use std::pin::Pin;

use clap::Parser;
use roub_proto::v1::object_service_server::{ObjectService, ObjectServiceServer};
use roub_proto::v1::repository_service_server::{RepositoryService, RepositoryServiceServer};
use roub_proto::v1::{
    CompareAndSwapRefRequest, CompareAndSwapRefResponse, CreateChangesetRequest,
    CreateChangesetResponse, GetObjectsRequest, Object, PutBlobsRequest, PutBlobsResponse,
    ResolveRefRequest, ResolveRefResponse,
};
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "roub-server", version, about = "Authoritative roub server")]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "ROUB_LISTEN", default_value = "0.0.0.0:7470")]
    listen: SocketAddr,
}

#[derive(Default)]
struct Server;

type ObjectStream = Pin<Box<dyn Stream<Item = Result<Object, Status>> + Send>>;

#[tonic::async_trait]
impl ObjectService for Server {
    type GetObjectsStream = ObjectStream;

    async fn get_objects(
        &self,
        _request: Request<GetObjectsRequest>,
    ) -> Result<Response<Self::GetObjectsStream>, Status> {
        Err(Status::unimplemented("GetObjects is not implemented yet"))
    }
}

#[tonic::async_trait]
impl RepositoryService for Server {
    async fn resolve_ref(
        &self,
        _request: Request<ResolveRefRequest>,
    ) -> Result<Response<ResolveRefResponse>, Status> {
        Err(Status::unimplemented("ResolveRef is not implemented yet"))
    }

    async fn put_blobs(
        &self,
        _request: Request<Streaming<PutBlobsRequest>>,
    ) -> Result<Response<PutBlobsResponse>, Status> {
        Err(Status::unimplemented("PutBlobs is not implemented yet"))
    }

    async fn create_changeset(
        &self,
        _request: Request<CreateChangesetRequest>,
    ) -> Result<Response<CreateChangesetResponse>, Status> {
        Err(Status::unimplemented(
            "CreateChangeset is not implemented yet",
        ))
    }

    async fn compare_and_swap_ref(
        &self,
        _request: Request<CompareAndSwapRefRequest>,
    ) -> Result<Response<CompareAndSwapRefResponse>, Status> {
        Err(Status::unimplemented(
            "CompareAndSwapRef is not implemented yet",
        ))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args = Args::parse();
    tracing::info!(listen = %args.listen, "roub-server starting");

    tonic::transport::Server::builder()
        .add_service(ObjectServiceServer::new(Server))
        .add_service(RepositoryServiceServer::new(Server))
        .serve_with_shutdown(args.listen, async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("roub-server shutting down");
        })
        .await?;
    Ok(())
}

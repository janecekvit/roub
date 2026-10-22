//! Test harness shared by the integration tests: a server on a local port over either store.
//!
//! PostgreSQL tests run when `ROUB_TEST_DATABASE_URL` is set and are skipped otherwise, unless
//! `ROUB_REQUIRE_POSTGRES` is also set (as in CI), where a missing database fails the test. Each
//! test gets its own schema and blob directory, so tests can run in parallel on one database.

// Each test binary uses a different part of the harness.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use roub_core::RepositoryName;
use roub_proto::v1::object_service_client::ObjectServiceClient;
use roub_proto::v1::object_service_server::ObjectServiceServer;
use roub_proto::v1::repository_service_client::RepositoryServiceClient;
use roub_proto::v1::repository_service_server::RepositoryServiceServer;
use roub_server::{MemoryStore, PostgresStore, RoubService, Store};
use tokio::net::TcpListener;
use tokio_postgres::NoTls;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Channel;

pub const REPO: &str = "monorepo";

#[derive(Clone)]
pub struct Client {
    pub objects: ObjectServiceClient<Channel>,
    pub repos: RepositoryServiceClient<Channel>,
}

impl Client {
    /// Serves `store` on a free local port and connects to it.
    pub async fn start(store: impl Store) -> Self {
        let service = RoubService::new(Arc::new(store));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(ObjectServiceServer::new(service.clone()))
                .add_service(RepositoryServiceServer::new(service))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        let channel = Channel::from_shared(format!("http://{address}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        Self {
            objects: ObjectServiceClient::new(channel.clone()),
            repos: RepositoryServiceClient::new(channel),
        }
    }
}

pub fn repository() -> RepositoryName {
    RepositoryName::new(REPO).unwrap()
}

pub fn memory_store() -> MemoryStore {
    MemoryStore::new([repository()])
}

/// A fresh, empty PostgreSQL schema and blob directory.
pub struct PostgresTarget {
    pub config: tokio_postgres::Config,
    pub blob_dir: PathBuf,
}

impl PostgresTarget {
    /// Connects a store, as a server does at start-up.
    pub async fn connect(&self) -> PostgresStore {
        PostgresStore::connect(self.config.clone(), &self.blob_dir, [repository()])
            .await
            .unwrap()
    }

    /// A plain connection to the test schema, for checks below the `Store` API.
    pub async fn sql(&self) -> tokio_postgres::Client {
        let (client, connection) = self.config.connect(NoTls).await.unwrap();
        tokio::spawn(connection);
        client
    }
}

/// Returns `None` (the test is skipped) if no test database is configured.
pub async fn postgres_target() -> Option<PostgresTarget> {
    let Ok(url) = std::env::var("ROUB_TEST_DATABASE_URL") else {
        assert!(
            std::env::var_os("ROUB_REQUIRE_POSTGRES").is_none(),
            "ROUB_REQUIRE_POSTGRES is set, but ROUB_TEST_DATABASE_URL is not"
        );
        eprintln!("skipped: set ROUB_TEST_DATABASE_URL to run this test against PostgreSQL");
        return None;
    };
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let schema = format!(
        "roub_test_{}_{nanos}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );

    let mut config: tokio_postgres::Config = url.parse().expect("ROUB_TEST_DATABASE_URL is valid");
    let target = PostgresTarget {
        config: config.clone(),
        blob_dir: std::env::temp_dir().join(&schema),
    };
    target
        .sql()
        .await
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    config.options(format!("-c search_path={schema}"));
    Some(PostgresTarget { config, ..target })
}

/// Generates one test per store for each named `async fn(Client)`.
#[allow(unused_macros)]
macro_rules! store_tests {
    ($($name:ident),* $(,)?) => {
        mod memory {
            $(
                #[tokio::test(flavor = "multi_thread")]
                async fn $name() {
                    let client = crate::common::Client::start(crate::common::memory_store()).await;
                    super::$name(client).await;
                }
            )*
        }

        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread")]
                async fn $name() {
                    let Some(target) = crate::common::postgres_target().await else {
                        return;
                    };
                    let client = crate::common::Client::start(target.connect().await).await;
                    super::$name(client).await;
                }
            )*
        }
    };
}

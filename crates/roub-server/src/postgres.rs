//! The production `Store`: metadata in PostgreSQL, blob contents in a `LocalBlobStore`.
//!
//! The schema lives in `migrations/`; `PostgresStore::connect` applies what is missing under an
//! advisory lock, so several servers can start at once. Every statement is scoped by the
//! repository's numeric id, resolved once at start-up.

use std::collections::HashMap;
use std::path::PathBuf;

use deadpool_postgres::{GenericClient, Manager, ManagerConfig, Pool, RecyclingMethod};
use roub_core::{ObjectId, ObjectKind, RefName, RepositoryName};
use tokio_postgres::NoTls;

use crate::blob::LocalBlobStore;
use crate::store::{NewChangeset, RefState, Store, StoreError, StoredObject, SwapOutcome};

/// Schema migrations in order. Applied ones are recorded in `schema_migrations` and never edited.
const MIGRATIONS: &[(i32, &str)] = &[(1, include_str!("../migrations/0001_initial.sql"))];

/// Key of the advisory lock that serializes migrations across servers.
const MIGRATION_LOCK: i64 = 0x726f_7562; // "roub"

/// Most connections one server opens.
const MAX_CONNECTIONS: usize = 16;

/// PostgreSQL for metadata, trees and changesets; a blob store for blob contents.
pub struct PostgresStore {
    pool: Pool,
    blobs: LocalBlobStore,
    repositories: HashMap<RepositoryName, i64>,
}

fn backend(error: impl std::error::Error + Send + Sync + 'static) -> StoreError {
    StoreError::Backend(Box::new(error))
}

impl PostgresStore {
    /// Connects, applies missing migrations and makes sure every repository exists.
    pub async fn connect(
        config: tokio_postgres::Config,
        blob_root: impl Into<PathBuf>,
        repositories: impl IntoIterator<Item = RepositoryName>,
    ) -> Result<Self, StoreError> {
        let manager = Manager::from_config(
            config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(manager)
            .max_size(MAX_CONNECTIONS)
            .build()
            .map_err(backend)?;
        let blobs = LocalBlobStore::new(blob_root).map_err(backend)?;

        let mut client = pool.get().await.map_err(backend)?;
        migrate(&mut client).await?;
        let mut ids = HashMap::new();
        for name in repositories {
            let row = client
                .query_one(
                    "WITH inserted AS (
                         INSERT INTO repositories (name) VALUES ($1)
                         ON CONFLICT (name) DO NOTHING RETURNING id
                     )
                     SELECT id FROM inserted
                     UNION ALL SELECT id FROM repositories WHERE name = $1",
                    &[&name.as_str()],
                )
                .await
                .map_err(backend)?;
            ids.insert(name, row.get(0));
        }
        drop(client);
        Ok(Self {
            pool,
            blobs,
            repositories: ids,
        })
    }

    fn repository_id(&self, repository: &RepositoryName) -> Result<i64, StoreError> {
        self.repositories
            .get(repository)
            .copied()
            .ok_or_else(|| StoreError::UnknownRepository(repository.clone()))
    }

    async fn client(&self) -> Result<deadpool_postgres::Object, StoreError> {
        self.pool.get().await.map_err(backend)
    }

    async fn ref_state(
        client: &impl GenericClient,
        repository: i64,
        name: &RefName,
    ) -> Result<Option<RefState>, StoreError> {
        let row = client
            .query_opt(
                "SELECT target, version FROM refs WHERE repository_id = $1 AND name = $2",
                &[&repository, &name.as_str()],
            )
            .await
            .map_err(backend)?;
        row.map(|row| {
            Ok(RefState {
                target: optional_id(row.get(0))?,
                version: unsigned(row.get(1))?,
            })
        })
        .transpose()
    }
}

/// Applies every migration the database does not have yet, in one transaction.
async fn migrate(client: &mut deadpool_postgres::Object) -> Result<(), StoreError> {
    let transaction = client.transaction().await.map_err(backend)?;
    transaction
        .execute("SELECT pg_advisory_xact_lock($1)", &[&MIGRATION_LOCK])
        .await
        .map_err(backend)?;
    transaction
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    INTEGER     PRIMARY KEY,
                 applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
             )",
        )
        .await
        .map_err(backend)?;
    let applied: Vec<i32> = transaction
        .query("SELECT version FROM schema_migrations", &[])
        .await
        .map_err(backend)?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let known = MIGRATIONS.last().map_or(0, |(version, _)| *version);
    if let Some(newer) = applied.iter().find(|version| **version > known) {
        return Err(StoreError::Backend(
            format!("the database has schema version {newer}; this server knows up to {known}")
                .into(),
        ));
    }
    for (version, sql) in MIGRATIONS {
        if applied.contains(version) {
            continue;
        }
        transaction.batch_execute(sql).await.map_err(backend)?;
        transaction
            .execute(
                "INSERT INTO schema_migrations (version) VALUES ($1)",
                &[version],
            )
            .await
            .map_err(backend)?;
        tracing::info!(version, "applied schema migration");
    }
    transaction.commit().await.map_err(backend)
}

/// Reads an id column. The schema guarantees 32 bytes, so anything else means the database was
/// changed outside roub.
fn id(bytes: &[u8]) -> Result<ObjectId, StoreError> {
    ObjectId::from_slice(bytes).map_err(backend)
}

fn optional_id(bytes: Option<&[u8]>) -> Result<Option<ObjectId>, StoreError> {
    bytes.map(id).transpose()
}

/// Reads a version or size column; the schema keeps both non-negative.
fn unsigned(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(backend)
}

impl Store for PostgresStore {
    async fn object(
        &self,
        repository: &RepositoryName,
        id: &ObjectId,
    ) -> Result<Option<StoredObject>, StoreError> {
        let repository = self.repository_id(repository)?;
        let client = self.client().await?;
        let row = client
            .query_opt(
                "SELECT 'tree', content FROM trees WHERE repository_id = $1 AND id = $2
                 UNION ALL
                 SELECT 'changeset', content FROM changesets WHERE repository_id = $1 AND id = $2
                 UNION ALL
                 SELECT 'blob', NULL FROM blobs WHERE repository_id = $1 AND id = $2",
                &[&repository, &id.as_bytes().as_slice()],
            )
            .await
            .map_err(backend)?;
        drop(client);
        let Some(row) = row else {
            return Ok(None);
        };
        let kind = match row.get::<_, &str>(0) {
            "tree" => ObjectKind::Tree,
            "changeset" => ObjectKind::Changeset,
            _ => {
                let content = self
                    .blobs
                    .get(repository, *id)
                    .await
                    .map_err(backend)?
                    .ok_or_else(|| {
                        StoreError::Backend(
                            format!("blob {id} is indexed but missing from the blob store").into(),
                        )
                    })?;
                return Ok(Some(StoredObject {
                    kind: ObjectKind::Blob,
                    content,
                }));
            }
        };
        Ok(Some(StoredObject {
            kind,
            content: row.get(1),
        }))
    }

    async fn put_blob(
        &self,
        repository: &RepositoryName,
        id: ObjectId,
        content: Vec<u8>,
    ) -> Result<bool, StoreError> {
        let repository = self.repository_id(repository)?;
        if self.blob_size_by_id(repository, &id).await?.is_some() {
            return Ok(false);
        }
        let size = i64::try_from(content.len()).map_err(backend)?;
        // Content first: an index row always means the content is durable.
        self.blobs
            .put(repository, id, content)
            .await
            .map_err(backend)?;
        let inserted = self
            .client()
            .await?
            .execute(
                "INSERT INTO blobs (repository_id, id, size) VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
                &[&repository, &id.as_bytes().as_slice(), &size],
            )
            .await
            .map_err(backend)?;
        Ok(inserted == 1)
    }

    async fn blob_size(
        &self,
        repository: &RepositoryName,
        id: &ObjectId,
    ) -> Result<Option<u64>, StoreError> {
        let repository = self.repository_id(repository)?;
        self.blob_size_by_id(repository, id).await
    }

    async fn root_tree(
        &self,
        repository: &RepositoryName,
        changeset: &ObjectId,
    ) -> Result<Option<ObjectId>, StoreError> {
        let repository = self.repository_id(repository)?;
        let row = self
            .client()
            .await?
            .query_opt(
                "SELECT root_tree FROM changesets WHERE repository_id = $1 AND id = $2",
                &[&repository, &changeset.as_bytes().as_slice()],
            )
            .await
            .map_err(backend)?;
        row.map(|row| id(row.get(0))).transpose()
    }

    async fn append_changeset(
        &self,
        repository: &RepositoryName,
        changeset: NewChangeset,
    ) -> Result<(), StoreError> {
        let repository = self.repository_id(repository)?;
        let mut client = self.client().await?;
        let transaction = client.transaction().await.map_err(backend)?;
        let tree_ids: Vec<&[u8]> = changeset
            .trees
            .iter()
            .map(|(id, _)| id.as_bytes().as_slice())
            .collect();
        let tree_contents: Vec<&[u8]> = changeset
            .trees
            .iter()
            .map(|(_, content)| content.as_slice())
            .collect();
        transaction
            .execute(
                "INSERT INTO trees (repository_id, id, content)
                 SELECT $1, id, content FROM unnest($2::bytea[], $3::bytea[]) AS t (id, content)
                 ON CONFLICT DO NOTHING",
                &[&repository, &tree_ids, &tree_contents],
            )
            .await
            .map_err(backend)?;
        transaction
            .execute(
                "INSERT INTO changesets (repository_id, id, content, root_tree)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT DO NOTHING",
                &[
                    &repository,
                    &changeset.id.as_bytes().as_slice(),
                    &changeset.content,
                    &changeset.root_tree.as_bytes().as_slice(),
                ],
            )
            .await
            .map_err(backend)?;
        transaction.commit().await.map_err(backend)
    }

    async fn resolve_ref(
        &self,
        repository: &RepositoryName,
        name: &RefName,
    ) -> Result<Option<RefState>, StoreError> {
        let repository = self.repository_id(repository)?;
        let client = self.client().await?;
        Self::ref_state(&client, repository, name).await
    }

    async fn compare_and_swap_ref(
        &self,
        repository: &RepositoryName,
        name: &RefName,
        expected: Option<ObjectId>,
        desired: Option<ObjectId>,
    ) -> Result<SwapOutcome, StoreError> {
        let repository = self.repository_id(repository)?;
        let expected_bytes = expected.as_ref().map(|id| id.as_bytes().as_slice());
        let desired_bytes = desired.as_ref().map(|id| id.as_bytes().as_slice());
        let mut client = self.client().await?;

        if expected == desired {
            // Nothing moves, so there is nothing to write or log.
            let current = Self::ref_state(&client, repository, name)
                .await?
                .unwrap_or_default();
            return Ok(SwapOutcome {
                swapped: current.target == expected,
                current,
            });
        }

        let transaction = client.transaction().await.map_err(backend)?;
        // One statement per case, so the comparison and the write happen under the row lock.
        // Under READ COMMITTED a concurrent winner makes the WHERE clause fail, not succeed twice.
        let moved = match expected_bytes {
            None => {
                transaction
                    .query_opt(
                        "INSERT INTO refs (repository_id, name, target, version)
                         VALUES ($1, $2, $3, 1)
                         ON CONFLICT (repository_id, name) DO UPDATE
                             SET target = EXCLUDED.target, version = refs.version + 1
                             WHERE refs.target IS NULL
                         RETURNING version",
                        &[&repository, &name.as_str(), &desired_bytes],
                    )
                    .await
            }
            Some(expected_bytes) => {
                transaction
                    .query_opt(
                        "UPDATE refs SET target = $3, version = version + 1
                         WHERE repository_id = $1 AND name = $2 AND target = $4
                         RETURNING version",
                        &[&repository, &name.as_str(), &desired_bytes, &expected_bytes],
                    )
                    .await
            }
        }
        .map_err(backend)?;

        let Some(row) = moved else {
            transaction.rollback().await.map_err(backend)?;
            let current = Self::ref_state(&client, repository, name)
                .await?
                .unwrap_or_default();
            return Ok(SwapOutcome {
                swapped: false,
                current,
            });
        };
        let new_version: i64 = row.get(0);
        transaction
            .execute(
                "INSERT INTO ref_log (repository_id, name, version, old_target, new_target)
                 VALUES ($1, $2, $3, $4, $5)",
                &[
                    &repository,
                    &name.as_str(),
                    &new_version,
                    &expected_bytes,
                    &desired_bytes,
                ],
            )
            .await
            .map_err(backend)?;
        transaction.commit().await.map_err(backend)?;
        Ok(SwapOutcome {
            swapped: true,
            current: RefState {
                target: desired,
                version: unsigned(new_version)?,
            },
        })
    }
}

impl PostgresStore {
    async fn blob_size_by_id(
        &self,
        repository: i64,
        id: &ObjectId,
    ) -> Result<Option<u64>, StoreError> {
        let row = self
            .client()
            .await?
            .query_opt(
                "SELECT size FROM blobs WHERE repository_id = $1 AND id = $2",
                &[&repository, &id.as_bytes().as_slice()],
            )
            .await
            .map_err(backend)?;
        row.map(|row| unsigned(row.get(0))).transpose()
    }
}

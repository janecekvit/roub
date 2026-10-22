//! The gRPC handlers. They validate every request, enforce the changeset rules that need stored
//! state (parents and blobs exist, sizes match, copy sources exist, the tree derivation applies),
//! and leave persistence to a `Store`.

use std::pin::Pin;
use std::sync::Arc;

use roub_core::{
    Changeset, DecodeError, DeriveError, DerivedTrees, FileChange, FileOp, ObjectId, ObjectKind,
    RepositoryName, Tree, TreeSource, derive_trees, lookup,
};
use roub_proto::v1 as proto;
use roub_proto::v1::object_service_server::ObjectService;
use roub_proto::v1::repository_service_server::RepositoryService;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::convert;
use crate::store::{NewChangeset, Store, StoreError};

/// Most ids one `GetObjects` call may name.
pub const MAX_IDS_PER_REQUEST: usize = 1024;

/// How many objects `GetObjects` reads ahead of a slow client.
const STREAM_BUFFER: usize = 16;

/// Serves `ObjectService` and `RepositoryService` from a `Store`.
pub struct RoubService<S> {
    store: Arc<S>,
}

impl<S> Clone for RoubService<S> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
        }
    }
}

impl<S: Store> RoubService<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    async fn root_tree(
        &self,
        repository: &RepositoryName,
        changeset: &ObjectId,
    ) -> Result<ObjectId, Status> {
        self.store
            .root_tree(repository, changeset)
            .await?
            .ok_or_else(|| {
                Status::failed_precondition(format!("changeset {changeset} is not stored"))
            })
    }

    /// Checks every put's blob: it must be stored, with the size the change claims.
    async fn check_blobs(
        &self,
        repository: &RepositoryName,
        changes: &[FileChange],
    ) -> Result<(), Status> {
        for change in changes {
            let FileOp::Put { blob, size, .. } = &change.op else {
                continue;
            };
            match self.store.blob_size(repository, blob).await? {
                Some(stored) if stored == *size => {}
                Some(stored) => {
                    return Err(Status::invalid_argument(format!(
                        "'{}': size {size} does not match blob {blob}, which has {stored} bytes",
                        change.path
                    )));
                }
                None => {
                    return Err(Status::failed_precondition(format!(
                        "'{}': blob {blob} is not stored; upload it with PutBlobs first",
                        change.path
                    )));
                }
            }
        }
        Ok(())
    }
}

impl From<StoreError> for Status {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::UnknownRepository(_) => Status::not_found(error.to_string()),
            StoreError::Backend(_) => {
                // The client gets no backend details: they can name hosts or credentials.
                tracing::error!(%error, "storage backend failed");
                Status::internal("storage backend failed")
            }
        }
    }
}

type ObjectStream = Pin<Box<dyn Stream<Item = Result<proto::Object, Status>> + Send>>;

#[tonic::async_trait]
impl<S: Store> ObjectService for RoubService<S> {
    type GetObjectsStream = ObjectStream;

    async fn get_objects(
        &self,
        request: Request<proto::GetObjectsRequest>,
    ) -> Result<Response<Self::GetObjectsStream>, Status> {
        let request = request.into_inner();
        let repository = convert::repository(&request.repository)?;
        if request.ids.len() > MAX_IDS_PER_REQUEST {
            return Err(Status::invalid_argument(format!(
                "{} ids requested, at most {MAX_IDS_PER_REQUEST} per call",
                request.ids.len()
            )));
        }
        let ids = request
            .ids
            .iter()
            .enumerate()
            .map(|(index, id)| convert::object_id(Some(id), &format!("ids[{index}]")))
            .collect::<Result<Vec<_>, _>>()?;

        let (sender, receiver) = mpsc::channel(STREAM_BUFFER);
        let store = Arc::clone(&self.store);
        tokio::spawn(async move {
            for id in ids {
                let item = match store.object(&repository, &id).await {
                    Ok(Some(object)) => Ok(proto::Object {
                        id: Some(convert::proto_id(&id)),
                        kind: convert::proto_kind(object.kind).into(),
                        compression: proto::Compression::None.into(),
                        content: object.content,
                    }),
                    Ok(None) => Err(Status::not_found(format!("object {id} is not stored"))),
                    Err(error) => Err(error.into()),
                };
                let last = item.is_err();
                // A failed send means the client went away.
                if sender.send(item).await.is_err() || last {
                    break;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

#[tonic::async_trait]
impl<S: Store> RepositoryService for RoubService<S> {
    async fn resolve_ref(
        &self,
        request: Request<proto::ResolveRefRequest>,
    ) -> Result<Response<proto::ResolveRefResponse>, Status> {
        let request = request.into_inner();
        let repository = convert::repository(&request.repository)?;
        let name = convert::ref_name(&request.ref_name)?;
        let state = self.store.resolve_ref(&repository, &name).await?;
        let Some((changeset, version)) = state.and_then(|s| Some((s.target?, s.version))) else {
            return Err(Status::not_found(format!("ref '{name}' does not exist")));
        };
        let root_tree = self
            .store
            .root_tree(&repository, &changeset)
            .await?
            .ok_or_else(|| {
                tracing::error!(%repository, %name, %changeset, "ref points at a missing changeset");
                Status::internal(format!("ref '{name}' points at a missing changeset"))
            })?;
        Ok(Response::new(proto::ResolveRefResponse {
            changeset: Some(convert::proto_id(&changeset)),
            version,
            root_tree: Some(convert::proto_id(&root_tree)),
        }))
    }

    async fn put_blobs(
        &self,
        request: Request<Streaming<proto::PutBlobsRequest>>,
    ) -> Result<Response<proto::PutBlobsResponse>, Status> {
        let mut stream = request.into_inner();
        let mut repository: Option<RepositoryName> = None;
        let mut response = proto::PutBlobsResponse::default();
        while let Some(message) = stream.message().await? {
            let name = convert::repository(&message.repository)?;
            match &repository {
                Some(first) if *first != name => {
                    return Err(Status::invalid_argument(format!(
                        "one PutBlobs stream names repositories '{first}' and '{name}'"
                    )));
                }
                Some(_) => {}
                None => repository = Some(name.clone()),
            }
            let blob = message
                .blob
                .ok_or_else(|| Status::invalid_argument("blob is missing"))?;
            let id = convert::object_id(blob.id.as_ref(), "blob.id")?;
            if blob.kind() != proto::ObjectKind::Blob {
                return Err(Status::invalid_argument(format!(
                    "object {id} is not a blob"
                )));
            }
            if blob.compression() != proto::Compression::None {
                return Err(Status::unimplemented(format!(
                    "blob {id}: only uncompressed blobs are accepted for now"
                )));
            }
            let computed = ObjectId::compute(ObjectKind::Blob, &blob.content);
            if computed != id {
                return Err(Status::invalid_argument(format!(
                    "blob {id}: the content hashes to {computed}"
                )));
            }
            if self.store.put_blob(&name, id, blob.content).await? {
                response.stored += 1;
            } else {
                response.already_present += 1;
            }
        }
        Ok(Response::new(response))
    }

    async fn create_changeset(
        &self,
        request: Request<proto::CreateChangesetRequest>,
    ) -> Result<Response<proto::CreateChangesetResponse>, Status> {
        let request = request.into_inner();
        let repository = convert::repository(&request.repository)?;
        let changeset = Changeset::new(convert::changeset_fields(request)?)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let id = changeset.id();
        let respond = |root_tree: ObjectId| {
            Response::new(proto::CreateChangesetResponse {
                changeset: Some(convert::proto_id(&id)),
                root_tree: Some(convert::proto_id(&root_tree)),
            })
        };

        // A retry of a request that already succeeded.
        if let Some(root_tree) = self.store.root_tree(&repository, &id).await? {
            return Ok(respond(root_tree));
        }

        let mut parent_roots = Vec::with_capacity(changeset.parents().len());
        for parent in changeset.parents() {
            parent_roots.push(self.root_tree(&repository, parent).await?);
        }
        self.check_blobs(&repository, changeset.changes()).await?;

        // Tree derivation is synchronous; it runs on a blocking thread that reads trees from the
        // store through the runtime.
        let trees = BlockingTrees {
            store: Arc::clone(&self.store),
            repository: repository.clone(),
            runtime: Handle::current(),
        };
        let changeset = Arc::new(changeset);
        let derived = {
            let changeset = Arc::clone(&changeset);
            tokio::task::spawn_blocking(move || derive(&trees, &changeset, &parent_roots))
                .await
                .map_err(|e| Status::internal(format!("tree derivation failed: {e}")))??
        };

        let root_tree = derived.root;
        self.store
            .append_changeset(
                &repository,
                NewChangeset {
                    id,
                    content: changeset.encode(),
                    root_tree,
                    trees: derived
                        .new_trees
                        .iter()
                        .map(|tree| (tree.id(), tree.encode()))
                        .collect(),
                },
            )
            .await?;
        tracing::info!(%repository, changeset = %id, %root_tree, "changeset created");
        Ok(respond(root_tree))
    }

    async fn compare_and_swap_ref(
        &self,
        request: Request<proto::CompareAndSwapRefRequest>,
    ) -> Result<Response<proto::CompareAndSwapRefResponse>, Status> {
        let request = request.into_inner();
        let repository = convert::repository(&request.repository)?;
        let name = convert::ref_name(&request.ref_name)?;
        let expected = convert::optional_object_id(request.expected.as_ref(), "expected")?;
        let desired = convert::optional_object_id(request.desired.as_ref(), "desired")?;
        if let Some(desired) = &desired {
            self.root_tree(&repository, desired).await?;
        }
        let outcome = self
            .store
            .compare_and_swap_ref(&repository, &name, expected, desired)
            .await?;
        if outcome.swapped {
            tracing::info!(
                %repository,
                %name,
                version = outcome.current.version,
                target = ?outcome.current.target,
                "ref moved"
            );
        }
        Ok(Response::new(proto::CompareAndSwapRefResponse {
            swapped: outcome.swapped,
            current: outcome.current.target.as_ref().map(convert::proto_id),
            version: outcome.current.version,
        }))
    }
}

/// Checks the copy sources and derives the trees. Runs on a blocking thread.
fn derive<S: Store>(
    trees: &BlockingTrees<S>,
    changeset: &Changeset,
    parent_roots: &[ObjectId],
) -> Result<DerivedTrees, Status> {
    for change in changeset.changes() {
        if let FileOp::Put {
            copy_from: Some(copy),
            ..
        } = &change.op
        {
            let source =
                lookup(trees, &parent_roots[copy.parent], &copy.path).map_err(derive_status)?;
            if !source.is_some_and(|entry| entry.mode.is_file()) {
                return Err(Status::failed_precondition(format!(
                    "'{}' is copied from '{}', which is not a file in parent {}",
                    change.path, copy.path, copy.parent
                )));
            }
        }
    }
    derive_trees(trees, parent_roots.first(), changeset.changes()).map_err(derive_status)
}

fn derive_status(error: DeriveError) -> Status {
    match error {
        // The parent's trees were stored by an earlier commit, so failing to read one is a
        // server fault, not the caller's.
        DeriveError::Source { .. } | DeriveError::InvalidTree(..) => {
            tracing::error!(%error, "reading stored trees failed");
            Status::internal("reading stored trees failed")
        }
        _ => Status::failed_precondition(error.to_string()),
    }
}

/// Reads trees from a `Store` for the synchronous derivation in `roub-core`.
struct BlockingTrees<S> {
    store: Arc<S>,
    repository: RepositoryName,
    runtime: Handle,
}

#[derive(Debug, thiserror::Error)]
enum TreeReadError {
    #[error("tree {0} is not stored")]
    Missing(ObjectId),
    #[error("object {0} is not a tree")]
    NotATree(ObjectId),
    #[error("tree {0}: {1}")]
    Decode(ObjectId, DecodeError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl<S: Store> TreeSource for BlockingTrees<S> {
    type Error = TreeReadError;

    fn tree(&self, id: &ObjectId) -> Result<Tree, TreeReadError> {
        let object = self
            .runtime
            .block_on(self.store.object(&self.repository, id))?
            .ok_or(TreeReadError::Missing(*id))?;
        if object.kind != ObjectKind::Tree {
            return Err(TreeReadError::NotATree(*id));
        }
        Tree::decode(&object.content).map_err(|e| TreeReadError::Decode(*id, e))
    }
}

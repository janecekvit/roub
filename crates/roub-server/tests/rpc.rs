//! End-to-end tests: a real gRPC server on a local port, an in-memory store, and the generated
//! clients, as roubd and roub-cache will use them.

use std::sync::Arc;

use roub_core::{Changeset, ObjectId, ObjectKind, RepositoryName, Tree};
use roub_proto::v1 as proto;
use roub_proto::v1::object_service_client::ObjectServiceClient;
use roub_proto::v1::object_service_server::ObjectServiceServer;
use roub_proto::v1::repository_service_client::RepositoryServiceClient;
use roub_proto::v1::repository_service_server::RepositoryServiceServer;
use roub_server::{MemoryStore, RoubService};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::Code;
use tonic::transport::Channel;

const REPO: &str = "monorepo";

struct Client {
    objects: ObjectServiceClient<Channel>,
    repos: RepositoryServiceClient<Channel>,
}

async fn start() -> Client {
    let store = MemoryStore::new([RepositoryName::new(REPO).unwrap()]);
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
    Client {
        objects: ObjectServiceClient::new(channel.clone()),
        repos: RepositoryServiceClient::new(channel),
    }
}

fn id(id: ObjectId) -> proto::ObjectId {
    proto::ObjectId {
        digest: id.as_bytes().to_vec(),
    }
}

fn blob_id(content: &[u8]) -> ObjectId {
    ObjectId::compute(ObjectKind::Blob, content)
}

fn core_id(id: Option<proto::ObjectId>) -> ObjectId {
    ObjectId::from_slice(&id.expect("id is set").digest).unwrap()
}

fn blob(content: &[u8]) -> proto::PutBlobsRequest {
    proto::PutBlobsRequest {
        repository: REPO.into(),
        blob: Some(proto::Object {
            id: Some(id(blob_id(content))),
            kind: proto::ObjectKind::Blob.into(),
            compression: proto::Compression::None.into(),
            content: content.to_vec(),
        }),
    }
}

fn put(path: &str, content: &[u8]) -> proto::FileChange {
    proto::FileChange {
        path: path.into(),
        change: Some(proto::file_change::Change::Put(proto::FileContent {
            blob: Some(id(blob_id(content))),
            mode: proto::FileMode::Regular.into(),
            size: content.len() as u64,
            copy_from: None,
        })),
    }
}

fn delete(path: &str) -> proto::FileChange {
    proto::FileChange {
        path: path.into(),
        change: Some(proto::file_change::Change::Delete(proto::Deleted {})),
    }
}

/// A changeset by the author of the golden vectors in `docs/design/object-format.md`.
fn changeset(
    parents: &[ObjectId],
    changes: Vec<proto::FileChange>,
) -> proto::CreateChangesetRequest {
    proto::CreateChangesetRequest {
        repository: REPO.into(),
        changes,
        message: "Add a file\n".into(),
        parents: parents.iter().copied().map(id).collect(),
        author: Some(proto::Signature {
            name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            time: 1_792_000_000,
            tz_offset_minutes: 120,
        }),
        committer: None,
    }
}

impl Client {
    async fn put_blobs(&mut self, blobs: Vec<proto::PutBlobsRequest>) -> proto::PutBlobsResponse {
        self.repos
            .put_blobs(tokio_stream::iter(blobs))
            .await
            .unwrap()
            .into_inner()
    }

    async fn create(
        &mut self,
        request: proto::CreateChangesetRequest,
    ) -> Result<(ObjectId, ObjectId), tonic::Status> {
        let response = self.repos.create_changeset(request).await?.into_inner();
        Ok((core_id(response.changeset), core_id(response.root_tree)))
    }

    async fn get(&mut self, ids: &[ObjectId]) -> Result<Vec<proto::Object>, tonic::Status> {
        let request = proto::GetObjectsRequest {
            repository: REPO.into(),
            ids: ids.iter().copied().map(id).collect(),
        };
        let mut stream = self.objects.get_objects(request).await?.into_inner();
        let mut objects = Vec::new();
        while let Some(object) = stream.message().await? {
            objects.push(object);
        }
        Ok(objects)
    }

    async fn tree(&mut self, tree: ObjectId) -> Tree {
        let objects = self.get(&[tree]).await.unwrap();
        assert_eq!(objects[0].kind(), proto::ObjectKind::Tree);
        Tree::decode(&objects[0].content).unwrap()
    }

    async fn swap(
        &mut self,
        ref_name: &str,
        expected: Option<ObjectId>,
        desired: Option<ObjectId>,
    ) -> Result<proto::CompareAndSwapRefResponse, tonic::Status> {
        let request = proto::CompareAndSwapRefRequest {
            repository: REPO.into(),
            ref_name: ref_name.into(),
            expected: expected.map(id),
            desired: desired.map(id),
        };
        Ok(self.repos.compare_and_swap_ref(request).await?.into_inner())
    }

    async fn resolve(
        &mut self,
        ref_name: &str,
    ) -> Result<proto::ResolveRefResponse, tonic::Status> {
        let request = proto::ResolveRefRequest {
            repository: REPO.into(),
            ref_name: ref_name.into(),
        };
        Ok(self.repos.resolve_ref(request).await?.into_inner())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn commit_matches_the_golden_vectors() {
    let mut client = start().await;
    let stored = client.put_blobs(vec![blob(b"hello")]).await;
    assert_eq!((stored.stored, stored.already_present), (1, 0));

    let (golden, root) = client
        .create(changeset(&[], vec![put("a", b"hello")]))
        .await
        .unwrap();
    // The ids pinned in docs/design/object-format.md.
    assert_eq!(
        golden.to_string(),
        "2d219349af1bfe55bfd16510324b54309d9eff291a69dcbbf14fb1dd8d525475"
    );
    assert_eq!(
        root.to_string(),
        "ae0bf66b4d1833721f77a88cff499e19879014d36faaf3fc8b9c6ad11ea40ee3"
    );

    // A retry returns the same ids.
    let again = client
        .create(changeset(&[], vec![put("a", b"hello")]))
        .await
        .unwrap();
    assert_eq!(again, (golden, root));

    // Every object is served by id, in the order asked, and verifies against it.
    let objects = client
        .get(&[golden, root, blob_id(b"hello")])
        .await
        .unwrap();
    let kinds: Vec<_> = objects.iter().map(|o| o.kind()).collect();
    assert_eq!(
        kinds,
        [
            proto::ObjectKind::Changeset,
            proto::ObjectKind::Tree,
            proto::ObjectKind::Blob
        ]
    );
    assert_eq!(Changeset::decode(&objects[0].content).unwrap().id(), golden);
    assert_eq!(Tree::decode(&objects[1].content).unwrap().id(), root);
    assert_eq!(objects[2].content, b"hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn second_commit_derives_from_its_parent() {
    let mut client = start().await;
    client
        .put_blobs(vec![blob(b"1"), blob(b"22"), blob(b"333")])
        .await;
    let (first, _) = client
        .create(changeset(
            &[],
            vec![put("src/a.rs", b"1"), put("README", b"22")],
        ))
        .await
        .unwrap();

    let mut rename = put("src/b.rs", b"1");
    if let Some(proto::file_change::Change::Put(content)) = &mut rename.change {
        content.copy_from = Some(proto::CopyFrom {
            parent: 0,
            path: "src/a.rs".into(),
        });
    }
    let (_, root) = client
        .create(changeset(
            &[first],
            vec![rename, delete("src/a.rs"), put("README", b"333")],
        ))
        .await
        .unwrap();

    let top = client.tree(root).await;
    let names: Vec<_> = top
        .entries()
        .iter()
        .map(|e| (e.name.as_str(), e.size))
        .collect();
    assert_eq!(names, [("README", 3), ("src", 0)]);
    let src = client.tree(top.get("src").unwrap().id).await;
    let names: Vec<_> = src.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["b.rs"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_changesets_that_do_not_apply() {
    let mut client = start().await;
    client.put_blobs(vec![blob(b"x")]).await;
    let (first, _) = client
        .create(changeset(&[], vec![put("a", b"x")]))
        .await
        .unwrap();

    let code = |result: Result<(ObjectId, ObjectId), tonic::Status>| result.unwrap_err().code();

    // The blob was never uploaded.
    assert_eq!(
        code(
            client
                .create(changeset(&[first], vec![put("b", b"missing")]))
                .await
        ),
        Code::FailedPrecondition
    );
    // The size does not match the stored blob.
    let mut wrong_size = put("b", b"x");
    if let Some(proto::file_change::Change::Put(content)) = &mut wrong_size.change {
        content.size = 2;
    }
    assert_eq!(
        code(client.create(changeset(&[first], vec![wrong_size])).await),
        Code::InvalidArgument
    );
    // The parent is not stored.
    let unknown = ObjectId::from_bytes([9; ObjectId::LEN]);
    assert_eq!(
        code(
            client
                .create(changeset(&[unknown], vec![put("b", b"x")]))
                .await
        ),
        Code::FailedPrecondition
    );
    // Deleting a file the parent does not have.
    assert_eq!(
        code(
            client
                .create(changeset(&[first], vec![delete("nope")]))
                .await
        ),
        Code::FailedPrecondition
    );
    // A case collision with an existing file.
    assert_eq!(
        code(
            client
                .create(changeset(&[first], vec![put("A", b"x")]))
                .await
        ),
        Code::FailedPrecondition
    );
    // A copy from a path the parent does not have.
    let mut bad_copy = put("b", b"x");
    if let Some(proto::file_change::Change::Put(content)) = &mut bad_copy.change {
        content.copy_from = Some(proto::CopyFrom {
            parent: 0,
            path: "nope".into(),
        });
    }
    assert_eq!(
        code(client.create(changeset(&[first], vec![bad_copy])).await),
        Code::FailedPrecondition
    );
    // A path Windows cannot check out.
    assert_eq!(
        code(
            client
                .create(changeset(&[first], vec![put("aux.txt", b"x")]))
                .await
        ),
        Code::InvalidArgument
    );
    // The same path twice.
    assert_eq!(
        code(
            client
                .create(changeset(&[first], vec![put("b", b"x"), delete("b")]))
                .await
        ),
        Code::InvalidArgument
    );
    // An unknown repository.
    let mut elsewhere = changeset(&[], vec![put("a", b"x")]);
    elsewhere.repository = "elsewhere".into();
    assert_eq!(code(client.create(elsewhere).await), Code::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn put_blobs_verifies_ids() {
    let mut client = start().await;
    let stored = client
        .put_blobs(vec![blob(b"a"), blob(b"a"), blob(b"b")])
        .await;
    assert_eq!((stored.stored, stored.already_present), (2, 1));

    let mut forged = blob(b"evil");
    forged.blob.as_mut().unwrap().id = Some(id(blob_id(b"good")));
    let error = client
        .repos
        .put_blobs(tokio_stream::iter(vec![forged]))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    // Nothing was stored under the forged id.
    assert_eq!(
        client.get(&[blob_id(b"good")]).await.unwrap_err().code(),
        Code::NotFound
    );

    let mut compressed = blob(b"z");
    compressed.blob.as_mut().unwrap().compression = proto::Compression::Zstd.into();
    let error = client
        .repos
        .put_blobs(tokio_stream::iter(vec![compressed]))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unimplemented);

    let mut other_repo = blob(b"c");
    other_repo.repository = "other".into();
    let error = client
        .repos
        .put_blobs(tokio_stream::iter(vec![blob(b"c"), other_repo]))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
}

#[tokio::test(flavor = "multi_thread")]
async fn get_objects_stops_at_the_first_missing_object() {
    let mut client = start().await;
    client.put_blobs(vec![blob(b"a")]).await;
    let request = proto::GetObjectsRequest {
        repository: REPO.into(),
        ids: vec![
            id(blob_id(b"a")),
            id(blob_id(b"missing")),
            id(blob_id(b"a")),
        ],
    };
    let mut stream = client
        .objects
        .get_objects(request)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(stream.message().await.unwrap().unwrap().content, b"a");
    assert_eq!(stream.message().await.unwrap_err().code(), Code::NotFound);

    let too_many = proto::GetObjectsRequest {
        repository: REPO.into(),
        ids: vec![id(blob_id(b"a")); roub_server::service::MAX_IDS_PER_REQUEST + 1],
    };
    let error = client.objects.get_objects(too_many).await.unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
}

#[tokio::test(flavor = "multi_thread")]
async fn refs_move_by_compare_and_swap() {
    let mut client = start().await;
    client.put_blobs(vec![blob(b"1"), blob(b"2")]).await;
    let (first, first_root) = client
        .create(changeset(&[], vec![put("f", b"1")]))
        .await
        .unwrap();
    let (second, _) = client
        .create(changeset(&[first], vec![put("f", b"2")]))
        .await
        .unwrap();

    assert_eq!(
        client.resolve("main").await.unwrap_err().code(),
        Code::NotFound
    );

    // Create.
    let created = client.swap("main", None, Some(first)).await.unwrap();
    assert!(created.swapped);
    assert_eq!(created.version, 1);
    let resolved = client.resolve("main").await.unwrap();
    assert_eq!(core_id(resolved.changeset), first);
    assert_eq!(core_id(resolved.root_tree), first_root);
    assert_eq!(resolved.version, 1);

    // Creating again loses: the ref exists.
    let lost = client.swap("main", None, Some(second)).await.unwrap();
    assert!(!lost.swapped);
    assert_eq!(core_id(lost.current), first);

    // Move.
    let moved = client
        .swap("main", Some(first), Some(second))
        .await
        .unwrap();
    assert!(moved.swapped);
    assert_eq!(moved.version, 2);

    // A stale expectation loses and learns the winner.
    let stale = client.swap("main", Some(first), Some(first)).await.unwrap();
    assert!(!stale.swapped);
    assert_eq!(core_id(stale.current), second);
    assert_eq!(stale.version, 2);

    // Delete, then create again: the version keeps counting.
    let deleted = client.swap("main", Some(second), None).await.unwrap();
    assert!(deleted.swapped && deleted.current.is_none());
    assert_eq!(
        client.resolve("main").await.unwrap_err().code(),
        Code::NotFound
    );
    let recreated = client.swap("main", None, Some(first)).await.unwrap();
    assert_eq!(recreated.version, 4);

    // A ref can only point at a stored changeset.
    let unknown = ObjectId::from_bytes([9; ObjectId::LEN]);
    let error = client
        .swap("main", Some(first), Some(unknown))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    let error = client
        .swap("bad name", None, Some(first))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
}

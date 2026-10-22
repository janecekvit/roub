//! PostgreSQL-specific guarantees: state survives a restart, history cannot be rewritten below
//! the API, and a server refuses a schema it does not know. Skipped without
//! `ROUB_TEST_DATABASE_URL`; see `common/mod.rs`.

mod common;

use roub_core::{ObjectId, ObjectKind, RefName};
use roub_server::{PostgresStore, Store, StoreError};

use common::{postgres_target, repository};

fn blob_id(content: &[u8]) -> ObjectId {
    ObjectId::compute(ObjectKind::Blob, content)
}

/// Stores a blob, a tree and a changeset through the `Store` API and points `main` at it.
async fn seed(store: &PostgresStore) -> (ObjectId, ObjectId) {
    let blob = blob_id(b"hello");
    store
        .put_blob(&repository(), blob, b"hello".to_vec())
        .await
        .unwrap();
    // A tree holding the blob; the changeset id and bytes are placeholders, the store does not
    // decode them.
    let tree = roub_core::Tree::new(vec![roub_core::TreeEntry {
        name: "a".into(),
        mode: roub_core::EntryMode::Regular,
        id: blob,
        size: 5,
    }])
    .unwrap();
    let changeset = ObjectId::from_bytes([7; ObjectId::LEN]);
    store
        .append_changeset(
            &repository(),
            roub_server::store::NewChangeset {
                id: changeset,
                content: vec![1, 2, 3],
                root_tree: tree.id(),
                trees: vec![(tree.id(), tree.encode())],
            },
        )
        .await
        .unwrap();
    let main = RefName::new("main").unwrap();
    let outcome = store
        .compare_and_swap_ref(&repository(), &main, None, Some(changeset))
        .await
        .unwrap();
    assert!(outcome.swapped);
    (changeset, tree.id())
}

#[tokio::test(flavor = "multi_thread")]
async fn state_survives_a_restart() {
    let Some(target) = postgres_target().await else {
        return;
    };
    let (changeset, root) = seed(&target.connect().await).await;

    // A second start applies no migration twice and sees everything the first one stored.
    let store = target.connect().await;
    let main = RefName::new("main").unwrap();
    let state = store
        .resolve_ref(&repository(), &main)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((state.target, state.version), (Some(changeset), 1));
    assert_eq!(
        store.root_tree(&repository(), &changeset).await.unwrap(),
        Some(root)
    );
    let blob = store
        .object(&repository(), &blob_id(b"hello"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (blob.kind, blob.content.as_slice()),
        (ObjectKind::Blob, &b"hello"[..])
    );

    let migrations: i64 = target
        .sql()
        .await
        .query_one("SELECT count(*) FROM schema_migrations", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(migrations, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn history_cannot_be_rewritten_below_the_api() {
    let Some(target) = postgres_target().await else {
        return;
    };
    seed(&target.connect().await).await;
    let sql = target.sql().await;
    for statement in [
        "UPDATE ref_log SET new_target = NULL",
        "DELETE FROM ref_log",
        "TRUNCATE ref_log",
        "UPDATE changesets SET content = '\\x00'",
        "DELETE FROM trees",
        "TRUNCATE blobs CASCADE",
        "DELETE FROM refs",
        // Versions only grow, one step at a time.
        "UPDATE refs SET version = 1",
        "UPDATE refs SET version = version + 2",
        "UPDATE refs SET name = 'other'",
    ] {
        let error = sql.batch_execute(statement).await.unwrap_err();
        // Rejected by the schema's triggers, not by a foreign key or by chance.
        let message = error.as_db_error().map(|e| e.message().to_owned());
        assert!(
            message.as_deref().is_some_and(|m| m.contains("immutable")
                || m.contains("must move from version")
                || m.contains("cannot be renamed")),
            "{statement}: {error:?}"
        );
    }
    // Ids are always 32 bytes.
    let error = sql
        .batch_execute("INSERT INTO trees (repository_id, id, content) SELECT id, '\\x01', '' FROM repositories")
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION),
        "{error:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_newer_schema() {
    let Some(target) = postgres_target().await else {
        return;
    };
    target.connect().await;
    target
        .sql()
        .await
        .batch_execute("INSERT INTO schema_migrations (version) VALUES (9999)")
        .await
        .unwrap();
    let error = PostgresStore::connect(target.config.clone(), &target.blob_dir, [repository()])
        .await
        .err()
        .expect("a newer schema is refused");
    assert!(error.to_string().contains("9999"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_blobs_are_errors() {
    let Some(target) = postgres_target().await else {
        return;
    };
    let store = target.connect().await;
    seed(&store).await;
    // Overwrite the only blob file with other content.
    let mut stack = vec![target.blob_dir.clone()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            stack.extend(std::fs::read_dir(path).unwrap().map(|e| e.unwrap().path()));
        } else {
            std::fs::write(path, b"jello").unwrap();
        }
    }
    let error = store
        .object(&repository(), &blob_id(b"hello"))
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::Backend(_)), "{error}");
}

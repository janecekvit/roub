//! An in-memory `Store` for tests and local experiments. Everything is lost when the process
//! exits.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use roub_core::{ObjectId, ObjectKind, RefName, RepositoryName};

use crate::store::{
    NewChangeset, RefLogEntry, RefState, Store, StoreError, StoredObject, SwapOutcome,
};

/// Hosts a fixed set of repositories in memory.
pub struct MemoryStore {
    repositories: HashMap<RepositoryName, Mutex<Repository>>,
}

#[derive(Default)]
struct Repository {
    objects: HashMap<ObjectId, StoredObject>,
    /// Changeset id to root tree id: the derived data the server keeps per changeset.
    root_trees: HashMap<ObjectId, ObjectId>,
    refs: HashMap<RefName, RefState>,
    ref_log: Vec<RefLogEntry>,
}

impl MemoryStore {
    /// Creates empty repositories with these names.
    pub fn new(repositories: impl IntoIterator<Item = RepositoryName>) -> Self {
        Self {
            repositories: repositories
                .into_iter()
                .map(|name| (name, Mutex::default()))
                .collect(),
        }
    }

    /// The log of every ref move in the repository, oldest first.
    pub fn ref_log(&self, repository: &RepositoryName) -> Result<Vec<RefLogEntry>, StoreError> {
        Ok(self.lock(repository)?.ref_log.clone())
    }

    fn lock(&self, repository: &RepositoryName) -> Result<MutexGuard<'_, Repository>, StoreError> {
        let repo = self
            .repositories
            .get(repository)
            .ok_or_else(|| StoreError::UnknownRepository(repository.clone()))?;
        // Every update below completes or does nothing, so a panic elsewhere cannot leave a
        // repository half-written and the data stays usable.
        Ok(repo.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Store for MemoryStore {
    async fn object(
        &self,
        repository: &RepositoryName,
        id: &ObjectId,
    ) -> Result<Option<StoredObject>, StoreError> {
        Ok(self.lock(repository)?.objects.get(id).cloned())
    }

    async fn put_blob(
        &self,
        repository: &RepositoryName,
        id: ObjectId,
        content: Vec<u8>,
    ) -> Result<bool, StoreError> {
        let mut repo = self.lock(repository)?;
        if repo.objects.contains_key(&id) {
            return Ok(false);
        }
        repo.objects.insert(
            id,
            StoredObject {
                kind: ObjectKind::Blob,
                content,
            },
        );
        Ok(true)
    }

    async fn blob_size(
        &self,
        repository: &RepositoryName,
        id: &ObjectId,
    ) -> Result<Option<u64>, StoreError> {
        Ok(self
            .lock(repository)?
            .objects
            .get(id)
            .filter(|object| object.kind == ObjectKind::Blob)
            .map(|blob| blob.content.len() as u64))
    }

    async fn root_tree(
        &self,
        repository: &RepositoryName,
        changeset: &ObjectId,
    ) -> Result<Option<ObjectId>, StoreError> {
        Ok(self.lock(repository)?.root_trees.get(changeset).copied())
    }

    async fn append_changeset(
        &self,
        repository: &RepositoryName,
        changeset: NewChangeset,
    ) -> Result<(), StoreError> {
        let mut repo = self.lock(repository)?;
        if repo.root_trees.contains_key(&changeset.id) {
            return Ok(());
        }
        for (id, content) in changeset.trees {
            repo.objects.entry(id).or_insert(StoredObject {
                kind: ObjectKind::Tree,
                content,
            });
        }
        repo.objects.insert(
            changeset.id,
            StoredObject {
                kind: ObjectKind::Changeset,
                content: changeset.content,
            },
        );
        repo.root_trees.insert(changeset.id, changeset.root_tree);
        Ok(())
    }

    async fn resolve_ref(
        &self,
        repository: &RepositoryName,
        name: &RefName,
    ) -> Result<Option<RefState>, StoreError> {
        Ok(self.lock(repository)?.refs.get(name).copied())
    }

    async fn compare_and_swap_ref(
        &self,
        repository: &RepositoryName,
        name: &RefName,
        expected: Option<ObjectId>,
        desired: Option<ObjectId>,
    ) -> Result<SwapOutcome, StoreError> {
        let mut repo = self.lock(repository)?;
        let current = repo.refs.get(name).copied().unwrap_or_default();
        if current.target != expected {
            return Ok(SwapOutcome {
                swapped: false,
                current,
            });
        }
        if expected == desired {
            // Nothing moves, so there is nothing to log.
            return Ok(SwapOutcome {
                swapped: true,
                current,
            });
        }
        let new = RefState {
            target: desired,
            version: current.version + 1,
        };
        repo.refs.insert(name.clone(), new);
        repo.ref_log.push(RefLogEntry {
            ref_name: name.clone(),
            version: new.version,
            old: expected,
            new: desired,
        });
        Ok(SwapOutcome {
            swapped: true,
            current: new,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> RepositoryName {
        RepositoryName::new("test").unwrap()
    }

    fn id(byte: u8) -> ObjectId {
        ObjectId::from_bytes([byte; ObjectId::LEN])
    }

    #[tokio::test]
    async fn unknown_repositories_are_errors() {
        let store = MemoryStore::new([repo()]);
        let other = RepositoryName::new("other").unwrap();
        assert!(matches!(
            store.object(&other, &id(1)).await,
            Err(StoreError::UnknownRepository(name)) if name == other
        ));
    }

    #[tokio::test]
    async fn blobs_are_put_once() {
        let store = MemoryStore::new([repo()]);
        assert!(
            store
                .put_blob(&repo(), id(1), b"abc".to_vec())
                .await
                .unwrap()
        );
        assert!(
            !store
                .put_blob(&repo(), id(1), b"abc".to_vec())
                .await
                .unwrap()
        );
        assert_eq!(store.blob_size(&repo(), &id(1)).await.unwrap(), Some(3));
        assert_eq!(store.blob_size(&repo(), &id(2)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn changesets_are_appended_once_with_their_trees() {
        let store = MemoryStore::new([repo()]);
        let changeset = NewChangeset {
            id: id(1),
            content: vec![1],
            root_tree: id(2),
            trees: vec![(id(2), vec![2])],
        };
        store
            .append_changeset(&repo(), changeset.clone())
            .await
            .unwrap();
        let again = NewChangeset {
            root_tree: id(3),
            ..changeset
        };
        store.append_changeset(&repo(), again).await.unwrap();
        assert_eq!(store.root_tree(&repo(), &id(1)).await.unwrap(), Some(id(2)));
        let tree = store.object(&repo(), &id(2)).await.unwrap().unwrap();
        assert_eq!(tree.kind, ObjectKind::Tree);
        // A tree is not a blob.
        assert_eq!(store.blob_size(&repo(), &id(2)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn ref_versions_never_restart() {
        let store = MemoryStore::new([repo()]);
        let main = RefName::new("main").unwrap();
        let repo = repo();
        let swap = |expected, desired| store.compare_and_swap_ref(&repo, &main, expected, desired);

        let created = swap(None, Some(id(1))).await.unwrap();
        assert!(created.swapped);
        assert_eq!(created.current.version, 1);

        let stale = swap(Some(id(9)), Some(id(2))).await.unwrap();
        assert!(!stale.swapped);
        assert_eq!(stale.current, created.current);

        let unchanged = swap(Some(id(1)), Some(id(1))).await.unwrap();
        assert!(unchanged.swapped);
        assert_eq!(unchanged.current.version, 1);

        let deleted = swap(Some(id(1)), None).await.unwrap();
        assert_eq!(
            deleted.current,
            RefState {
                target: None,
                version: 2
            }
        );
        let recreated = swap(None, Some(id(3))).await.unwrap();
        assert_eq!(recreated.current.version, 3);

        let log = store.ref_log(&repo).unwrap();
        let moves: Vec<_> = log.iter().map(|e| (e.version, e.old, e.new)).collect();
        assert_eq!(
            moves,
            [
                (1, None, Some(id(1))),
                (2, Some(id(1)), None),
                (3, None, Some(id(3))),
            ]
        );
    }
}

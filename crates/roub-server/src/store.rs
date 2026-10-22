//! The storage roub-server runs on: objects, changesets with their derived root trees, and refs.
//!
//! `Store` is what the RPC handlers need and nothing more, so the in-memory store used by tests
//! and the PostgreSQL plus blob store backend implement the same contract. The handlers do all
//! validation; a store only persists what it is given and reports what is there.

use std::future::Future;

use roub_core::{ObjectId, ObjectKind, RefName, RepositoryName};

/// Why a store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("repository '{0}' does not exist")]
    UnknownRepository(RepositoryName),
    #[error("storage backend: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// An object as stored: its kind and its canonical, uncompressed bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredObject {
    pub kind: ObjectKind,
    pub content: Vec<u8>,
}

/// A changeset ready to append: its canonical bytes and everything derived from it, written
/// together so a stored changeset always has its trees.
#[derive(Clone, Debug)]
pub struct NewChangeset {
    pub id: ObjectId,
    pub content: Vec<u8>,
    pub root_tree: ObjectId,
    /// Encoded trees the derivation produced, by id. Trees that already exist are skipped.
    pub trees: Vec<(ObjectId, Vec<u8>)>,
}

/// A ref's row. A deleted ref keeps its row with no target, so its version never restarts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefState {
    pub target: Option<ObjectId>,
    /// Number of times the ref was created, moved or deleted; 0 if it never existed.
    pub version: u64,
}

/// The outcome of a compare-and-swap on a ref.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapOutcome {
    pub swapped: bool,
    /// The ref after the call: the new state on success, the winner's otherwise.
    pub current: RefState,
}

/// One entry of the append-only log of ref moves. Rows are never changed or removed, so a
/// verifiable ref log can be built from them later (`docs/design/decisions.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefLogEntry {
    pub ref_name: RefName,
    pub version: u64,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
}

/// Persistent state of every repository the server hosts.
pub trait Store: Send + Sync + 'static {
    /// Returns an object of any kind.
    fn object(
        &self,
        repository: &RepositoryName,
        id: &ObjectId,
    ) -> impl Future<Output = Result<Option<StoredObject>, StoreError>> + Send;

    /// Stores a blob unless it is already there. Returns `true` if it was written. The caller has
    /// checked that `id` is the blob id of `content`.
    fn put_blob(
        &self,
        repository: &RepositoryName,
        id: ObjectId,
        content: Vec<u8>,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Returns a blob's size in bytes, or `None` if it is not stored.
    fn blob_size(
        &self,
        repository: &RepositoryName,
        id: &ObjectId,
    ) -> impl Future<Output = Result<Option<u64>, StoreError>> + Send;

    /// Returns a changeset's root tree, or `None` if the changeset is not stored.
    fn root_tree(
        &self,
        repository: &RepositoryName,
        changeset: &ObjectId,
    ) -> impl Future<Output = Result<Option<ObjectId>, StoreError>> + Send;

    /// Appends a changeset with its derived trees in one transaction. Appending a changeset that
    /// is already stored changes nothing.
    fn append_changeset(
        &self,
        repository: &RepositoryName,
        changeset: NewChangeset,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Returns a ref's row, or `None` if the ref never existed.
    fn resolve_ref(
        &self,
        repository: &RepositoryName,
        name: &RefName,
    ) -> impl Future<Output = Result<Option<RefState>, StoreError>> + Send;

    /// Sets the ref's target to `desired` if its target is `expected` (`None`: no target), bumps
    /// its version and appends to the ref log, atomically. The caller has checked that `desired`
    /// is a stored changeset.
    fn compare_and_swap_ref(
        &self,
        repository: &RepositoryName,
        name: &RefName,
        expected: Option<ObjectId>,
        desired: Option<ObjectId>,
    ) -> impl Future<Output = Result<SwapOutcome, StoreError>> + Send;
}

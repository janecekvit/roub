//! Object model and path rules shared by every roub component.
//!
//! Everything here is pure: no I/O, no async. The server, the cache, the daemon and the CLI
//! all agree on object ids, on the canonical encoding of trees and changesets, and on which paths
//! a repository may contain through this crate. The format is specified in
//! `docs/design/object-format.md`.

pub mod changeset;
pub mod derive;
pub mod encoding;
pub mod object;
pub mod path;
pub mod tree;

pub use changeset::{Changeset, ChangesetFields, CopyFrom, FileChange, FileOp, Signature};
pub use derive::{DeriveError, DerivedTrees, TreeSource, derive_trees};
pub use encoding::{DecodeError, Extension};
pub use object::{ObjectId, ObjectKind, ParseObjectIdError};
pub use path::{PathError, RepoPath, find_case_collisions};
pub use tree::{EntryMode, Tree, TreeEntry};

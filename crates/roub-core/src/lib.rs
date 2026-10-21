//! Object model and path rules shared by every roub component.
//!
//! Everything here is pure: no I/O, no async. The server, the cache, the daemon and the CLI
//! all agree on object ids and on which paths a repository may contain through this crate.

pub mod object;
pub mod path;

pub use object::{ObjectId, ObjectKind, ParseObjectIdError};
pub use path::{PathError, RepoPath, find_case_collisions};

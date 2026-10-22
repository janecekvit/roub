//! Derives a changeset's trees from its first parent's root tree and its changes.
//!
//! Only the directories on a changed path are read and rewritten; every other subtree is reused
//! by id. That keeps the commit path's cost proportional to the change, not to the repository
//! (the "cost follows the change" rule in `CLAUDE.md`).

use std::collections::BTreeMap;

use crate::changeset::{FileChange, FileOp};
use crate::object::ObjectId;
use crate::path::{RepoPath, fold_case};
use crate::tree::{EntryMode, Tree, TreeEntry};

/// Where `derive_trees` reads the parent's existing trees from.
pub trait TreeSource {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Returns the tree with this id. Missing trees are an error: a parent's trees always exist.
    fn tree(&self, id: &ObjectId) -> Result<Tree, Self::Error>;
}

/// Why a changeset's changes cannot be applied to its first parent's tree.
#[derive(Debug, thiserror::Error)]
pub enum DeriveError {
    #[error("'{0}' is deleted but is not a file in the parent")]
    DeleteMissing(String),
    #[error("'{0}' is put with the content and mode it already has")]
    NoOpPut(String),
    #[error("'{0}' is a file in the parent, so nothing can be put inside it")]
    FileInPath(String),
    #[error("'{0}' and '{1}' differ only in letter case")]
    CaseCollision(String, String),
    #[error("tree entry '{0}': {1}")]
    InvalidTree(String, crate::encoding::DecodeError),
    #[error("reading tree {id}: {source}")]
    Source {
        id: ObjectId,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// The trees of a new changeset.
#[derive(Debug)]
pub struct DerivedTrees {
    /// The new root tree's id.
    pub root: ObjectId,
    /// Every tree on a changed path, root included, ready to store. Storing a tree that already
    /// exists is a no-op, since trees are content-addressed.
    pub new_trees: Vec<Tree>,
}

/// Applies `changes` (sorted, as `Changeset::changes` returns them) to `parent_root`, the first
/// parent's root tree, or `None` for a root changeset.
pub fn derive_trees<S: TreeSource>(
    source: &S,
    parent_root: Option<&ObjectId>,
    changes: &[FileChange],
) -> Result<DerivedTrees, DeriveError> {
    let base = match parent_root {
        Some(id) => Some(load(source, id)?),
        None => None,
    };
    let relative: Vec<(Vec<&str>, &FileChange)> = changes
        .iter()
        .map(|change| (change.path.components().collect(), change))
        .collect();
    let mut new_trees = Vec::new();
    let root = apply(source, base, &relative, 0, "", &mut new_trees)?
        // An empty repository still has a root tree: the empty one.
        .unwrap_or_else(|| {
            let empty = Tree::default();
            new_trees.push(empty.clone());
            empty.id()
        });
    Ok(DerivedTrees { root, new_trees })
}

/// Applies the changes under one directory. `depth` is how many leading components of each
/// change's path are already consumed; `prefix` is the directory's path, for error messages.
/// Returns the new directory's id, or `None` if it ends up empty.
fn apply<S: TreeSource>(
    source: &S,
    base: Option<Tree>,
    changes: &[(Vec<&str>, &FileChange)],
    depth: usize,
    prefix: &str,
    new_trees: &mut Vec<Tree>,
) -> Result<Option<ObjectId>, DeriveError> {
    let mut entries: BTreeMap<String, TreeEntry> = base
        .map(|tree| {
            tree.entries()
                .iter()
                .map(|entry| (entry.name.clone(), entry.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut touched: Vec<String> = Vec::new();

    // Sorted changes keep each child's changes contiguous: the change on the child itself first,
    // then the changes inside it.
    let mut index = 0;
    while index < changes.len() {
        let name = changes[index].0[depth];
        let end = changes[index..]
            .iter()
            .position(|(components, _)| components[depth] != name)
            .map_or(changes.len(), |offset| index + offset);
        let group = &changes[index..end];
        index = end;

        let path = join(prefix, name);
        let (direct, nested) = match group.first() {
            Some((components, change)) if components.len() == depth + 1 => {
                (Some(*change), &group[1..])
            }
            _ => (None, group),
        };
        let existing = entries.remove(name);

        if matches!(direct.map(|c| &c.op), Some(FileOp::Put { .. })) && !nested.is_empty() {
            return Err(DeriveError::FileInPath(path));
        }
        let child = if nested.is_empty() {
            None
        } else {
            let base_child = match &existing {
                Some(entry) if entry.mode == EntryMode::Tree => Some(load(source, &entry.id)?),
                // The changeset rules allow nested puts under a file only with a delete of it.
                Some(_) if matches!(direct.map(|c| &c.op), Some(FileOp::Delete)) => None,
                Some(_) => return Err(DeriveError::FileInPath(path)),
                None => None,
            };
            apply(source, base_child, nested, depth + 1, &path, new_trees)?
        };

        let result = match direct.map(|change| &change.op) {
            Some(FileOp::Delete) => {
                match &existing {
                    Some(entry) if entry.mode.is_file() => {}
                    _ => return Err(DeriveError::DeleteMissing(path)),
                }
                child.map(|id| directory_entry(name, id))
            }
            // A put replaces a file or a whole directory; nested changes under it were rejected
            // above, so `child` is `None` here.
            Some(FileOp::Put {
                mode, blob, size, ..
            }) => {
                let entry = TreeEntry {
                    name: name.to_owned(),
                    mode: *mode,
                    id: *blob,
                    size: *size,
                };
                if existing.as_ref() == Some(&entry) {
                    return Err(DeriveError::NoOpPut(path));
                }
                Some(entry)
            }
            None => child.map(|id| directory_entry(name, id)),
        };
        if let Some(entry) = result {
            touched.push(entry.name.clone());
            entries.insert(entry.name.clone(), entry);
        }
    }

    check_case(&entries, &touched, prefix)?;
    if entries.is_empty() && !prefix.is_empty() {
        return Ok(None);
    }
    let tree = Tree::new(entries.into_values().collect())
        .map_err(|e| DeriveError::InvalidTree(prefix.to_owned(), e))?;
    let id = tree.id();
    new_trees.push(tree);
    Ok(Some(id))
}

/// Rejects a new or changed name that collides with a sibling on a case-insensitive file system.
/// Only touched names are checked against their siblings, so the cost stays with the change.
fn check_case(
    entries: &BTreeMap<String, TreeEntry>,
    touched: &[String],
    prefix: &str,
) -> Result<(), DeriveError> {
    if touched.is_empty() {
        return Ok(());
    }
    let folded: BTreeMap<String, &str> = touched
        .iter()
        .map(|name| (fold_case(name), name.as_str()))
        .collect();
    for name in entries.keys() {
        if let Some(&touched_name) = folded.get(&fold_case(name))
            && touched_name != name
        {
            let (a, b) = if name.as_str() < touched_name {
                (name.as_str(), touched_name)
            } else {
                (touched_name, name.as_str())
            };
            return Err(DeriveError::CaseCollision(join(prefix, a), join(prefix, b)));
        }
    }
    Ok(())
}

fn directory_entry(name: &str, id: ObjectId) -> TreeEntry {
    TreeEntry {
        name: name.to_owned(),
        mode: EntryMode::Tree,
        id,
        size: 0,
    }
}

fn load<S: TreeSource>(source: &S, id: &ObjectId) -> Result<Tree, DeriveError> {
    source.tree(id).map_err(|e| DeriveError::Source {
        id: *id,
        source: Box::new(e),
    })
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Returns the entry at `path` under a root tree, or `None` if nothing is there. Reads one tree
/// per path component, so the cost follows the path's depth, not the repository's size.
pub fn lookup<S: TreeSource>(
    source: &S,
    root: &ObjectId,
    path: &RepoPath,
) -> Result<Option<TreeEntry>, DeriveError> {
    let mut tree = load(source, root)?;
    let mut components = path.components().peekable();
    while let Some(name) = components.next() {
        let Some(entry) = tree.get(name) else {
            return Ok(None);
        };
        if components.peek().is_none() {
            return Ok(Some(entry.clone()));
        }
        if entry.mode != EntryMode::Tree {
            return Ok(None);
        }
        tree = load(source, &entry.id)?;
    }
    Ok(None)
}

/// Lists every file under a root tree, for tests and diagnostics.
pub fn list_files<S: TreeSource>(
    source: &S,
    root: &ObjectId,
) -> Result<Vec<(RepoPath, TreeEntry)>, DeriveError> {
    let mut files = Vec::new();
    let mut stack = vec![(String::new(), *root)];
    while let Some((prefix, id)) = stack.pop() {
        for entry in load(source, &id)?.entries() {
            let path = join(&prefix, &entry.name);
            if entry.mode == EntryMode::Tree {
                stack.push((path, entry.id));
            } else {
                let repo_path =
                    RepoPath::new(path).expect("tree entry names are valid path components");
                files.push((repo_path, entry.clone()));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::convert::Infallible;

    use super::*;
    use crate::changeset::CopyFrom;
    use crate::object::ObjectKind;

    #[derive(Default)]
    struct Store(HashMap<ObjectId, Tree>);

    impl TreeSource for Store {
        type Error = Infallible;
        fn tree(&self, id: &ObjectId) -> Result<Tree, Infallible> {
            Ok(self.0[id].clone())
        }
    }

    impl Store {
        /// Applies changes and stores the new trees, like the server would.
        fn commit(
            &mut self,
            parent: Option<&ObjectId>,
            changes: Vec<FileChange>,
        ) -> Result<(ObjectId, usize), DeriveError> {
            let mut changes = changes;
            changes.sort_by(|a, b| a.path.cmp(&b.path));
            let derived = derive_trees(self, parent, &changes)?;
            let written = derived.new_trees.len();
            for tree in derived.new_trees {
                self.0.insert(tree.id(), tree);
            }
            Ok((derived.root, written))
        }

        fn files(&self, root: &ObjectId) -> Vec<String> {
            list_files(self, root)
                .unwrap()
                .into_iter()
                .map(|(path, entry)| format!("{path}={}", entry.size))
                .collect()
        }
    }

    fn put(path: &str, content: &[u8]) -> FileChange {
        FileChange {
            path: RepoPath::new(path).unwrap(),
            op: FileOp::Put {
                mode: EntryMode::Regular,
                blob: ObjectId::compute(ObjectKind::Blob, content),
                size: content.len() as u64,
                copy_from: None,
            },
        }
    }

    fn delete(path: &str) -> FileChange {
        FileChange {
            path: RepoPath::new(path).unwrap(),
            op: FileOp::Delete,
        }
    }

    #[test]
    fn builds_and_updates_nested_trees() {
        let mut store = Store::default();
        let (root1, _) = store
            .commit(
                None,
                vec![
                    put("a/b/c.txt", b"1"),
                    put("a/d.txt", b"22"),
                    put("e", b"333"),
                ],
            )
            .unwrap();
        assert_eq!(store.files(&root1), ["a/b/c.txt=1", "a/d.txt=2", "e=3"]);

        // Changing one file rewrites only the trees on its path: a/b, a and the root.
        let (root2, written) = store
            .commit(Some(&root1), vec![put("a/b/c.txt", b"4444")])
            .unwrap();
        assert_eq!(written, 3);
        assert_eq!(store.files(&root2), ["a/b/c.txt=4", "a/d.txt=2", "e=3"]);

        // Deleting the last file of a directory removes the directory.
        let (root3, _) = store
            .commit(Some(&root2), vec![delete("a/b/c.txt")])
            .unwrap();
        assert_eq!(store.files(&root3), ["a/d.txt=2", "e=3"]);
        assert!(store.0[&root3].get("a").is_some());
        let a = store.0[&root3].get("a").unwrap().id;
        assert!(store.0[&a].get("b").is_none());
    }

    #[test]
    fn same_content_gives_same_root() {
        let mut store = Store::default();
        let (one, _) = store
            .commit(None, vec![put("x/y", b"1"), put("z", b"2")])
            .unwrap();
        let (other, _) = store
            .commit(None, vec![put("z", b"2"), put("x/y", b"1")])
            .unwrap();
        assert_eq!(one, other);
    }

    #[test]
    fn file_and_directory_replace_each_other() {
        let mut store = Store::default();
        let (root, _) = store.commit(None, vec![put("a", b"file")]).unwrap();
        let (root, _) = store
            .commit(Some(&root), vec![delete("a"), put("a/inner", b"x")])
            .unwrap();
        assert_eq!(store.files(&root), ["a/inner=1"]);
        // A put on a directory replaces the whole directory.
        let (root, _) = store.commit(Some(&root), vec![put("a", b"back")]).unwrap();
        assert_eq!(store.files(&root), ["a=4"]);
    }

    #[test]
    fn rejects_inconsistent_changes() {
        let mut store = Store::default();
        let (root, _) = store
            .commit(None, vec![put("a", b"1"), put("d/f", b"2")])
            .unwrap();
        assert!(matches!(
            store.commit(Some(&root), vec![delete("missing")]),
            Err(DeriveError::DeleteMissing(_))
        ));
        assert!(matches!(
            store.commit(Some(&root), vec![delete("d")]),
            Err(DeriveError::DeleteMissing(_))
        ));
        assert!(matches!(
            store.commit(Some(&root), vec![put("a", b"1")]),
            Err(DeriveError::NoOpPut(_))
        ));
        assert!(matches!(
            store.commit(Some(&root), vec![put("a/x", b"1")]),
            Err(DeriveError::FileInPath(_))
        ));
    }

    #[test]
    fn rejects_case_collisions_with_existing_siblings() {
        let mut store = Store::default();
        let (root, _) = store
            .commit(None, vec![put("docs/Readme.md", b"1")])
            .unwrap();
        match store.commit(Some(&root), vec![put("docs/README.md", b"2")]) {
            Err(DeriveError::CaseCollision(a, b)) => {
                assert_eq!(
                    (a.as_str(), b.as_str()),
                    ("docs/README.md", "docs/Readme.md")
                );
            }
            other => panic!("expected a case collision, got {other:?}"),
        }
        // Directories collide too.
        assert!(matches!(
            store.commit(Some(&root), vec![put("Docs/other", b"3")]),
            Err(DeriveError::CaseCollision(_, _))
        ));
        // Renaming by case in one changeset is fine: the old name is deleted.
        let (renamed, _) = store
            .commit(
                Some(&root),
                vec![delete("docs/Readme.md"), put("docs/README.md", b"1")],
            )
            .unwrap();
        assert_eq!(store.files(&renamed), ["docs/README.md=1"]);
    }

    #[test]
    fn empty_repository_has_the_empty_root() {
        let mut store = Store::default();
        let (root, _) = store.commit(None, vec![]).unwrap();
        assert_eq!(root, Tree::default().id());
        let (with_file, _) = store.commit(Some(&root), vec![put("f", b"x")]).unwrap();
        let (emptied, _) = store.commit(Some(&with_file), vec![delete("f")]).unwrap();
        assert_eq!(emptied, root);
    }

    #[test]
    fn copy_source_does_not_change_the_tree() {
        let mut store = Store::default();
        let mut copy = put("b", b"same");
        if let FileOp::Put { copy_from, .. } = &mut copy.op {
            *copy_from = Some(CopyFrom {
                parent: 0,
                path: RepoPath::new("a").unwrap(),
            });
        }
        let (with_copy, _) = store.commit(None, vec![put("a", b"same"), copy]).unwrap();
        let (plain, _) = store
            .commit(None, vec![put("a", b"same"), put("b", b"same")])
            .unwrap();
        assert_eq!(with_copy, plain);
    }

    #[test]
    fn looks_up_files_and_directories() {
        let mut store = Store::default();
        let (root, _) = store
            .commit(None, vec![put("a/b/c.txt", b"1"), put("d", b"22")])
            .unwrap();
        let find = |path: &str| lookup(&store, &root, &RepoPath::new(path).unwrap()).unwrap();
        assert_eq!(find("a/b/c.txt").map(|e| e.size), Some(1));
        assert_eq!(find("d").map(|e| e.mode), Some(EntryMode::Regular));
        assert_eq!(find("a/b").map(|e| e.mode), Some(EntryMode::Tree));
        assert_eq!(find("a/x"), None);
        // A path through a file finds nothing.
        assert_eq!(find("d/e"), None);
    }
}

//! Trees: one directory listing each, content-addressed (`docs/design/object-format.md`).
//!
//! Trees are derived data. A changeset names only the files that changed, and the server
//! computes the trees from the first parent's root tree (`derive`). Clients still fetch trees
//! by id, so a tree's encoding is part of the on-disk and wire format.

use crate::encoding::{
    DecodeError, Extension, Reader, Writer, check_extensions, read_extensions, write_extensions,
};
use crate::object::{ObjectId, ObjectKind};
use crate::path::validate_component;

/// The only tree format version so far.
const TREE_FORMAT_V1: u8 = 1;

/// What a tree entry points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum EntryMode {
    /// A regular file; the entry's id is a blob.
    Regular = 1,
    /// An executable file; the entry's id is a blob.
    Executable = 2,
    /// A symbolic link; the entry's id is a blob holding the link target.
    Symlink = 3,
    /// A directory; the entry's id is a tree.
    Tree = 4,
}

impl EntryMode {
    pub(crate) fn from_u8(value: u8) -> Result<Self, DecodeError> {
        Ok(match value {
            1 => Self::Regular,
            2 => Self::Executable,
            3 => Self::Symlink,
            4 => Self::Tree,
            tag => return Err(DecodeError::UnknownTag { field: "mode", tag }),
        })
    }

    /// Whether the entry is a file of some kind, as opposed to a directory.
    pub fn is_file(self) -> bool {
        self != Self::Tree
    }
}

/// One name in a directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TreeEntry {
    /// A single path component, valid on Linux and Windows.
    pub name: String,
    pub mode: EntryMode,
    /// A blob for files and symlinks, a tree for directories.
    pub id: ObjectId,
    /// File size in bytes (link target length for symlinks), so a checkout or a virtual file
    /// system can list a directory without fetching any blob. Always 0 for directories.
    pub size: u64,
}

/// A directory listing, entries sorted by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tree {
    entries: Vec<TreeEntry>,
    extensions: Vec<Extension>,
}

impl Tree {
    /// Builds a tree, checking every rule the decoder checks.
    pub fn new(entries: Vec<TreeEntry>) -> Result<Self, DecodeError> {
        Self::with_extensions(entries, Vec::new())
    }

    pub fn with_extensions(
        mut entries: Vec<TreeEntry>,
        extensions: Vec<Extension>,
    ) -> Result<Self, DecodeError> {
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        check_entries(&entries)?;
        check_extensions(&extensions)?;
        Ok(Self {
            entries,
            extensions,
        })
    }

    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }

    pub fn extensions(&self) -> &[Extension] {
        &self.extensions
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up an entry by its exact name.
    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries
            .binary_search_by(|entry| entry.name.as_bytes().cmp(name.as_bytes()))
            .ok()
            .map(|index| &self.entries[index])
    }

    /// The canonical encoding.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(TREE_FORMAT_V1);
        w.len(self.entries.len());
        for entry in &self.entries {
            w.str(&entry.name);
            w.u8(entry.mode as u8);
            w.id(&entry.id);
            w.uvarint(entry.size);
        }
        write_extensions(&mut w, &self.extensions);
        w.into_bytes()
    }

    /// Decodes a canonical encoding; any other bytes are rejected.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let version = r.u8()?;
        if version != TREE_FORMAT_V1 {
            return Err(DecodeError::UnsupportedVersion(version));
        }
        let count = r.count()?;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let name = r.str()?.to_owned();
            let mode = EntryMode::from_u8(r.u8()?)?;
            let id = r.id()?;
            let size = r.uvarint()?;
            entries.push(TreeEntry {
                name,
                mode,
                id,
                size,
            });
        }
        let extensions = read_extensions(&mut r)?;
        r.finish()?;
        check_entries(&entries)?;
        Ok(Self {
            entries,
            extensions,
        })
    }

    /// The tree's object id: BLAKE3 of the canonical encoding in the tree context.
    pub fn id(&self) -> ObjectId {
        ObjectId::compute(ObjectKind::Tree, &self.encode())
    }
}

/// Names are valid components, strictly increasing by bytes; directories have size 0.
fn check_entries(entries: &[TreeEntry]) -> Result<(), DecodeError> {
    for entry in entries {
        validate_component(&entry.name)
            .map_err(|e| DecodeError::Invalid(format!("tree entry name: {e}")))?;
        if entry.mode == EntryMode::Tree && entry.size != 0 {
            return Err(DecodeError::Invalid(format!(
                "directory entry '{}' must have size 0",
                entry.name
            )));
        }
    }
    for pair in entries.windows(2) {
        if pair[0].name.as_bytes() >= pair[1].name.as_bytes() {
            return Err(DecodeError::Invalid(format!(
                "tree entries must be strictly sorted by name: '{}' then '{}'",
                pair[0].name, pair[1].name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(content: &[u8]) -> ObjectId {
        ObjectId::compute(ObjectKind::Blob, content)
    }

    fn file(name: &str, content: &[u8]) -> TreeEntry {
        TreeEntry {
            name: name.to_owned(),
            mode: EntryMode::Regular,
            id: blob(content),
            size: content.len() as u64,
        }
    }

    #[test]
    fn sorts_on_construction_and_round_trips() {
        let tree = Tree::new(vec![file("b.txt", b"b"), file("a.txt", b"a")]).unwrap();
        assert_eq!(tree.entries()[0].name, "a.txt");
        let bytes = tree.encode();
        assert_eq!(Tree::decode(&bytes), Ok(tree.clone()));
        assert_eq!(tree.get("b.txt").unwrap().size, 1);
        assert!(tree.get("c.txt").is_none());
    }

    #[test]
    fn empty_tree_golden() {
        let tree = Tree::default();
        assert_eq!(tree.encode(), [0x01, 0x00, 0x00]);
        assert_eq!(
            tree.id().to_string(),
            "ed28216ca8aa0beefc5b9074e4841bc8cd551fd5a5a24fc9f05c88bc86724286"
        );
    }

    #[test]
    fn one_file_golden() {
        let tree = Tree::new(vec![file("a", b"hello")]).unwrap();
        let mut expected = vec![0x01, 0x01, 0x01, b'a', 0x01];
        expected.extend_from_slice(blob(b"hello").as_bytes());
        expected.extend_from_slice(&[0x05, 0x00]);
        assert_eq!(tree.encode(), expected);
        // Computed independently with the reference BLAKE3 implementation.
        assert_eq!(
            tree.id().to_string(),
            "ae0bf66b4d1833721f77a88cff499e19879014d36faaf3fc8b9c6ad11ea40ee3"
        );
    }

    #[test]
    fn rejects_unsorted_duplicate_and_invalid_names() {
        let a = file("a", b"1");
        let mut bytes = Tree::new(vec![a.clone(), file("b", b"2")])
            .unwrap()
            .encode();
        // Swap the names "a" and "b" in place: same length, now out of order.
        let first = bytes.iter().position(|&b| b == b'a').unwrap();
        let second = bytes.iter().rposition(|&b| b == b'b').unwrap();
        bytes.swap(first, second);
        assert!(matches!(Tree::decode(&bytes), Err(DecodeError::Invalid(_))));

        assert!(Tree::new(vec![a.clone(), a]).is_err());
        assert!(Tree::new(vec![file("CON", b"x")]).is_err());
        assert!(Tree::new(vec![file("a/b", b"x")]).is_err());
    }

    #[test]
    fn rejects_bad_versions_modes_and_sizes() {
        assert_eq!(
            Tree::decode(&[0x02, 0x00, 0x00]),
            Err(DecodeError::UnsupportedVersion(2))
        );
        let mut bytes = Tree::new(vec![file("a", b"x")]).unwrap().encode();
        bytes[4] = 9;
        assert_eq!(
            Tree::decode(&bytes),
            Err(DecodeError::UnknownTag {
                field: "mode",
                tag: 9
            })
        );
        let dir = TreeEntry {
            name: "d".into(),
            mode: EntryMode::Tree,
            id: Tree::default().id(),
            size: 1,
        };
        assert!(Tree::new(vec![dir]).is_err());
        assert_eq!(
            Tree::decode(&[0x01, 0x00, 0x00, 0x00]),
            Err(DecodeError::TrailingBytes(1))
        );
    }
}

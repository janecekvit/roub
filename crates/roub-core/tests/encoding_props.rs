//! Property tests for the canonical encoding: everything built through the public API
//! round-trips to the same bytes and id, and no input makes a decoder panic.

use proptest::collection::{btree_map, vec};
use proptest::prelude::*;
use roub_core::{
    Changeset, ChangesetFields, CopyFrom, EntryMode, FileChange, FileOp, ObjectId, ObjectKind,
    RepoPath, Signature, Tree, TreeEntry,
};

fn name() -> impl Strategy<Value = String> {
    // Short names over a small alphabet, so generated paths share directories.
    "[a-c][a-c0-9._-]{0,3}".prop_filter("valid component", |n| {
        !n.ends_with('.') && RepoPath::new(n.clone()).is_ok()
    })
}

fn id() -> impl Strategy<Value = ObjectId> {
    any::<[u8; ObjectId::LEN]>().prop_map(ObjectId::from_bytes)
}

fn file_mode() -> impl Strategy<Value = EntryMode> {
    prop_oneof![
        Just(EntryMode::Regular),
        Just(EntryMode::Executable),
        Just(EntryMode::Symlink),
    ]
}

fn tree() -> impl Strategy<Value = Tree> {
    btree_map(
        name(),
        (
            prop_oneof![file_mode(), Just(EntryMode::Tree)],
            id(),
            any::<u64>(),
        ),
        0..8,
    )
    .prop_map(|entries| {
        let entries = entries
            .into_iter()
            .map(|(name, (mode, id, size))| TreeEntry {
                name,
                mode,
                id,
                size: if mode == EntryMode::Tree { 0 } else { size },
            })
            .collect();
        Tree::new(entries).expect("generated tree is valid")
    })
}

fn signature() -> impl Strategy<Value = Signature> {
    (
        "[A-Za-z ]{0,12}",
        "[a-z@.]{0,12}",
        any::<i64>(),
        -1440i64..=1440,
    )
        .prop_map(|(name, email, time, tz_offset_minutes)| Signature {
            name,
            email,
            time,
            tz_offset_minutes,
        })
}

fn changeset() -> impl Strategy<Value = Changeset> {
    let parents = vec(id(), 0..4).prop_map(|mut ids| {
        ids.sort();
        ids.dedup();
        ids
    });
    let paths = btree_map(
        vec(name(), 1..4).prop_map(|parts| parts.join("/")),
        any::<bool>(),
        0..8,
    );
    (
        parents,
        signature(),
        proptest::option::of(signature()),
        "\\PC{0,40}",
        paths,
        id(),
        file_mode(),
    )
        .prop_map(|(parents, author, committer, message, paths, blob, mode)| {
            // Drop any path that would put a file where another path needs a directory.
            let mut paths: Vec<(RepoPath, bool)> = paths
                .into_iter()
                .map(|(p, delete)| (RepoPath::new(p).expect("valid path"), delete))
                .collect();
            paths.sort_by(|a, b| a.0.cmp(&b.0));
            let mut kept: Vec<(RepoPath, bool)> = Vec::new();
            for (path, delete) in paths {
                if kept
                    .iter()
                    .any(|(other, other_delete)| !*other_delete && other.is_ancestor_of(&path))
                {
                    continue;
                }
                kept.push((path, delete));
            }
            let first_parent = !parents.is_empty();
            let changes = kept
                .into_iter()
                .map(|(path, delete)| FileChange {
                    op: if delete {
                        FileOp::Delete
                    } else {
                        FileOp::Put {
                            mode,
                            blob,
                            size: path.as_str().len() as u64,
                            copy_from: first_parent.then(|| CopyFrom {
                                parent: 0,
                                path: path.clone(),
                            }),
                        }
                    },
                    path,
                })
                .collect();
            Changeset::new(ChangesetFields {
                parents,
                author,
                committer,
                message,
                changes,
                extensions: Vec::new(),
            })
            .expect("generated changeset is valid")
        })
}

proptest! {
    #[test]
    fn tree_round_trips(tree in tree()) {
        let bytes = tree.encode();
        let decoded = Tree::decode(&bytes).unwrap();
        prop_assert_eq!(decoded.encode(), bytes);
        prop_assert_eq!(decoded.id(), tree.id());
    }

    #[test]
    fn changeset_round_trips(changeset in changeset()) {
        let bytes = changeset.encode();
        let decoded = Changeset::decode(&bytes).unwrap();
        prop_assert_eq!(decoded.encode(), bytes);
        prop_assert_eq!(decoded.id(), changeset.id());
    }

    #[test]
    fn any_accepted_bytes_are_canonical(bytes in vec(any::<u8>(), 0..256)) {
        // Whatever a decoder accepts re-encodes to exactly the same bytes: the encoding is
        // unique, so the id of accepted bytes is the id of the object.
        if let Ok(tree) = Tree::decode(&bytes) {
            prop_assert_eq!(tree.encode(), bytes.clone());
        }
        if let Ok(changeset) = Changeset::decode(&bytes) {
            prop_assert_eq!(changeset.encode(), bytes);
        }
    }

    #[test]
    fn single_byte_corruption_never_panics(changeset in changeset(), at in any::<usize>(), byte in any::<u8>()) {
        let mut bytes = changeset.encode();
        let at = at % bytes.len();
        bytes[at] = byte;
        if let Ok(decoded) = Changeset::decode(&bytes) {
            prop_assert_eq!(decoded.encode(), bytes);
        }
    }
}

#[test]
fn ids_are_domain_separated_across_kinds() {
    let bytes = Tree::default().encode();
    assert_ne!(
        ObjectId::compute(ObjectKind::Blob, &bytes),
        Tree::default().id()
    );
}

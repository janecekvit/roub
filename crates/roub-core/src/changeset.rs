//! Changesets: the unit of work, one entry of the write-ahead log
//! (`docs/design/object-format.md`).
//!
//! A changeset lists only the paths that changed relative to its first parent, plus who made the
//! change and why. It does not name a root tree: trees are derived from the first parent's tree
//! and these changes (`derive`), so the derivation can evolve without changing changeset ids.

use crate::encoding::{
    DecodeError, Extension, Reader, Writer, check_extensions, read_extensions, write_extensions,
};
use crate::object::{ObjectId, ObjectKind};
use crate::path::RepoPath;
use crate::tree::EntryMode;

/// The only changeset format version so far.
const CHANGESET_FORMAT_V1: u8 = 1;

/// Most parents a changeset may have. Octopus merges beyond this are not worth the cost of
/// unbounded parent lists in every ancestry query.
pub const MAX_PARENTS: usize = 16;

/// Largest timezone offset accepted, in minutes east of UTC.
pub const MAX_TZ_OFFSET_MINUTES: i64 = 24 * 60;

const OP_PUT: u8 = 1;
const OP_DELETE: u8 = 2;

/// Who made a change, and when.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Signature {
    pub name: String,
    pub email: String,
    /// Seconds since the Unix epoch, UTC.
    pub time: i64,
    /// Minutes east of UTC at the author's location: `+120` for Prague in summer.
    pub tz_offset_minutes: i64,
}

/// Where a copied or renamed file came from: a path in one of the parents.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CopyFrom {
    /// Index into `Changeset::parents`.
    pub parent: usize,
    pub path: RepoPath,
}

/// What happens to one path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FileOp {
    /// The path now holds this file, added or modified. A mode change is a `Put` with a new mode.
    /// A `Put` on a path that is a directory in the first parent replaces the whole directory.
    Put {
        /// `Regular`, `Executable` or `Symlink`; never `Tree`.
        mode: EntryMode,
        blob: ObjectId,
        size: u64,
        /// Set for a copy, or for a rename together with a `Delete` of the source.
        copy_from: Option<CopyFrom>,
    },
    /// The file at this path is removed. Directories are never deleted directly; a directory
    /// disappears when its last file does. Replacing a file with a directory is a `Delete` of
    /// the file plus `Put`s inside the new directory.
    Delete,
}

/// One changed path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileChange {
    pub path: RepoPath,
    pub op: FileOp,
}

/// A changeset: parents, metadata and the files changed relative to the first parent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Changeset {
    parents: Vec<ObjectId>,
    author: Signature,
    committer: Option<Signature>,
    message: String,
    changes: Vec<FileChange>,
    extensions: Vec<Extension>,
}

/// The fields of a changeset, before validation.
#[derive(Clone, Debug)]
pub struct ChangesetFields {
    pub parents: Vec<ObjectId>,
    pub author: Signature,
    /// `None` when the committer is the author.
    pub committer: Option<Signature>,
    pub message: String,
    pub changes: Vec<FileChange>,
    pub extensions: Vec<Extension>,
}

impl Changeset {
    /// Builds a changeset, sorting the changes and checking every rule the decoder checks.
    pub fn new(mut fields: ChangesetFields) -> Result<Self, DecodeError> {
        fields.changes.sort_by(|a, b| a.path.cmp(&b.path));
        let changeset = Self {
            parents: fields.parents,
            author: fields.author,
            committer: fields.committer,
            message: fields.message,
            changes: fields.changes,
            extensions: fields.extensions,
        };
        check_extensions(&changeset.extensions)?;
        changeset.check()?;
        Ok(changeset)
    }

    /// Parents in order; the first is the one the changes are relative to.
    pub fn parents(&self) -> &[ObjectId] {
        &self.parents
    }

    pub fn first_parent(&self) -> Option<&ObjectId> {
        self.parents.first()
    }

    pub fn author(&self) -> &Signature {
        &self.author
    }

    /// The committer, which is the author when not recorded separately.
    pub fn committer(&self) -> &Signature {
        self.committer.as_ref().unwrap_or(&self.author)
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Changed paths in tree-walk order (`RepoPath`'s ordering).
    pub fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    pub fn extensions(&self) -> &[Extension] {
        &self.extensions
    }

    /// The canonical encoding.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(CHANGESET_FORMAT_V1);
        w.len(self.parents.len());
        for parent in &self.parents {
            w.id(parent);
        }
        write_signature(&mut w, &self.author);
        match &self.committer {
            Some(committer) => {
                w.flag(true);
                write_signature(&mut w, committer);
            }
            None => w.flag(false),
        }
        w.str(&self.message);
        w.len(self.changes.len());
        for change in &self.changes {
            w.str(change.path.as_str());
            match &change.op {
                FileOp::Put {
                    mode,
                    blob,
                    size,
                    copy_from,
                } => {
                    w.u8(OP_PUT);
                    w.u8(*mode as u8);
                    w.id(blob);
                    w.uvarint(*size);
                    match copy_from {
                        Some(copy) => {
                            w.flag(true);
                            w.len(copy.parent);
                            w.str(copy.path.as_str());
                        }
                        None => w.flag(false),
                    }
                }
                FileOp::Delete => w.u8(OP_DELETE),
            }
        }
        write_extensions(&mut w, &self.extensions);
        w.into_bytes()
    }

    /// Decodes a canonical encoding; any other bytes are rejected.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let version = r.u8()?;
        if version != CHANGESET_FORMAT_V1 {
            return Err(DecodeError::UnsupportedVersion(version));
        }
        let parent_count = r.count()?;
        let mut parents = Vec::with_capacity(parent_count);
        for _ in 0..parent_count {
            parents.push(r.id()?);
        }
        let author = read_signature(&mut r)?;
        let committer = if r.flag()? {
            Some(read_signature(&mut r)?)
        } else {
            None
        };
        let message = r.str()?.to_owned();
        let change_count = r.count()?;
        let mut changes = Vec::with_capacity(change_count);
        for _ in 0..change_count {
            let path = read_path(&mut r)?;
            let op = match r.u8()? {
                OP_PUT => {
                    let mode = EntryMode::from_u8(r.u8()?)?;
                    let blob = r.id()?;
                    let size = r.uvarint()?;
                    let copy_from = if r.flag()? {
                        let parent = r.uvarint()?;
                        let parent = usize::try_from(parent)
                            .map_err(|_| DecodeError::IntegerOutOfRange(parent))?;
                        Some(CopyFrom {
                            parent,
                            path: read_path(&mut r)?,
                        })
                    } else {
                        None
                    };
                    FileOp::Put {
                        mode,
                        blob,
                        size,
                        copy_from,
                    }
                }
                OP_DELETE => FileOp::Delete,
                tag => {
                    return Err(DecodeError::UnknownTag {
                        field: "file operation",
                        tag,
                    });
                }
            };
            changes.push(FileChange { path, op });
        }
        let extensions = read_extensions(&mut r)?;
        r.finish()?;
        let changeset = Self {
            parents,
            author,
            committer,
            message,
            changes,
            extensions,
        };
        changeset.check()?;
        Ok(changeset)
    }

    /// The changeset's object id: BLAKE3 of the canonical encoding in the changeset context.
    pub fn id(&self) -> ObjectId {
        ObjectId::compute(ObjectKind::Changeset, &self.encode())
    }

    /// The structural rules, shared by the constructor and the decoder.
    fn check(&self) -> Result<(), DecodeError> {
        let invalid = |message: String| Err(DecodeError::Invalid(message));
        if self.parents.len() > MAX_PARENTS {
            return invalid(format!(
                "{} parents, at most {MAX_PARENTS} allowed",
                self.parents.len()
            ));
        }
        for (index, parent) in self.parents.iter().enumerate() {
            if self.parents[..index].contains(parent) {
                return invalid(format!("parent {parent} is listed twice"));
            }
        }
        check_signature(&self.author)?;
        if let Some(committer) = &self.committer {
            check_signature(committer)?;
        }
        for pair in self.changes.windows(2) {
            let (previous, next) = (&pair[0], &pair[1]);
            if previous.path >= next.path {
                return invalid(format!(
                    "changes must be strictly sorted by path: '{}' then '{}'",
                    previous.path, next.path
                ));
            }
        }
        // A file cannot be put where another change needs a directory. Sorted order puts every
        // path directly before the paths inside it, so checking neighbours is enough.
        for (index, change) in self.changes.iter().enumerate() {
            if matches!(change.op, FileOp::Put { .. })
                && let Some(next) = self.changes.get(index + 1)
                && change.path.is_ancestor_of(&next.path)
            {
                return invalid(format!(
                    "'{}' is put as a file but '{}' is inside it",
                    change.path, next.path
                ));
            }
            if let FileOp::Put {
                mode, copy_from, ..
            } = &change.op
            {
                if !mode.is_file() {
                    return invalid(format!(
                        "'{}': a change puts a file, not a directory",
                        change.path
                    ));
                }
                if let Some(copy) = copy_from
                    && copy.parent >= self.parents.len()
                {
                    return invalid(format!(
                        "'{}' is copied from parent {} of {}",
                        change.path,
                        copy.parent,
                        self.parents.len()
                    ));
                }
            }
        }
        Ok(())
    }
}

fn write_signature(w: &mut Writer, signature: &Signature) {
    w.str(&signature.name);
    w.str(&signature.email);
    w.svarint(signature.time);
    w.svarint(signature.tz_offset_minutes);
}

fn read_signature(r: &mut Reader<'_>) -> Result<Signature, DecodeError> {
    Ok(Signature {
        name: r.str()?.to_owned(),
        email: r.str()?.to_owned(),
        time: r.svarint()?,
        tz_offset_minutes: r.svarint()?,
    })
}

fn check_signature(signature: &Signature) -> Result<(), DecodeError> {
    for (field, value) in [("name", &signature.name), ("email", &signature.email)] {
        if value.chars().any(char::is_control) {
            return Err(DecodeError::Invalid(format!(
                "signature {field} contains a control character"
            )));
        }
    }
    // A range check, not `abs()`: `i64::MIN.abs()` overflows (found by fuzzing).
    if !(-MAX_TZ_OFFSET_MINUTES..=MAX_TZ_OFFSET_MINUTES).contains(&signature.tz_offset_minutes) {
        return Err(DecodeError::Invalid(format!(
            "timezone offset {} minutes is out of range",
            signature.tz_offset_minutes
        )));
    }
    Ok(())
}

fn read_path(r: &mut Reader<'_>) -> Result<RepoPath, DecodeError> {
    RepoPath::new(r.str()?).map_err(|e| DecodeError::Invalid(format!("path: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn signature() -> Signature {
        Signature {
            name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            time: 1_792_000_000,
            tz_offset_minutes: 120,
        }
    }

    fn path(s: &str) -> RepoPath {
        RepoPath::new(s).unwrap()
    }

    fn put(p: &str, content: &[u8]) -> FileChange {
        FileChange {
            path: path(p),
            op: FileOp::Put {
                mode: EntryMode::Regular,
                blob: ObjectId::compute(ObjectKind::Blob, content),
                size: content.len() as u64,
                copy_from: None,
            },
        }
    }

    fn delete(p: &str) -> FileChange {
        FileChange {
            path: path(p),
            op: FileOp::Delete,
        }
    }

    fn fields(parents: Vec<ObjectId>, changes: Vec<FileChange>) -> ChangesetFields {
        ChangesetFields {
            parents,
            author: signature(),
            committer: None,
            message: "Add a file\n".into(),
            changes,
            extensions: Vec::new(),
        }
    }

    fn parent(n: u8) -> ObjectId {
        ObjectId::from_bytes([n; ObjectId::LEN])
    }

    #[test]
    fn round_trips_and_sorts_changes() {
        let changeset = Changeset::new(fields(
            vec![parent(1)],
            vec![put("b", b"b"), delete("a.b"), put("a/c", b"c")],
        ))
        .unwrap();
        let order: Vec<&str> = changeset
            .changes()
            .iter()
            .map(|c| c.path.as_str())
            .collect();
        assert_eq!(order, ["a/c", "a.b", "b"]);
        assert_eq!(
            Changeset::decode(&changeset.encode()),
            Ok(changeset.clone())
        );
        assert_eq!(changeset.committer(), changeset.author());
    }

    #[test]
    fn root_changeset_golden() {
        let changeset = Changeset::new(fields(Vec::new(), vec![put("a", b"hello")])).unwrap();
        let mut expected = vec![0x01, 0x00];
        // Author: name, email, time 1_792_000_000 (zigzag 3_584_000_000), tz +120 (zigzag 240).
        expected.push(12);
        expected.extend_from_slice(b"Ada Lovelace");
        expected.push(15);
        expected.extend_from_slice(b"ada@example.com");
        expected.extend_from_slice(&[0x80, 0x80, 0xfe, 0xac, 0x0d, 0xf0, 0x01]);
        // No committer, message, one change.
        expected.push(0x00);
        expected.push(11);
        expected.extend_from_slice(b"Add a file\n");
        expected.extend_from_slice(&[0x01, 0x01, b'a', 0x01, 0x01]);
        expected.extend_from_slice(ObjectId::compute(ObjectKind::Blob, b"hello").as_bytes());
        // Size 5, no copy source, no extensions.
        expected.extend_from_slice(&[0x05, 0x00, 0x00]);
        assert_eq!(changeset.encode(), expected);
        // Computed independently with the reference BLAKE3 implementation.
        assert_eq!(
            changeset.id().to_string(),
            "2d219349af1bfe55bfd16510324b54309d9eff291a69dcbbf14fb1dd8d525475"
        );
    }

    #[test]
    fn file_may_become_directory_but_not_both_at_once() {
        // Replacing the file `a` with a directory `a/` is a delete plus puts inside it; the
        // reverse is a single put of `a`, which replaces the directory.
        assert!(
            Changeset::new(fields(vec![parent(1)], vec![delete("a"), put("a/b", b"x")])).is_ok()
        );
        assert!(
            Changeset::new(fields(vec![parent(1)], vec![put("a", b"x"), delete("a/b")])).is_err()
        );
        assert!(
            Changeset::new(fields(
                vec![parent(1)],
                vec![put("a", b"x"), put("a/b", b"y")]
            ))
            .is_err()
        );
    }

    #[test]
    fn rejects_duplicates_bad_parents_and_copies() {
        assert!(
            Changeset::new(fields(vec![parent(1)], vec![put("a", b"x"), delete("a")])).is_err()
        );
        assert!(Changeset::new(fields(vec![parent(1), parent(1)], vec![])).is_err());
        assert!(
            Changeset::new(fields(
                (0..=MAX_PARENTS as u8).map(parent).collect(),
                vec![]
            ))
            .is_err()
        );

        let mut copy = put("b", b"x");
        if let FileOp::Put { copy_from, .. } = &mut copy.op {
            *copy_from = Some(CopyFrom {
                parent: 1,
                path: path("a"),
            });
        }
        assert!(Changeset::new(fields(vec![parent(1)], vec![copy.clone()])).is_err());
        assert!(Changeset::new(fields(vec![parent(1), parent(2)], vec![copy])).is_ok());

        let mut tree_put = put("d", b"x");
        if let FileOp::Put { mode, .. } = &mut tree_put.op {
            *mode = EntryMode::Tree;
        }
        assert!(Changeset::new(fields(vec![parent(1)], vec![tree_put])).is_err());
    }

    #[test]
    fn rejects_bad_signatures() {
        let mut f = fields(vec![], vec![]);
        f.author.name = "Ada\nEve".into();
        assert!(Changeset::new(f).is_err());
        let mut f = fields(vec![], vec![]);
        f.author.tz_offset_minutes = 24 * 60 + 1;
        assert!(Changeset::new(f).is_err());
    }

    #[test]
    fn rejects_extreme_timezone_offsets() {
        for tz_offset_minutes in [i64::MIN, -(24 * 60) - 1, i64::MAX] {
            let mut f = fields(vec![], vec![]);
            f.author.tz_offset_minutes = tz_offset_minutes;
            assert!(Changeset::new(f).is_err(), "{tz_offset_minutes}");
        }
        // Fuzzer input: an empty author with time -1 and offset i64::MIN.
        let mut bytes = vec![0x01, 0x00, 0x00, 0x00, 0x01];
        bytes.extend_from_slice(&[0xff; 9]);
        bytes.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x00]);
        assert!(matches!(
            Changeset::decode(&bytes),
            Err(DecodeError::Invalid(_))
        ));
    }

    #[test]
    fn decoder_rejects_unsorted_changes() {
        let changeset =
            Changeset::new(fields(vec![], vec![put("a", b"1"), put("b", b"2")])).unwrap();
        let mut bytes = changeset.encode();
        // Rename the path "a" to "c" in place: the changes are now out of order. The path is
        // encoded as length 1, the byte, then the put operation tag.
        let at = bytes
            .windows(3)
            .position(|w| w == [0x01, b'a', OP_PUT])
            .unwrap();
        bytes[at + 1] = b'c';
        assert!(matches!(
            Changeset::decode(&bytes),
            Err(DecodeError::Invalid(_))
        ));
    }
}

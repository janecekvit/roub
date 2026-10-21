//! Content-addressed object identifiers.
//!
//! An object id is the BLAKE3-256 digest of the object's canonical, uncompressed encoding.
//! Hashing the uncompressed bytes keeps ids stable if the storage compression changes.
//! Each object kind hashes in its own BLAKE3 key-derivation context, so a blob and a tree
//! with identical bytes never share an id.

use std::fmt;
use std::str::FromStr;

/// The kinds of immutable objects roub stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    /// File contents.
    Blob,
    /// A directory listing: names, modes and child object ids.
    Tree,
    /// A changeset: the files changed relative to the first parent, plus metadata.
    Changeset,
}

impl ObjectKind {
    /// BLAKE3 key-derivation context. Changing a context changes every id of that kind,
    /// so these strings are part of the on-disk format and must never be edited.
    const fn context(self) -> &'static str {
        match self {
            Self::Blob => "roub 2026-10 blob v1",
            Self::Tree => "roub 2026-10 tree v1",
            Self::Changeset => "roub 2026-10 changeset v1",
        }
    }
}

/// BLAKE3-256 digest identifying an immutable object.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId([u8; ObjectId::LEN]);

impl ObjectId {
    /// Digest length in bytes.
    pub const LEN: usize = 32;

    /// Computes the id of an object of `kind` whose canonical encoding is `content`.
    pub fn compute(kind: ObjectKind, content: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key(kind.context());
        hasher.update(content);
        Self(*hasher.finalize().as_bytes())
    }

    /// Wraps a digest received from storage or the wire.
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// Wraps a digest of unknown length, as received in a protobuf `bytes` field.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, ParseObjectIdError> {
        let digest: [u8; Self::LEN] = bytes
            .try_into()
            .map_err(|_| ParseObjectIdError::Length(bytes.len()))?;
        Ok(Self(digest))
    }

    /// The raw digest.
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({self})")
    }
}

/// Why a string or byte slice is not a valid object id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseObjectIdError {
    #[error("object id must be {expected} bytes, got {0}", expected = ObjectId::LEN)]
    Length(usize),
    #[error("object id must be {} lowercase hex characters", ObjectId::LEN * 2)]
    Hex,
}

impl FromStr for ObjectId {
    type Err = ParseObjectIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s.as_bytes();
        if hex.len() != Self::LEN * 2 {
            return Err(ParseObjectIdError::Hex);
        }
        let mut digest = [0u8; Self::LEN];
        for (byte, pair) in digest.iter_mut().zip(hex.chunks_exact(2)) {
            *byte = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
        }
        Ok(Self(digest))
    }
}

fn hex_value(c: u8) -> Result<u8, ParseObjectIdError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(ParseObjectIdError::Hex),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_content_same_id() {
        let a = ObjectId::compute(ObjectKind::Blob, b"hello");
        let b = ObjectId::compute(ObjectKind::Blob, b"hello");
        assert_eq!(a, b);
    }

    #[test]
    fn kinds_are_domain_separated() {
        let blob = ObjectId::compute(ObjectKind::Blob, b"same bytes");
        let tree = ObjectId::compute(ObjectKind::Tree, b"same bytes");
        let changeset = ObjectId::compute(ObjectKind::Changeset, b"same bytes");
        assert_ne!(blob, tree);
        assert_ne!(blob, changeset);
        assert_ne!(tree, changeset);
    }

    #[test]
    fn hex_round_trip() {
        let id = ObjectId::compute(ObjectKind::Tree, b"src/");
        let text = id.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(text.parse::<ObjectId>(), Ok(id));
    }

    #[test]
    fn rejects_bad_hex() {
        assert_eq!("abc".parse::<ObjectId>(), Err(ParseObjectIdError::Hex));
        let upper = "A".repeat(64);
        assert_eq!(upper.parse::<ObjectId>(), Err(ParseObjectIdError::Hex));
    }

    #[test]
    fn from_slice_checks_length() {
        assert_eq!(
            ObjectId::from_slice(&[0u8; 31]),
            Err(ParseObjectIdError::Length(31))
        );
        assert!(ObjectId::from_slice(&[0u8; 32]).is_ok());
    }
}

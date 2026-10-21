//! Primitives of the canonical object encoding (`docs/design/object-format.md`).
//!
//! Every value has exactly one valid encoding. The decoder rejects anything else: integers that
//! are not minimally encoded, trailing bytes, unsorted or duplicate entries, unknown critical
//! extensions. An object id hashes the exact stored bytes, so this strictness is what makes ids
//! stable: two implementations that accept the same objects compute the same ids.

use crate::object::{ObjectId, ParseObjectIdError};

/// Longest unsigned LEB128 encoding of a `u64`.
const MAX_VARINT_BYTES: usize = 10;

/// Why bytes are not a valid canonical encoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("input ends in the middle of a value")]
    UnexpectedEnd,
    #[error("{0} trailing bytes after the object")]
    TrailingBytes(usize),
    #[error("integer is not minimally encoded or exceeds 64 bits")]
    NonCanonicalInteger,
    #[error("integer {0} does not fit the field")]
    IntegerOutOfRange(u64),
    #[error("list claims {count} entries but only {remaining} bytes remain")]
    CountTooLarge { count: u64, remaining: usize },
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
    #[error("unsupported format version {0}")]
    UnsupportedVersion(u8),
    #[error("boolean flag must be 0 or 1, got {0}")]
    InvalidFlag(u8),
    #[error("unknown {field} tag {tag}")]
    UnknownTag { field: &'static str, tag: u8 },
    #[error("extension tags must be strictly increasing")]
    UnsortedExtensions,
    #[error("unknown critical extension {0}")]
    UnknownCriticalExtension(u64),
    #[error(transparent)]
    ObjectId(#[from] ParseObjectIdError),
    #[error("{0}")]
    Invalid(String),
}

/// Appends canonical encodings to a buffer.
#[derive(Default)]
pub(crate) struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    /// Unsigned LEB128, minimal length.
    pub(crate) fn uvarint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }

    /// ZigZag, then unsigned LEB128.
    pub(crate) fn svarint(&mut self, value: i64) {
        self.uvarint(((value << 1) ^ (value >> 63)) as u64);
    }

    pub(crate) fn len(&mut self, len: usize) {
        self.uvarint(len as u64);
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.len(value.len());
        self.buf.extend_from_slice(value);
    }

    pub(crate) fn str(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    pub(crate) fn id(&mut self, id: &ObjectId) {
        self.buf.extend_from_slice(id.as_bytes());
    }

    pub(crate) fn flag(&mut self, value: bool) {
        self.u8(u8::from(value));
    }
}

/// Reads canonical encodings, rejecting every non-canonical form.
pub(crate) struct Reader<'a> {
    input: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self { input }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.input.len()
    }

    /// Succeeds only if the whole input was consumed.
    pub(crate) fn finish(self) -> Result<(), DecodeError> {
        match self.input.len() {
            0 => Ok(()),
            n => Err(DecodeError::TrailingBytes(n)),
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.input.len() < n {
            return Err(DecodeError::UnexpectedEnd);
        }
        let (head, tail) = self.input.split_at(n);
        self.input = tail;
        Ok(head)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn uvarint(&mut self) -> Result<u64, DecodeError> {
        let mut value: u64 = 0;
        for index in 0..MAX_VARINT_BYTES {
            let byte = self.u8()?;
            let payload = u64::from(byte & 0x7f);
            // The tenth byte may only carry the top bit of a u64.
            if index == MAX_VARINT_BYTES - 1 && payload > 1 {
                return Err(DecodeError::NonCanonicalInteger);
            }
            value |= payload << (7 * index);
            if byte & 0x80 == 0 {
                // A zero final byte after the first one means a shorter encoding existed.
                if index > 0 && byte == 0 {
                    return Err(DecodeError::NonCanonicalInteger);
                }
                return Ok(value);
            }
        }
        Err(DecodeError::NonCanonicalInteger)
    }

    pub(crate) fn svarint(&mut self) -> Result<i64, DecodeError> {
        let raw = self.uvarint()?;
        Ok(((raw >> 1) as i64) ^ -((raw & 1) as i64))
    }

    /// A length or count that must fit in the remaining input, so a forged count can never make
    /// the decoder allocate more than the input size.
    pub(crate) fn count(&mut self) -> Result<usize, DecodeError> {
        let count = self.uvarint()?;
        let remaining = self.remaining();
        if count > remaining as u64 {
            return Err(DecodeError::CountTooLarge { count, remaining });
        }
        Ok(count as usize)
    }

    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let len = self.count()?;
        self.take(len)
    }

    pub(crate) fn str(&mut self) -> Result<&'a str, DecodeError> {
        std::str::from_utf8(self.bytes()?).map_err(|_| DecodeError::InvalidUtf8)
    }

    pub(crate) fn id(&mut self) -> Result<ObjectId, DecodeError> {
        Ok(ObjectId::from_slice(self.take(ObjectId::LEN)?)?)
    }

    pub(crate) fn flag(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(DecodeError::InvalidFlag(other)),
        }
    }
}

/// An extension field: forward-compatible data a newer writer added to an object.
///
/// Odd tags are critical: a decoder that does not know the tag must reject the object, because
/// ignoring it would change its meaning. Even tags may be ignored, and are kept byte for byte so
/// the object re-encodes to the same bytes and keeps its id. Format version 1 defines no tags.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Extension {
    pub tag: u64,
    pub value: Vec<u8>,
}

impl Extension {
    pub fn is_critical(&self) -> bool {
        self.tag % 2 == 1
    }
}

pub(crate) fn write_extensions(w: &mut Writer, extensions: &[Extension]) {
    w.len(extensions.len());
    for extension in extensions {
        w.uvarint(extension.tag);
        w.bytes(&extension.value);
    }
}

pub(crate) fn read_extensions(r: &mut Reader<'_>) -> Result<Vec<Extension>, DecodeError> {
    let count = r.count()?;
    let mut extensions: Vec<Extension> = Vec::with_capacity(count);
    for _ in 0..count {
        let tag = r.uvarint()?;
        if extensions.last().is_some_and(|last| last.tag >= tag) {
            return Err(DecodeError::UnsortedExtensions);
        }
        let value = r.bytes()?.to_vec();
        let extension = Extension { tag, value };
        if extension.is_critical() {
            return Err(DecodeError::UnknownCriticalExtension(tag));
        }
        extensions.push(extension);
    }
    Ok(extensions)
}

/// Checks extensions an encoder was given: sorted, unique, and no critical tag this version
/// does not know.
pub(crate) fn check_extensions(extensions: &[Extension]) -> Result<(), DecodeError> {
    for pair in extensions.windows(2) {
        if pair[0].tag >= pair[1].tag {
            return Err(DecodeError::UnsortedExtensions);
        }
    }
    match extensions.iter().find(|e| e.is_critical()) {
        Some(critical) => Err(DecodeError::UnknownCriticalExtension(critical.tag)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uvarint_bytes(value: u64) -> Vec<u8> {
        let mut w = Writer::new();
        w.uvarint(value);
        w.into_bytes()
    }

    #[test]
    fn uvarint_known_encodings() {
        assert_eq!(uvarint_bytes(0), [0x00]);
        assert_eq!(uvarint_bytes(127), [0x7f]);
        assert_eq!(uvarint_bytes(128), [0x80, 0x01]);
        assert_eq!(uvarint_bytes(300), [0xac, 0x02]);
        assert_eq!(
            uvarint_bytes(u64::MAX),
            [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]
        );
    }

    #[test]
    fn uvarint_round_trip_edges() {
        for value in [0, 1, 127, 128, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let bytes = uvarint_bytes(value);
            let mut r = Reader::new(&bytes);
            assert_eq!(r.uvarint(), Ok(value));
            assert_eq!(r.finish(), Ok(()));
        }
    }

    #[test]
    fn uvarint_rejects_non_minimal_and_overflow() {
        for bad in [
            &[0x80, 0x00][..],
            &[0xff, 0x00],
            &[0x81, 0x80, 0x00],
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02],
            &[
                0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
            ],
        ] {
            assert_eq!(
                Reader::new(bad).uvarint(),
                Err(DecodeError::NonCanonicalInteger),
                "{bad:02x?}"
            );
        }
        assert_eq!(
            Reader::new(&[0x80]).uvarint(),
            Err(DecodeError::UnexpectedEnd)
        );
    }

    #[test]
    fn svarint_zigzag() {
        for (value, encoded) in [
            (0i64, 0u64),
            (-1, 1),
            (1, 2),
            (-2, 3),
            (i64::MAX, u64::MAX - 1),
        ] {
            let mut w = Writer::new();
            w.svarint(value);
            assert_eq!(w.into_bytes(), uvarint_bytes(encoded));
        }
        for value in [i64::MIN, -3_600, 0, 3_600, i64::MAX] {
            let mut w = Writer::new();
            w.svarint(value);
            let bytes = w.into_bytes();
            assert_eq!(Reader::new(&bytes).svarint(), Ok(value));
        }
    }

    #[test]
    fn count_cannot_exceed_input() {
        let mut r = Reader::new(&[0x05, 0x01, 0x02]);
        assert_eq!(
            r.count(),
            Err(DecodeError::CountTooLarge {
                count: 5,
                remaining: 2
            })
        );
    }

    #[test]
    fn strings_and_flags_are_strict() {
        assert_eq!(
            Reader::new(&[0x01, 0xff]).str(),
            Err(DecodeError::InvalidUtf8)
        );
        assert_eq!(
            Reader::new(&[0x02]).flag(),
            Err(DecodeError::InvalidFlag(2))
        );
        assert_eq!(
            Reader::new(&[0x00, 0x00]).finish(),
            Err(DecodeError::TrailingBytes(2))
        );
    }

    #[test]
    fn extensions_keep_ignorable_and_reject_critical() {
        let mut w = Writer::new();
        write_extensions(
            &mut w,
            &[
                Extension {
                    tag: 2,
                    value: b"a".to_vec(),
                },
                Extension {
                    tag: 4,
                    value: vec![],
                },
            ],
        );
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let read = read_extensions(&mut r).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].value, b"a");

        // Same tag twice, then a critical tag.
        assert_eq!(
            read_extensions(&mut Reader::new(&[0x02, 0x02, 0x00, 0x02, 0x00])),
            Err(DecodeError::UnsortedExtensions)
        );
        assert_eq!(
            read_extensions(&mut Reader::new(&[0x01, 0x03, 0x00])),
            Err(DecodeError::UnknownCriticalExtension(3))
        );
    }
}

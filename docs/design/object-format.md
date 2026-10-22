# Object format, version 1

This is the canonical encoding of roub's immutable objects: blobs, trees and changesets. Object
ids are hashes of these bytes, so every tier and any other implementation must produce and accept
exactly the same bytes. `roub-core` implements this document (`encoding`, `tree`, `changeset`,
`derive`); the golden tests in those modules and the vectors below pin it.

## Object ids

An id is the 32-byte BLAKE3 hash of the object's canonical, uncompressed bytes, computed in
key-derivation mode with a context string per object kind and format version:

| Kind | Context string |
|---|---|
| Blob | `roub 2026-10 blob v1` |
| Tree, format 1 | `roub 2026-10 tree v1` |
| Changeset, format 1 | `roub 2026-10 changeset v1` |

The context strings are part of the format and never change. A new format version gets a new
context string, so ids of different versions can never collide. Compression is a storage concern:
it never changes an id.

Text form: 64 lowercase hexadecimal characters.

## Primitives

| Name | Encoding |
|---|---|
| `u8` | one byte |
| `uvarint` | unsigned LEB128, at most 10 bytes. **Must be minimal**: no trailing zero group, and the tenth byte is `0x00` or `0x01`. |
| `svarint` | ZigZag (`(n << 1) ^ (n >> 63)`), then `uvarint` |
| `bool` | `u8`, exactly `0x00` or `0x01` |
| `bytes` | `uvarint` length, then the bytes |
| `string` | `bytes` holding valid UTF-8 |
| `id` | 32 raw bytes |
| `list<T>` | `uvarint` count, then the items. The count must not exceed the bytes that remain, so no input can make a decoder allocate more than its own size. |
| `option<T>` | `bool`, then `T` if it is `0x01` |

A decoder rejects trailing bytes after an object.

## Extensions

Trees and changesets end with `list<Extension>`, where `Extension = uvarint tag, bytes value`:

- Tags are strictly increasing.
- **Odd tags are critical.** A decoder that does not know the tag must reject the object, because
  ignoring it would change its meaning.
- **Even tags are ignorable.** A decoder keeps them byte for byte, so the object re-encodes to the
  same bytes and keeps its id.

Format version 1 defines no tags: every odd tag is rejected, and even tags are carried unchanged.
New optional fields are added as extensions. Anything else needs a new format version.

## Blob

The file's bytes, unchanged. There is no header, so a blob's id depends only on its content and
the blob context string. Large files will be split into chunks for storage and transfer. That is
a storage format below the blob id: the id stays the hash of the whole content.

## Tree

One directory.

```
Tree    = u8 version (1)
          list<Entry>
          list<Extension>
Entry   = string name
          u8 mode
          id
          uvarint size
```

| Mode | Value | `id` points at | `size` |
|---|---|---|---|
| Regular file | 1 | blob | file size in bytes |
| Executable file | 2 | blob | file size in bytes |
| Symbolic link | 3 | blob holding the target | target length in bytes |
| Directory | 4 | tree | must be 0 |

Rules:
- Every name is a valid path component: the rules of `roub-core::path`, which also exclude `/`.
- Entries are strictly increasing by the bytes of their names, so names are unique.
- Sizes are in the tree so a checkout or a virtual file system (ProjFS needs them up front) can list a
  directory without fetching any blob.
- Only a root tree may be empty, as the root of an empty repository. Derivation never produces an
  empty subdirectory: a directory disappears with its last file.
- Symlinks and the executable bit are always stored, whatever the platform. How a Windows checkout
  represents them is an open decision; the stored tree does not depend on it.

## Changeset

One entry of the write-ahead log: the files that changed relative to the first parent.

```
Changeset  = u8 version (1)
             list<id> parents
             Signature author
             option<Signature> committer     absent: the committer is the author
             string message
             list<Change> changes
             list<Extension>
Signature  = string name
             string email
             svarint time                    seconds since the Unix epoch, UTC
             svarint tz_offset               minutes east of UTC, |offset| <= 1440
Change     = string path
             u8 op
             op 1, Put:    u8 mode (1, 2 or 3), id blob, uvarint size, option<CopyFrom>
             op 2, Delete: nothing
CopyFrom   = uvarint parent                  index into parents
             string path
```

Rules:
- **Parents.** At most 16, no duplicates, in order. The first parent is the base of `changes`.
  A changeset with no parents starts a history.
- **Signatures.** Names and emails contain no control characters.
- **Changes.**
  - Each path is a valid `RepoPath`. Changes are strictly increasing in tree-walk order: compare
    paths component by component, bytewise within a component. So `a/b` sorts before `a.b`, and
    the list holds one change per path.
  - `Put` adds or modifies a file; a mode change is a `Put` with the new mode.
  - `Delete` removes a file. Directories are never named.
- **File and directory transitions.**
  - A file becomes a directory with a `Delete` of the file plus `Put`s inside it.
  - A directory becomes a file with a single `Put` of the path, which replaces the whole directory.
  - A `Put` at a path followed by any change inside that path is invalid.
- **Copies and renames.** `copy_from` names the source path and the parent it came from. A rename
  is a `Put` with `copy_from` plus a `Delete` of the source. It is history metadata only and does
  not change the derived tree.
- **Merges.** A merge lists every path whose content differs from the first parent. Its tree is
  exactly the first parent's tree plus these changes.

A changeset does **not** contain its root tree. Trees are derived, so the derivation can be fixed
or extended without changing a single changeset id.

## Deriving trees

`derive::derive_trees` computes a changeset's root tree from the first parent's root tree:

1. Only directories on a changed path are read and rewritten; every other subtree is reused by id.
   The cost is proportional to the change, not to the repository.
2. Each changed path is checked against the parent tree:
   - `Delete` of a path that is not a file in the parent is rejected.
   - A `Put` identical to the existing entry (same mode, blob and size) is rejected, so a delta is
     never padded with no-ops.
   - A `Put` inside a path that is a file in the parent is rejected, unless the same changeset
     deletes that file.
3. A new or changed name that collides with a sibling on a case-insensitive file system is rejected.
   Only touched names are compared with their siblings, so this check also costs only as much as the
   change. Case folding approximates the NTFS upcase table with Unicode lowercase mapping; the exact
   table is an open decision.
4. The server stores the root tree id with the changeset row and writes the new trees in the same
   transaction.

## Golden vectors

These values were checked against an independent implementation (the reference BLAKE3 in Python),
and the unit tests assert the same values.

| Object | Encoding (hex) | Id |
|---|---|---|
| Blob `hello` | `68656c6c6f` | `5057547d9f441e76078397fd7e01e7fc2d916164d0dae77f670ae80bdf135c6a` |
| Empty tree | `010000` | `ed28216ca8aa0beefc5b9074e4841bc8cd551fd5a5a24fc9f05c88bc86724286` |
| Tree with file `a` = `hello` | `010101 61 01` + blob id + `05 00` | `ae0bf66b4d1833721f77a88cff499e19879014d36faaf3fc8b9c6ad11ea40ee3` |
| Root changeset putting `a` = `hello` (below) | see `changeset::tests::root_changeset_golden` | `2d219349af1bfe55bfd16510324b54309d9eff291a69dcbbf14fb1dd8d525475` |

The changeset has:
- author `Ada Lovelace <ada@example.com>`, time 1792000000, offset +120;
- no committer;
- message `"Add a file\n"`;
- one regular-file `Put` of `a` with the `hello` blob, size 5;
- no copy source and no extensions.

## Testing

- Unit tests next to each module cover every rule above, plus the golden vectors.
- `crates/roub-core/tests/encoding_props.rs` checks with property tests that:
  - every valid object round-trips to the same bytes and id;
  - whatever a decoder accepts from random or corrupted bytes re-encodes to exactly those bytes.
- `fuzz/` holds libFuzzer targets for both decoders, with the same canonicality assertion. CI runs
  each for 60 seconds on every pull request.

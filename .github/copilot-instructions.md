# Review instructions for roub

roub is a server-centric version control system in Rust (edition 2024) for Linux, Windows and
Azure. It serves other people's source code across trust boundaries, so review for correctness
and security first, style last. `CLAUDE.md` holds the full rules; flag any change that breaks one.

## Block the change when

- **Object format:** a change alters the canonical encoding or a BLAKE3 derive-key context string
  in `roub-core::object` without a new format version. Ids are part of the on-disk format.
- **Decoders** accept more than one encoding of the same value: trailing bytes, non-minimal
  integers, unsorted or duplicate entries, unknown critical fields. Every decoder must be strict.
- **Untrusted input** (wire, stored objects, git import) can panic, allocate without a bound, or
  decompress without a size limit. Look for `unwrap`, `expect`, indexing and `as` casts on such data.
- **Authorization:** an object, tree listing, blame or other derived data can reach a caller who
  may not read that path, through any tier (`roubd`, `roub-cache`, `roub-server`). An unreadable
  object must look exactly like a missing one. A failed or unimplemented auth check must deny.
- **Refs** move without compare-and-swap, a ref version can decrease or restart, or a ref answer
  is cached at any tier.
- **Commit path** work grows with the size of the repository instead of the size of the change.
- **Paths** skip `RepoPath` validation or case-collision checks, or a checkout can write outside
  the working copy (`..`, symlinked parents, absolute paths, Windows device names).
- **`unsafe`** appears outside a dedicated FFI module, or an `unsafe` block lacks a safety comment.
- **Wire contract:** a field number in `roub.proto` is reused or renumbered.
- **Workflows:** an action is not pinned to a full commit SHA, a job gets more permissions than it
  needs, `persist-credentials` is not `false`, or `${{ }}` expands untrusted input inside `run:`.
- **Dependencies:** a new crate is not declared in the root `Cargo.toml` workspace, or its
  licence is not in `deny.toml`.

## Also check

- New logic in `roub-core` has unit tests next to the code; format changes come with golden
  vectors.
- Libraries return typed errors (`thiserror`); only binaries use `anyhow`.
- Platform-specific code sits behind a trait with one implementation per OS, selected with `cfg`.
- A change to tiers, boundaries or flows updates `docs/architecture/roub-architecture.d2` and the
  rendered SVG in the same pull request; a decided design question updates
  `docs/design/decisions.md`.
- Code, comments, identifiers and log messages are in English.

## Do not comment on

- Formatting that `cargo fmt` decides, or lints that `cargo clippy -D warnings` already enforces.
- Generated code from `tonic-prost-build`.

# Design decisions

roub follows the design Anthropic described in
[Redesigning source control at Anthropic](https://www.anthropic.com/research/redesigning-source-control)
(Scion). The article leaves most details open. This file records what roub decided and what it
still has to decide.

## Decided

| Topic | Decision | Why |
|---|---|---|
| Language | Rust, one codebase for every tier | Small static binaries for containers, no GC in the cache tier, native FUSE (Linux) and ProjFS (Windows), and the compiler rejects whole classes of bugs before review. |
| Platforms | Linux (Debian) and Windows for every component; Azure and on-prem | Same binaries everywhere: Debian containers or `.deb` with systemd, a Windows service, Azure Container Apps (Linux only) or AKS. |
| Metadata store | PostgreSQL on-prem and in Azure | One code path. Ref moves are a compare-and-swap `UPDATE … WHERE` on one row. |
| Blob store | A local or mounted directory now (`blob.rs`: atomic writes, content verified on every read); Azure Blob Storage and S3 later, probably through the `object_store` crate | The directory needs no dependency; `object_store` with only its filesystem backend would add about 85 crates. Azurite stays in `compose.yaml` for the Azure backend. |
| Database schema | `crates/roub-server/migrations/`, applied at start-up under an advisory lock. Every table is keyed by repository. Trees and changesets live in PostgreSQL (`bytea`), blob contents in the blob store with an index row (`blobs`) written only after the content is durable. Triggers reject `UPDATE`, `DELETE` and `TRUNCATE` on objects and on `ref_log`, the deletion of a ref row, and any ref update that does not raise the version by exactly one | Trees and changesets are small and must be written in the same transaction; blobs can be large. The triggers make the immutability and ref-version rules hold even for a bug or a stray SQL statement, not only for the API. |
| Database access | `tokio-postgres` with `deadpool-postgres` | Small, asynchronous, no compile-time database needed; the queries are few and covered by tests against a real PostgreSQL. |
| Wire contract | gRPC, `crates/roub-proto/proto/roub/v1/roub.proto` | Same RPCs at every tier, streaming for object fetch and blob upload. |
| Object ids | BLAKE3-256 of the canonical, uncompressed encoding, with a key-derivation context per object kind | Fast, and ids stay stable if compression changes. A blob and a tree with the same bytes never share an id. |
| Paths | `/`-separated, relative, NFC; no Windows-forbidden characters, reserved device names, trailing dots or spaces; components up to 255 bytes; `.roub` reserved | Every path in a repository checks out on Linux and Windows (`roub-core::path`). |
| Case collisions | Rejected at commit time | `a.txt` and `A.txt` cannot both exist on NTFS. |
| Line endings | Stored as committed, never converted | Conversion, if any, belongs to checkout. |
| Refs | Only `main` and release branches are expected to be refs | Global mutable state is the scaling problem the design avoids. |
| Diagrams | D2 for architecture (rendered SVG checked in CI), Mermaid for sequence diagrams | Text sources diff well and are compact context for agents (the draw.io XML of the same diagram was about five times larger and mostly coordinates). D2 renders deterministically from a single binary; Mermaid renders natively on GitHub. |
| Supply chain | Actions pinned to commit SHAs, 14-day cooldown on new versions, least-privilege workflow tokens, Codecov via OIDC, CodeQL, zizmor, OpenSSF Scorecard, cargo-deny, private vulnerability reporting | roub hosts other people's source code; a compromised build or CI step would compromise every repository it serves. |
| Canonical encoding | Own binary format, specified in [object-format.md](object-format.md): a format-version byte, minimal LEB128 integers, sorted lists, critical (odd) and ignorable (even) extension tags; strict decoders; golden vectors, property tests and fuzzing | Ids hash the exact stored bytes, so the encoding must be unique and checkable by any implementation. Every reviewed implementation that got this right (Mononoke, CarterT27/scion, OpenCSGs/scion, bnsd55/openscion) used one strict encoding with pinned vectors. |
| Trees | Derived from the first parent's tree and the changes, synchronously at commit, stored as content-addressed objects; not part of the changeset id. Entries carry mode, id and size | As in Mononoke's Bonsai model: the derivation can evolve without changing changeset ids, and clients still fetch trees by id. Sizes let ProjFS and FUSE list directories without fetching blobs. |
| Changeset contents | Ordered parents (at most 16), author, optional committer, message, `Put`/`Delete` per path relative to the first parent, optional `copy_from` (parent, path) for copies and renames, extensions | `Put`/`Delete` is what every implementation converged on; `copy_from` keeps rename history exact instead of guessing it later. A merge's tree is the first parent's tree plus its changes. |
| Blob ids | BLAKE3 of the whole file content; chunking, when it comes, is a storage format below the id | Large-file chunking can be added without changing any id. |
| Symlinks and executable bit | Always stored in trees as their own modes | The stored tree is the same on every platform; only checkout differs. |
| Who computes ids | The server. `PutBlobs` hashes each blob and rejects a wrong id; `CreateChangeset` takes the changeset's fields, builds the canonical encoding and returns the id | A client can never store content under an id that does not match it. The same request always yields the same id, so `CreateChangeset` is idempotent and safe to retry. |
| Commit preconditions | Every parent and every blob must already be stored, each put's size must match its blob, and a copy source must be a file in the named parent | A stored changeset always derives to complete trees, and its history metadata points at real files. |
| Ref versions | Every create, move or delete of a ref increments its version; a deleted ref keeps its row, so the version never restarts. Every move is appended to an immutable ref log | Clients can detect any change between two reads (no ABA), and the verifiable ref log below can be built from the existing rows. |
| Repository and ref names | `/`-separated segments of ASCII letters, digits, `-`, `_` and `.`, no segment starting with a dot, at most 255 bytes (`roub-core::name`) | Safe without escaping in URLs, logs, metrics labels and database columns. |

## Open: decide before the MVP

These shape the on-disk and in-database formats, so changing them later needs a migration.

- **Large files:** chunking, deduplication and the compression codec (zstd level, dictionaries).
  The blob id is already fixed as the hash of the whole content.
- **Database schema, remaining parts:** WAL order (a sequence is not commit order), derived
  data beyond the root tree, ACLs and the git id map.
- **TLS to PostgreSQL**, required by Azure Database for PostgreSQL. Needs a TLS crate
  (`rustls` brings the ISC licence into `deny.toml`) and certificate configuration.
- **Derived data framework:** how a type is versioned, backfilled and invalidated when its
  definition changes.
- **Authorization through caches.** Per-directory ACLs are checked at roub-server. A cache that
  answers any object by hash could leak restricted content to a caller who knows the hash.
  Options: signed fetch grants issued by the server, or token checks at every tier.
- **Exact case folding.** `path::fold_case` uses Unicode lowercase mapping, which approximates
  the NTFS upcase table. Decide between Unicode simple case folding and the NTFS table.
- **Symlinks and the executable bit on a Windows checkout** (both are stored in trees): symlinks
  need Developer Mode or a privilege; without it, a checkout could write a file holding the
  target and warn.
- **Garbage collection** of unreferenced blobs and abandoned changesets.

## Open: later

- Daemon on-disk store format, eviction and snapshot format.
- Cache sizing, sharding and admission control.
- Review and landing (Scion's Antique): change requests, approval rules, batch landing,
  post-merge tests, automated bisect and revert.
- Code search beyond path search.
- Export back to git, or a read-only git facade.
- **Verifiable ref log (transparency log), after the MVP.** A Merkle log of every ref move
  (`repo, ref, version, old, new, changeset, principal, time`) in the C2SP `tlog-checkpoint`,
  `tlog-cosignature` and `tlog-tiles` formats, as in Certificate Transparency and the Go checksum
  database. roubd verifies an inclusion proof for every `ResolveRef` answer and consistency with
  the last checkpoint it saw, so a compromised server cannot rewind `main` or show different
  clients different histories; optional external witnesses cosign checkpoints. A batching
  sequencer assigns log positions, so the ref compare-and-swap never waits on the log. The log
  holds hashes and metadata only, never file contents or commit messages, so redaction does not
  break it. Not a blockchain: roub has one authoritative operator, and consensus on every ref
  move would undo lock-free commits.
  **Constraint for the MVP:** `ref_log` rows are immutable and a ref's version never decreases or
  restarts, including after the ref is deleted, so the log can be built from `ref_log` later
  without a migration.

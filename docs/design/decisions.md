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
| Metadata store | PostgreSQL on-prem and in Azure | One code path. Ref moves are `UPDATE … WHERE` on one row; the changeset WAL takes its order from a sequence. |
| Blob store | `object_store` crate behind a roub trait | Local filesystem, S3-compatible stores and Azure Blob Storage from one API, Azurite for local development. |
| Wire contract | gRPC, `crates/roub-proto/proto/roub/v1/roub.proto` | Same RPCs at every tier, streaming for object fetch and blob upload. |
| Object ids | BLAKE3-256 of the canonical, uncompressed encoding, with a key-derivation context per object kind | Fast, and ids stay stable if compression changes. A blob and a tree with the same bytes never share an id. |
| Paths | `/`-separated, relative, NFC; no Windows-forbidden characters, reserved device names, trailing dots or spaces; components up to 255 bytes; `.roub` reserved | Every path in a repository checks out on Linux and Windows (`roub-core::path`). |
| Case collisions | Rejected at commit time | `a.txt` and `A.txt` cannot both exist on NTFS. |
| Line endings | Stored as committed, never converted | Conversion, if any, belongs to checkout. |
| Refs | Only `main` and release branches are expected to be refs | Global mutable state is the scaling problem the design avoids. |
| Diagrams | D2 for architecture (rendered SVG checked in CI), Mermaid for sequence diagrams | Text sources diff well and are compact context for agents (the draw.io XML of the same diagram was about five times larger and mostly coordinates). D2 renders deterministically from a single binary; Mermaid renders natively on GitHub. |
| Supply chain | Actions pinned to commit SHAs, 14-day cooldown on new versions, least-privilege workflow tokens, Codecov via OIDC, CodeQL, zizmor, OpenSSF Scorecard, cargo-deny, private vulnerability reporting | roub hosts other people's source code; a compromised build or CI step would compromise every repository it serves. |

## Open: decide before the MVP

These shape the on-disk and in-database formats, so changing them later needs a migration.

- **Canonical encodings** of trees and changesets (field order, integer encoding, how a
  changeset names its parents and per-file operations: delete, rename, mode change, copy info).
- **Trees as derived data or as part of the changeset.** Scion's changesets list only changed
  files; trees are probably derived (as in Mononoke's Bonsai model).
- **Large files:** chunking, deduplication and the compression codec (zstd level, dictionaries).
- **Database schema** for the WAL, refs, derived data, ACLs and the git id map.
- **Derived data framework:** how a type is versioned, backfilled and invalidated when its
  definition changes.
- **Authorization through caches.** Per-directory ACLs are checked at roub-server. A cache that
  answers any object by hash could leak restricted content to a caller who knows the hash.
  Options: signed fetch grants issued by the server, or token checks at every tier.
- **Exact case folding.** `find_case_collisions` uses Unicode lowercase mapping, which
  approximates the NTFS upcase table. Decide whether to embed the NTFS table.
- **Symlinks and the executable bit on Windows:** store both in trees; decide how a Windows
  checkout represents them (symlinks need Developer Mode or a privilege).
- **Garbage collection** of unreferenced blobs and abandoned changesets.

## Open: later

- Daemon on-disk store format, eviction and snapshot format.
- Cache sizing, sharding and admission control.
- Review and landing (Scion's Antique): change requests, approval rules, batch landing,
  post-merge tests, automated bisect and revert.
- Code search beyond path search.
- Export back to git, or a read-only git facade.
